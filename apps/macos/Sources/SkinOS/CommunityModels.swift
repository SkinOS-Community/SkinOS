import AppKit
import Foundation

enum Presence: String, CaseIterable, Identifiable {
    case invisible, online, away, busy, offline
    var id: Self { self }
    var title: String {
        switch self {
        case .invisible: return "Невидимка"
        case .online: return "В сети"
        case .away: return "Нет на месте"
        case .busy: return "Не беспокоить"
        case .offline: return "Не в сети"
        }
    }
}

struct SteamFriend: Identifiable, Equatable {
    let id: String
    var name: String
    var avatarURL: URL?
    var state: Presence
    var gameName: String?
    var gameID: Int?
    init?(json: [String: Any]) {
        guard let id = json["steamid"] as? String else { return nil }
        self.id = id
        name = json["persona_name"] as? String ?? id
        avatarURL = (json["avatar_url"] as? String).flatMap(URL.init(string:))
        state = Presence(rawValue: json["state"] as? String ?? "offline") ?? .offline
        gameName = json["in_game_name"] as? String
        gameID = json["in_game_appid"] as? Int
    }
    var status: String {
        if let gameName, !gameName.isEmpty { return gameName }
        if let gameID, gameID != 0 { return "В игре" }
        return state.title
    }
    var inGame: Bool { (gameID ?? 0) != 0 || !(gameName ?? "").isEmpty }
    var sortRank: Int { inGame ? 0 : state == .online ? 1 : state == .away || state == .busy ? 2 : 3 }
}

struct ChatMessage: Identifiable, Equatable {
    let id: String
    let senderID: String
    let text: String
    let timestamp: TimeInterval
    let outgoing: Bool
    var clientMessageID: String?
    var state: String
    init(id: String, senderID: String, text: String, timestamp: TimeInterval, outgoing: Bool, clientMessageID: String?, state: String) {
        self.id = id; self.senderID = senderID; self.text = text; self.timestamp = timestamp
        self.outgoing = outgoing; self.clientMessageID = clientMessageID; self.state = state
    }
    init?(json: [String: Any]) {
        guard let id = json["id"] as? String, let text = json["text"] as? String else { return nil }
        self.id = id
        senderID = json["sender_steamid"] as? String ?? ""
        self.text = text
        timestamp = (json["timestamp"] as? NSNumber)?.doubleValue ?? Date().timeIntervalSince1970
        outgoing = json["outgoing"] as? Bool ?? false
        clientMessageID = json["client_message_id"] as? String
        state = json["state"] as? String ?? (outgoing ? "sent" : "received")
    }
}

struct PlatformRequirements: Identifiable {
    let id: String
    let minimum: String
    let recommended: String
    var title: String { id == "mac" ? "macOS" : id == "linux" ? "Linux / SteamOS" : "Windows" }
}

struct StoreAppDetails {
    let appID: Int
    let description: String
    let requirements: [PlatformRequirements]
    init?(json: [String: Any]) {
        guard let appID = json["appid"] as? Int else { return nil }
        self.appID = appID
        description = json["description"] as? String ?? ""
        let platforms = json["platforms"] as? [String: Bool] ?? [:]
        let data = json["requirements"] as? [String: [String: Any]] ?? [:]
        requirements = ["windows", "mac", "linux"].filter { platforms[$0] == true }.map { platform in
            PlatformRequirements(id: platform, minimum: data[platform]?["minimum"] as? String ?? "",
                                 recommended: data[platform]?["recommended"] as? String ?? "")
        }
    }
}

@MainActor
extension AppModel {
    var selectedFriend: SteamFriend? { friends.first { $0.id == selectedFriendID } }
    var filteredFriends: [SteamFriend] {
        friends.filter { friendSearch.isEmpty || $0.name.localizedCaseInsensitiveContains(friendSearch) }
            .sorted { $0.sortRank == $1.sortRank ? $0.name.localizedStandardCompare($1.name) == .orderedAscending : $0.sortRank < $1.sortRank }
    }
    var unreadTotal: Int { unreadCounts.values.reduce(0, +) }

    func resetCommunity(preserveNotificationRoute: Bool = false) {
        resetSteamInputDiagnostics()
        if !preserveNotificationRoute { notificationRouteWaiting = nil }
        notifications.deactivate()
        launchNotificationIDs = [:]
        noticeItems = []; noticesLoading = false; noticesLoaded = false; noticesError = nil
        notificationRequests = []; locallyReadNoticeIDs = []
        resetDownloadQueue()
        uninstallingGames = []; uninstallRequests = [:]; uninstallNotices = [:]
        uninstallPlans = [:]; loadingUninstallPlans = []; uninstallPlanRequests = [:]
        gameForLog = nil; gameLogs = [:]; gameLogErrors = [:]; loadingGameLogs = []
        gameLogRequests = [:]; latestGameLogRequests = [:]
        friends = []; chats = [:]; chatErrors = [:]; chatLoading = []; unreadCounts = [:]
        friendsLoaded = false; friendsLoading = false; friendsError = nil; selectedFriendID = nil
        socialRequests = [:]; requestedSocial = false; presence = .invisible; presencePending = false
        pendingChatRequests = [:]
        websiteDataStore = .nonPersistent(); webSessionRevision = 0
        webCookies = []; webSessionReady = false; webSessionLoading = false; webSessionError = nil
        webSessionRequest = nil; webSessionTargets = []; webSessionRequests = [:]; webSessionExpiry = [:]; webIdentity = UUID()
        storeURL = URL(string: "https://store.steampowered.com/")!
        profileURL = URL(string: "https://steamcommunity.com/my/")!
        favoriteIDs = []
    }

    func loadCommunity() {
        guard account != nil, !requestedSocial else { return }
        requestedSocial = true
        refreshDownloadQueue()
        // Steam notifications arrive from the backend after its initial library load.
        refreshFriends()
        if let id = send("get_profile") { socialRequests[id] = ("get_profile", "") }
    }

    func refreshFriends() {
        guard account != nil else { return }
        friendsLoading = true; friendsError = nil
        if let id = send("get_friends") { socialRequests[id] = ("get_friends", "") }
        else { friendsLoading = false }
    }

    func changePresence(_ value: Presence) {
        guard account != nil, !presencePending else { return }
        presencePending = true
        if let id = send("set_presence", payload: ["state": value.rawValue]) { socialRequests[id] = ("set_presence", "") }
        else { presencePending = false }
    }

    func selectFriend(_ friend: SteamFriend) {
        selectedFriendID = friend.id; markVisibleChatRead()
        if chats[friend.id] == nil { loadChat(friend.id) }
    }

    func loadChat(_ steamID: String) {
        guard account != nil, !chatLoading.contains(steamID) else { return }
        chatErrors[steamID] = nil; chatLoading.insert(steamID)
        if let id = send("get_chat_history", payload: ["steamid": steamID]) { socialRequests[id] = ("get_chat_history", steamID) }
        else { chatLoading.remove(steamID) }
    }

    func sendChat(_ text: String, to steamID: String) {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.utf8.count <= 6_000, let account else { return }
        let clientID = UUID().uuidString
        if let id = send("send_message", payload: ["steamid": steamID, "text": text, "client_message_id": clientID]) {
            socialRequests[id] = ("send_message", steamID)
            pendingChatRequests[id] = clientID
            chats[steamID, default: []].append(ChatMessage(id: clientID, senderID: account.id, text: text,
                timestamp: Date().timeIntervalSince1970, outgoing: true, clientMessageID: clientID, state: "sending"))
        }
    }

    func retryMessage(_ message: ChatMessage, to steamID: String) {
        chats[steamID]?.removeAll { $0.id == message.id }
        sendChat(message.text, to: steamID)
    }

    func toggleFavorite(_ game: SteamGame) {
        guard let account else { return }
        if favoriteIDs.contains(game.id) { favoriteIDs.remove(game.id) } else { favoriteIDs.insert(game.id) }
        UserDefaults.standard.set(Array(favoriteIDs), forKey: "favorites.\(account.id)")
    }

    func loadDetails(_ game: SteamGame, force: Bool = false) {
        guard !detailsLoading.contains(game.id), force || appDetails[game.id] == nil else { return }
        detailsLoading.insert(game.id); detailsErrors[game.id] = nil
        if let id = send("get_game_details", payload: ["appid": game.id]) { detailRequests[id] = game.id }
        else { detailsLoading.remove(game.id) }
    }

    func openStore(_ game: SteamGame) {
        storeURL = URL(string: "https://store.steampowered.com/app/\(game.id)/")!
        section = .store
    }

    func showProfile(_ steamID: String? = nil) {
        guard let id = steamID ?? account?.id, id.allSatisfy(\.isNumber) else { return }
        profileURL = URL(string: "https://steamcommunity.com/profiles/\(id)/")!
        section = .profile
    }

    func loadWebSession(target: String, force: Bool = false) {
        let expired = (webSessionExpiry[target] ?? 0) <= Date().timeIntervalSince1970 + 60
        guard account != nil, force || expired || !webSessionTargets.contains(target), !webSessionRequests.values.contains(target) else { return }
        webSessionLoading = true; webSessionError = nil
        if let id = send("get_web_session", payload: ["target": target]) { webSessionRequests[id] = target }
        else { webSessionLoading = false }
    }

    func receiveCommunity(_ event: String, payload: [String: Any], requestID: String?) -> Bool {
        switch event {
        case "friends":
            friends = (payload["friends"] as? [[String: Any]] ?? []).compactMap(SteamFriend.init(json:))
            friendsLoading = false; friendsLoaded = true; friendsError = nil
            if let requestID { socialRequests.removeValue(forKey: requestID) }
            resumeNotificationRouteIfPossible()
            return true
        case "friend_update":
            if let json = payload["friend"] as? [String: Any], let friend = SteamFriend(json: json) {
                if let index = friends.firstIndex(where: { $0.id == friend.id }) { friends[index] = friend }
                else { friends.append(friend) }
            }
            return true
        case "profile":
            if let current = account, payload["steamid"] as? String == current.id {
                account = SteamAccount(id: current.id, accountName: current.accountName,
                    displayName: payload["persona_name"] as? String ?? current.displayName,
                    avatarURL: (payload["avatar_url"] as? String).flatMap(URL.init(string:)) ?? current.avatarURL)
                presence = Presence(rawValue: payload["state"] as? String ?? "invisible") ?? .invisible
            }
            if let requestID { socialRequests.removeValue(forKey: requestID) }
            return true
        case "presence":
            presence = Presence(rawValue: payload["state"] as? String ?? "invisible") ?? .invisible
            presencePending = false
            if let requestID { socialRequests.removeValue(forKey: requestID) }
            return true
        case "chat_history":
            guard let steamID = payload["steamid"] as? String else { return true }
            let history = (payload["messages"] as? [[String: Any]] ?? []).compactMap(ChatMessage.init(json:))
            // Incoming messages and sends may arrive while the history is in flight.
            for message in history { mergeMessage(message, into: steamID) }
            chats[steamID, default: []].sort { $0.timestamp < $1.timestamp }
            chatLoading.remove(steamID); chatErrors[steamID] = nil
            if let requestID { socialRequests.removeValue(forKey: requestID) }
            return true
        case "chat_message":
            guard let steamID = payload["steamid"] as? String,
                  let json = payload["message"] as? [String: Any], let message = ChatMessage(json: json) else { return true }
            let existed = chats[steamID]?.contains { $0.id == message.id } == true
            mergeMessage(message, into: steamID)
            if !message.outgoing, !existed, !isViewingChat(steamID) { unreadCounts[steamID, default: 0] += 1 }
            if let requestID { socialRequests.removeValue(forKey: requestID); pendingChatRequests.removeValue(forKey: requestID) }
            return true
        case "game_details":
            if let details = StoreAppDetails(json: payload) {
                appDetails[details.appID] = details; detailsLoading.remove(details.appID); detailsErrors[details.appID] = nil
            }
            if let requestID { detailRequests.removeValue(forKey: requestID) }
            return true
        case "web_session":
            guard let account, payload["steamid"] as? String == account.id,
                  let requestID, let target = webSessionRequests.removeValue(forKey: requestID) else { return true }
            let allowed = Set(["store.steampowered.com", "steamcommunity.com", "checkout.steampowered.com"])
            let now = Date().timeIntervalSince1970
            guard let expires = (payload["expires_at"] as? NSNumber)?.doubleValue, expires > now + 60,
                  let rows = payload["cookies"] as? [[String: Any]], rows.count == 6 else {
                webSessionLoading = !webSessionRequests.isEmpty
                webSessionError = "Steam вернул неполную веб-сессию. Повторите подключение."
                return true
            }
            var cookies: [HTTPCookie] = []
            for json in rows {
                guard let name = json["name"] as? String, ["steamLoginSecure", "sessionid"].contains(name),
                      let value = json["value"] as? String, !value.isEmpty,
                      let domain = json["domain"] as? String, allowed.contains(domain), json["path"] as? String == "/",
                      json["secure"] as? Bool == true, name != "steamLoginSecure" || json["http_only"] as? Bool == true,
                      let cookieExpiry = (json["expires_at"] as? NSNumber)?.doubleValue, cookieExpiry > now + 60 else { continue }
                var properties: [HTTPCookiePropertyKey: Any] = [.name: name, .value: value, .domain: domain, .path: "/", .secure: "TRUE", .expires: Date(timeIntervalSince1970: min(expires, cookieExpiry))]
                if json["http_only"] as? Bool == true { properties[HTTPCookiePropertyKey("HttpOnly")] = "TRUE" }
                if let cookie = HTTPCookie(properties: properties) { cookies.append(cookie) }
            }
            guard Set(cookies.map { $0.domain + "/" + $0.name }).count == 6 else {
                webSessionLoading = !webSessionRequests.isEmpty
                webSessionError = "Не удалось проверить веб-сессию Steam. Повторите подключение."
                return true
            }
            webCookies = cookies
            webSessionExpiry[target] = expires
            webSessionError = nil
            webSessionRevision += 1
            webSessionTargets.insert(target); webSessionReady = true; webSessionLoading = !webSessionRequests.isEmpty
            return true
        case "error":
            guard let requestID else { return false }
            let message = payload["message"] as? String ?? "Не удалось выполнить запрос."
            if let appID = detailRequests.removeValue(forKey: requestID) { detailsLoading.remove(appID); detailsErrors[appID] = message; return true }
            if webSessionRequests.removeValue(forKey: requestID) != nil { webSessionLoading = !webSessionRequests.isEmpty; webSessionError = message; return true }
            if let request = socialRequests.removeValue(forKey: requestID) {
                switch request.command {
                case "get_friends": friendsLoading = false; friendsError = message
                case "get_chat_history": chatLoading.remove(request.target); chatErrors[request.target] = message
                case "send_message":
                    if let clientID = pendingChatRequests.removeValue(forKey: requestID),
                       let index = chats[request.target]?.firstIndex(where: { $0.clientMessageID == clientID }) { chats[request.target]?[index].state = "failed" }
                    chatErrors[request.target] = message
                case "set_presence": presencePending = false; errorMessage = message
                default: break
                }
                return true
            }
            return false
        default: return false
        }
    }

    private func mergeMessage(_ message: ChatMessage, into steamID: String) {
        var messages = chats[steamID, default: []]
        messages.removeAll { $0.id == message.id || (message.clientMessageID != nil && $0.clientMessageID == message.clientMessageID) }
        messages.append(message)
        chats[steamID] = messages.sorted { $0.timestamp == $1.timestamp ? $0.id < $1.id : $0.timestamp < $1.timestamp }
    }
}
