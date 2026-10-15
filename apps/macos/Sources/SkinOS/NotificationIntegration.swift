import AppKit
import Foundation

@MainActor
extension AppModel {
    var unreadNoticeCount: Int { noticeItems.filter { !$0.read }.count }

    var mainContentVisible: Bool {
        applicationIsActive && applicationWindowVisible && !isShowingLogin && gameForSettings == nil && gameForLog == nil
    }
    func updateWindowVisibility() {
        let app = NSApplication.shared
        applicationWindowVisible = app.keyWindow.map { $0.isVisible && !$0.isMiniaturized && $0 == app.mainWindow } ?? false
        markVisibleChatRead(); markVisibleNoticesRead()
    }
    func updateApplicationActivity(_ active: Bool) {
        applicationIsActive = active
        updateWindowVisibility()
        if active {
            markVisibleChatRead(); markVisibleNoticesRead()
            Task { await notifications.refreshPermission() }
        }
    }
    func isViewingChat(_ steamID: String) -> Bool {
        mainContentVisible && section == .friends && selectedFriendID == steamID
    }
    func markVisibleChatRead() {
        guard let id = selectedFriendID, isViewingChat(id) else { return }
        unreadCounts[id] = 0
    }
    func markVisibleNoticesRead() {
        guard mainContentVisible, section == .notifications else { return }
        for index in noticeItems.indices {
            noticeItems[index].read = true
            locallyReadNoticeIDs.insert(noticeItems[index].id)
        }
    }
    func canPresentNotification(owner: String, target: NotificationTarget) -> Bool {
        guard account?.id == owner else { return false }
        switch target {
        case .chat(let id): return !isViewingChat(id)
        case .steamInbox: return !mainContentVisible || section != .notifications
        case .download: return !mainContentVisible || section != .downloads
        case .game(let id): return !mainContentVisible || section != .library || selectedGameID != id
        default: return true
        }
    }

    func openNotification(owner: String, target: NotificationTarget) {
        guard account?.id == owner else {
            notificationRouteWaiting = (owner, target)
            if account != nil { errorMessage = "Это уведомление другого Steam-аккаунта. Переключитесь на нужный аккаунт, чтобы открыть его." }
            return
        }
        switch target {
        case .chat(let id):
            guard let friend = friends.first(where: { $0.id == id }) else {
                notificationRouteWaiting = (owner, target)
                if friendsLoaded { section = .notifications }
                return
            }
            section = .friends; selectFriend(friend)
        case .steamInbox: section = .notifications
        case .download(let appID): selectedGameID = appID; section = .downloads
        case .game(let appID): selectedGameID = appID; section = .library
        case .settings: section = .settings
        }
        notificationRouteWaiting = nil
        activateApplication()
    }
    func resumeNotificationRouteIfPossible() {
        guard let route = notificationRouteWaiting, route.owner == account?.id else { return }
        openNotification(owner: route.owner, target: route.target)
    }
    func openNotice(_ notice: ClientNotice) {
        guard notice.owner == account?.id else { return }
        locallyReadNoticeIDs.insert(notice.id)
        if let index = noticeItems.firstIndex(where: { $0.id == notice.id }) { noticeItems[index].read = true }
        if notice.target == .steamInbox {
            let url = notice.steamURL ?? URL(string: "https://steamcommunity.com/my/notifications/")!
            if url.host == "store.steampowered.com" { storeURL = url; section = .store }
            else { profileURL = url; section = .profile }
        } else { openNotification(owner: notice.owner, target: notice.target) }
    }

    func refreshNotices() {
        guard account != nil, !noticesLoading else { return }
        noticesLoading = true; noticesError = nil
        if let id = send("get_notifications") { notificationRequests.insert(id) }
        else { noticesLoading = false }
    }

    func receiveNotificationEvent(_ event: String, payload: [String: Any], requestID: String?) -> Bool {
        if event == "steam_notifications_status" {
            guard payload["steamid"] as? String == account?.id, account != nil else { return true }
            switch payload["state"] as? String {
            case "ready": noticesError = nil
            case "unavailable", "degraded":
                noticesError = payload["message"] as? String ?? payload["error"] as? String ?? "Уведомления Steam временно недоступны."
            default: break
            }
            return true
        }
        if event == "error" {
            let notificationError = ["notifications_failed", "notifications_storage_failed", "notifications_busy"]
                .contains(payload["code"] as? String ?? "")
            let matchingRequest = requestID.map { notificationRequests.remove($0) != nil } ?? false
            if notificationError || matchingRequest {
                noticesLoading = false
                noticesError = payload["message"] as? String ?? "Не удалось загрузить уведомления Steam."
                return true
            }
        }
        guard event == "steam_notifications" || event == "steam_notification" else { return false }
        guard let owner = payload["steamid"] as? String, owner == account?.id else { return true }
        if event == "steam_notifications" {
            // The backend snapshot is authoritative, including withdrawn requests.
            // Local game/download events have a separate lifetime in this feed.
            noticeItems.removeAll { $0.kind == .message || $0.kind == .steam }
            for row in payload["notifications"] as? [[String: Any]] ?? [] {
                if let notice = makeSteamNotice(row, owner: owner) {
                    mergeNotice(notice)
                }
            }
            noticesLoading = false; noticesError = nil
            if let requestID { notificationRequests.remove(requestID) }
        } else if let row = payload["notification"] as? [String: Any], let notice = makeSteamNotice(row, owner: owner) {
            mergeNotice(notice)
            let fresh = notifications.record(notice)
            let alreadyInChat: Bool
            if notice.kind == .message, case .chat(let friendID) = notice.target {
                alreadyInChat = chats[friendID]?.contains(where: { $0.id == notice.id }) == true
            } else { alreadyInChat = false }
            // A delayed CM push may follow history containing the same canonical ID.
            // Fresh live pushes precede chat_message in the backend event stream.
            if fresh, payload["notify"] as? Bool == true, !notice.read, !alreadyInChat {
                notifications.post(notice, alreadyRecorded: true)
            }
        }
        markVisibleNoticesRead()
        return true
    }

    func makeSteamNotice(_ row: [String: Any], owner: String) -> ClientNotice? {
        guard let id = row["id"] as? String, !id.isEmpty, let title = row["title"] as? String else { return nil }
        let category = row["category"] as? String ?? "steam"
        let message = ["message", "chat", "direct_message"].contains(category)
        let target: NotificationTarget
        if message, let friendID = row["friend_steamid"] as? String, !friendID.isEmpty, friendID.allSatisfy(\.isNumber) { target = .chat(friendID) }
        else { target = .steamInbox }
        return ClientNotice(id: id, owner: owner, kind: message ? .message : .steam, title: title,
            body: row["body"] as? String ?? "", target: target,
            timestamp: Date(timeIntervalSince1970: (row["timestamp"] as? NSNumber)?.doubleValue ?? Date().timeIntervalSince1970),
            read: row["read"] as? Bool == true || locallyReadNoticeIDs.contains(id),
            steamURL: (row["url"] as? String).flatMap(URL.init(string:)).flatMap { SteamWebPolicy.allows($0) ? $0 : nil })
    }
    func mergeNotice(_ notice: ClientNotice) {
        if let index = noticeItems.firstIndex(where: { $0.id == notice.id && $0.owner == notice.owner }) { noticeItems[index] = notice }
        else { noticeItems.append(notice) }
        noticeItems.sort { $0.timestamp > $1.timestamp }
        if noticeItems.count > 200 { noticeItems.removeLast(noticeItems.count - 200) }
    }

    func notifyLaunchFailure(_ appID: Int) {
        guard let account, let attempt = launchNotificationIDs[appID] else { return }
        let name = games.first { $0.id == appID }?.name ?? "Игра \(appID)"
        let notice = ClientNotice(id: "launch-\(attempt)-failed", owner: account.id, kind: .game,
            title: "\(name) — не удалось запустить", body: "Откройте журнал запуска в карточке игры, чтобы посмотреть подробности.",
            target: .game(appID), timestamp: Date())
        mergeNotice(notice); notifications.post(notice)
    }

    func notifyDownloadTransitions(previous: [Int: GameDownload], items: [GameDownload], revision: Int64) {
        guard downloadQueueReady, let account else { return }
        for item in items where item.complete || item.failed {
            guard let old = previous[item.id], old.isActive, old.phase != item.phase else { continue }
            let name = games.first { $0.id == item.id }?.name ?? "Игра \(item.id)"
            let notice = ClientNotice(id: "download-\(item.id)-\(revision)", owner: account.id, kind: .download,
                title: item.complete ? "\(name) — \(item.isUpdate ? "обновление готово" : "установка завершена")" : "\(name) — загрузка прервана",
                body: item.complete ? "Можно запускать игру из библиотеки SkinOS." : "Откройте загрузки, чтобы посмотреть причину и повторить попытку.",
                target: .download(item.id), timestamp: Date())
            mergeNotice(notice); notifications.post(notice)
        }
    }
}
