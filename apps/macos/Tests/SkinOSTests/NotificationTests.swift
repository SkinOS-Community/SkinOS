import Foundation
import Testing
@testable import SkinOS

@MainActor
struct NotificationTests {
    final class Deliveries {
        var notices: [ClientNotice] = []
        var sounds: [Bool] = []
        var activations = 0
    }
    private func fixture() -> (AppModel, Deliveries) {
        let deliveries = Deliveries()
        let service = ClientNotifications(defaults: nil) { notice, sound in
            deliveries.notices.append(notice); deliveries.sounds.append(sound)
        }
        let model = AppModel(notifications: service)
        model.activateApplication = { deliveries.activations += 1 }
        model.account = SteamAccount(id: "100", accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        service.activate(account: "100")
        model.friends = [SteamFriend(json: ["steamid": "200", "persona_name": "Test fixture", "state": "online"])!]
        model.chats["200"] = []
        return (model, deliveries)
    }
    private func row(_ id: String, category: String = "direct_message") -> [String: Any] {
        ["id": id, "type": 13, "category": category, "title": "Test fixture", "body": "Synthetic local fixture text",
         "friend_steamid": "200", "timestamp": 1_700_000_000, "read": false]
    }
    private func push(_ id: String, owner: String = "100", notify: Bool = true, category: String = "direct_message") -> [String: Any] {
        ["event": "steam_notification", "payload": ["steamid": owner, "notification": row(id, category: category), "is_new": true, "notify": notify]]
    }
    private func chat(_ id: String, outgoing: Bool = false) -> [String: Any] {
        ["event": "chat_message", "payload": ["steamid": "200", "message": ["id": id, "sender_steamid": outgoing ? "100" : "200", "text": "Fixture", "timestamp": 100, "outgoing": outgoing]]]
    }

    @Test func unifiedLivePushOnlyNoHistoryEchoOrDuplicateAlerts() {
        let (model, deliveries) = fixture()
        model.receive(["event": "chat_history", "payload": ["steamid": "200", "messages": []]])
        model.receive(chat("incoming"))
        model.receive(chat("echo", outgoing: true))
        #expect(deliveries.notices.isEmpty)
        model.receive(push("live"))
        model.receive(push("live"))
        model.receive(push("archive", notify: false))
        #expect(deliveries.notices.map(\.id) == ["live"])
        #expect(model.unreadCounts["200"] == 1)
    }

    @Test func snapshotBeforeIncrementalDoesNotSwallowNewNotification() {
        let (model, deliveries) = fixture()
        model.receive(["event": "steam_notifications", "payload": ["steamid": "100", "initial": true, "notifications": [row("old"), row("new")], "unread_count": 2]])
        #expect(deliveries.notices.isEmpty)
        model.receive(push("new"))
        #expect(deliveries.notices.map(\.id) == ["new"])
        model.receive(["event": "steam_notifications", "payload": ["steamid": "100", "initial": false, "notifications": [row("new")]]])
        model.receive(push("new"))
        #expect(deliveries.notices.count == 1)
    }

    @Test func optionalNotificationFailureStaysInInboxAndPreservesGameState() {
        let (model, _) = fixture()
        model.noticeItems = [ClientNotice(id: "existing", owner: "100", kind: .steam, title: "Fixture", body: "Retained", target: .steamInbox, timestamp: Date())]
        model.launchPhases[42] = "running"
        model.errorMessage = "Existing game error"
        for code in ["notifications_failed", "notifications_storage_failed", "notifications_busy"] {
            model.noticesLoading = true
            model.receive(["event": "error", "payload": ["code": code, "message": "Optional service unavailable"]])
            #expect(model.noticesError == "Optional service unavailable")
            #expect(!model.noticesLoading)
            #expect(model.errorMessage == "Existing game error")
            #expect(model.launchPhases[42] == "running")
            #expect(model.noticeItems.map(\.id) == ["existing"])
        }
    }

    @Test func backgroundNotificationStatusIsAccountScopedAndManualErrorsStayLocal() {
        let (model, _) = fixture()
        model.noticeItems = [ClientNotice(id: "existing", owner: "100", kind: .steam, title: "Fixture", body: "Retained", target: .steamInbox, timestamp: Date())]
        model.errorMessage = "Existing game error"
        model.receive(["event": "steam_notifications_status", "payload": ["steamid": "999", "state": "unavailable", "message": "Other account"]])
        #expect(model.noticesError == nil)
        model.receive(["event": "steam_notifications_status", "payload": ["steamid": "100", "state": "degraded", "message": "Service unavailable", "retryable": true]])
        #expect(model.noticesError == "Service unavailable")
        #expect(model.noticeItems.map(\.id) == ["existing"])
        model.receive(["event": "steam_notifications_status", "payload": ["steamid": "100", "state": "ready"]])
        #expect(model.noticesError == nil)
        model.notificationRequests.insert("manual-refresh")
        model.noticesLoading = true
        model.receive(["id": "manual-refresh", "event": "error", "payload": ["code": "notifications_failed", "message": "Manual request timed out"]])
        #expect(model.noticesError == "Manual request timed out")
        #expect(model.notificationRequests.isEmpty)
        #expect(!model.noticesLoading)
        #expect(model.errorMessage == "Existing game error")
        #expect(model.noticeItems.map(\.id) == ["existing"])
    }

    @Test func fullSnapshotRemovesWithdrawnSteamRowsButRetainsLocalEventsAndReadState() {
        let (model, deliveries) = fixture()
        model.mergeNotice(ClientNotice(id: "local-download", owner: "100", kind: .download, title: "Fixture", body: "Done", target: .download(42), timestamp: Date()))
        model.mergeNotice(ClientNotice(id: "local-game", owner: "100", kind: .game, title: "Fixture", body: "Failed", target: .game(42), timestamp: Date()))
        model.receive(["event": "steam_notifications", "payload": ["steamid": "100", "notifications": [row("withdrawn", category: "friend_request"), row("kept")]]])
        model.locallyReadNoticeIDs.insert("kept")
        model.receive(["event": "steam_notifications", "payload": ["steamid": "100", "notifications": [row("kept")]]])
        #expect(Set(model.noticeItems.map(\.id)) == Set(["local-download", "local-game", "kept"]))
        #expect(model.noticeItems.first { $0.id == "kept" }?.read == true)
        #expect(deliveries.notices.isEmpty)
    }

    @Test func delayedDMAlreadyLoadedInHistoryDoesNotNotifyAfterLeavingChat() {
        let (model, deliveries) = fixture()
        model.section = .friends; model.selectedFriendID = "200"
        model.applicationIsActive = true; model.applicationWindowVisible = true
        model.receive(["event": "chat_history", "payload": ["steamid": "200", "messages": [["id": "canonical-old", "sender_steamid": "200", "text": "Fixture", "timestamp": 100, "outgoing": false]]]])
        model.section = .library; model.applicationIsActive = false
        model.receive(push("canonical-old"))
        #expect(deliveries.notices.isEmpty)
        model.receive(push("canonical-new"))
        model.receive(chat("canonical-new"))
        #expect(deliveries.notices.map(\.id) == ["canonical-new"])
    }

    @Test func onlyVisibleActiveConversationSuppressesDMAndUnread() {
        let (model, deliveries) = fixture()
        model.section = .friends; model.selectedFriendID = "200"
        model.applicationIsActive = true; model.applicationWindowVisible = true
        model.receive(push("visible")); model.receive(chat("visible"))
        #expect(deliveries.notices.isEmpty)
        #expect(model.unreadCounts["200", default: 0] == 0)
        model.applicationWindowVisible = false // Minimized or closed, even if NSApp remains active.
        model.receive(push("minimized")); model.receive(chat("minimized"))
        #expect(deliveries.notices.map(\.id) == ["minimized"])
        #expect(model.unreadCounts["200"] == 1)
        model.applicationWindowVisible = true; model.applicationIsActive = false
        model.receive(push("inactive")); model.receive(chat("inactive"))
        #expect(deliveries.notices.count == 2)
        #expect(model.unreadCounts["200"] == 2)
        model.applicationIsActive = true; model.markVisibleChatRead()
        #expect(model.unreadCounts["200"] == 0)
    }

    @Test func togglesAndHiddenPreviewAreAppliedWithoutSendingMessages() {
        let (model, deliveries) = fixture()
        model.notifications.preferences.messages = false
        model.receive(push("disabled"))
        #expect(deliveries.notices.isEmpty)
        model.notifications.preferences.messages = true
        model.notifications.preferences.previews = false
        model.notifications.preferences.sound = false
        model.receive(push("hidden"))
        #expect(deliveries.notices.first?.body == "Новое сообщение Steam")
        #expect(deliveries.sounds == [false])
        model.notifications.preferences.steam = false
        model.receive(push("friend", category: "friend_request"))
        #expect(deliveries.notices.count == 1)
    }

    @Test func crossAccountEventsAndClicksCannotOpenAnotherAccountChat() {
        let (model, deliveries) = fixture()
        model.receive(push("foreign", owner: "999"))
        #expect(deliveries.notices.isEmpty)
        #expect(model.noticeItems.isEmpty)
        model.openNotification(owner: "999", target: .chat("200"))
        #expect(model.section == .library)
        #expect(model.selectedFriendID == nil)
        #expect(model.notificationRouteWaiting?.owner == "999")
        #expect(deliveries.activations == 0)
        model.openNotification(owner: "100", target: .chat("200"))
        #expect(model.section == .friends)
        #expect(model.selectedFriendID == "200")
        #expect(deliveries.activations == 1)
        model.resetCommunity()
        #expect(model.notificationRouteWaiting == nil)
        #expect(model.notifications.activeAccount == nil)
    }

    @Test func coldNotificationClickWaitsForMatchingLoginAndFriends() {
        let (model, deliveries) = fixture()
        model.account = nil; model.friends = []
        model.openNotification(owner: "100", target: .chat("200"))
        model.receive(["event": "status", "payload": ["state": "signed_out"]])
        #expect(model.notificationRouteWaiting?.owner == "100")
        model.resetCommunity(preserveNotificationRoute: true)
        model.account = SteamAccount(id: "100", accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        model.resumeNotificationRouteIfPossible()
        #expect(model.notificationRouteWaiting != nil)
        model.chats["200"] = []
        model.receive(["event": "friends", "payload": ["friends": [["steamid": "200", "persona_name": "Fixture", "state": "online"]]]])
        #expect(model.notificationRouteWaiting == nil)
        #expect(model.section == .friends)
        #expect(deliveries.activations == 1)
    }

    @Test func logoutCleanupIncludesOldProcessSessionsButProtectsNewLogin() {
        #expect(ClientNotifications.shouldRemoveOnLogout(notificationOwner: "100", notificationSession: "previous-process", loggedOutAccount: "100", currentAccount: nil, currentSession: "new"))
        #expect(ClientNotifications.shouldRemoveOnLogout(notificationOwner: "100", notificationSession: "old", loggedOutAccount: "100", currentAccount: "100", currentSession: "new"))
        #expect(!ClientNotifications.shouldRemoveOnLogout(notificationOwner: "100", notificationSession: "new", loggedOutAccount: "100", currentAccount: "100", currentSession: "new"))
        #expect(!ClientNotifications.shouldRemoveOnLogout(notificationOwner: "200", notificationSession: "old", loggedOutAccount: "100", currentAccount: nil, currentSession: "new"))
    }

    @Test func repeatedFailureEventsProduceOneGameNotification() {
        let (model, deliveries) = fixture()
        model.launchNotificationIDs[42] = "attempt"
        model.launchPhases[42] = "starting"
        model.gameRequests["attempt"] = 42
        model.requestCommands["attempt"] = "launch_game"
        model.receive(["event": "launch_status", "id": "attempt", "payload": ["appid": 42, "state": "failed"]])
        model.receive(["event": "error", "id": "attempt", "payload": ["appid": 42, "code": "launch_failed", "message": "Fixture failure"]])
        #expect(deliveries.notices.count == 1)
        #expect(deliveries.notices.first?.kind == .game)
        #expect(deliveries.sounds == [true])
    }

    @Test func completedQueueSnapshotDoesNotNotifyButRealTransitionDoes() {
        let (model, deliveries) = fixture()
        func snapshot(_ revision: Int, _ state: String) -> [String: Any] {
            ["event": "download_queue", "payload": ["steamid": "100", "revision": revision, "max_concurrent": 2, "items": [["appid": 42, "state": state, "queue_position": 0]]]]
        }
        model.receive(snapshot(1, "completed"))
        #expect(deliveries.notices.isEmpty)
        model.receive(snapshot(2, "downloading"))
        model.receive(snapshot(3, "completed"))
        model.receive(snapshot(3, "completed"))
        #expect(deliveries.notices.count == 1)
        #expect(deliveries.notices.first?.kind == .download)
    }
}
