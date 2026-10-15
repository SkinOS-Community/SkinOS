import Foundation
import Testing
@testable import SkinOS

@MainActor
struct CommunityTests {
    private func account() -> SteamAccount { SteamAccount(id: "123", accountName: "fixture", displayName: "Fixture", avatarURL: nil) }
    private func message(_ id: String, time: Double, text: String = "Fixture", outgoing: Bool = false, client: String? = nil) -> [String: Any] {
        var value: [String: Any] = ["id": id, "text": text, "sender_steamid": outgoing ? "123" : "456", "timestamp": time, "outgoing": outgoing, "state": outgoing ? "sent" : "received"]
        if let client { value["client_message_id"] = client }
        return value
    }
    private func session() -> [String: Any] {
        let expiry = Date().timeIntervalSince1970 + 600
        let cookies = ["store.steampowered.com", "steamcommunity.com", "checkout.steampowered.com"].flatMap { domain in
            ["steamLoginSecure", "sessionid"].map { name in
                ["name": name, "value": "synthetic-test-value", "domain": domain, "path": "/", "secure": true, "http_only": name == "steamLoginSecure", "expires_at": expiry] as [String: Any]
            }
        }
        return ["steamid": "123", "target": "store", "expires_at": expiry, "cookies": cookies]
    }

    @Test func lateHistoryPreservesNewPushAndSendAcknowledgementDeduplicates() {
        let model = AppModel(); model.account = account()
        model.chats["456"] = [ChatMessage(id: "pending", senderID: "123", text: "My message", timestamp: 3, outgoing: true, clientMessageID: "client", state: "sending")]
        _ = model.receiveCommunity("chat_message", payload: ["steamid": "456", "message": message("server", time: 3, text: "My message", outgoing: true)], requestID: nil)
        _ = model.receiveCommunity("chat_message", payload: ["steamid": "456", "message": message("new", time: 4)], requestID: nil)
        _ = model.receiveCommunity("chat_history", payload: ["steamid": "456", "messages": [message("old", time: 1), message("server", time: 3, text: "My message", outgoing: true)]], requestID: nil)
        _ = model.receiveCommunity("chat_message", payload: ["steamid": "456", "message": message("server", time: 3, text: "My message", outgoing: true, client: "client")], requestID: "ack")
        #expect(model.chats["456"]?.map(\.id) == ["old", "server", "new"])
        #expect(model.chats["456"]?[1].state == "sent")
    }

    @Test func rejectedSendIsMarkedFailedAndNeverRetriedAutomatically() {
        let model = AppModel(); model.account = account()
        model.chats["456"] = [ChatMessage(id: "pending", senderID: "123", text: "Fixture", timestamp: 3, outgoing: true, clientMessageID: "client", state: "sending")]
        model.socialRequests["request"] = ("send_message", "456")
        model.pendingChatRequests["request"] = "client"
        _ = model.receiveCommunity("error", payload: ["code": "chat_send_failed", "message": "Server rejected"], requestID: "request")
        #expect(model.chats["456"]?.count == 1)
        #expect(model.chats["456"]?.first?.state == "failed")
        #expect(model.chatErrors["456"] == "Server rejected")
        #expect(model.pendingChatRequests.isEmpty)
    }

    @Test func webSessionIsAccountScopedAndRejectsIncompleteCookies() {
        let model = AppModel(); model.account = account(); model.webSessionRequests["request"] = "store"
        _ = model.receiveCommunity("web_session", payload: session(), requestID: "request")
        #expect(model.webCookies.count == 6)
        #expect(model.webCookies.allSatisfy { $0.isSecure })
        #expect(model.webCookies.filter { $0.name == "steamLoginSecure" }.allSatisfy { $0.isHTTPOnly })
        #expect(model.webSessionTargets.contains("store"))
        let identity = model.webIdentity
        let dataStore = model.websiteDataStore
        model.resetCommunity()
        #expect(model.webCookies.isEmpty)
        #expect(model.webIdentity != identity)
        #expect(model.websiteDataStore !== dataStore)
        #expect(model.presence == .invisible)
        model.webSessionRequests["bad"] = "store"
        var invalid = session(); invalid["cookies"] = []
        _ = model.receiveCommunity("web_session", payload: invalid, requestID: "bad")
        #expect(model.webSessionTargets.isEmpty)
        #expect(model.webSessionError != nil)
    }

    @Test func libraryFiltersIntersectAndRecentSortUsesTimestamp() {
        let model = AppModel()
        model.games = [
            ["appid": 1, "name": "A", "windows_supported": true, "installed_dir": "/fixture", "playtime_minutes": 9, "last_played": 2],
            ["appid": 2, "name": "B", "windows_supported": true, "installed_dir": "/fixture", "playtime_minutes": 5, "last_played": 4],
            ["appid": 3, "name": "C", "linux_supported": true, "playtime_minutes": 0, "last_played": 0]
        ].compactMap(SteamGame.init(json:))
        model.filter = .windows; model.installFilter = .installed; model.playFilter = .played; model.librarySort = .recent
        #expect(model.filteredGames.map(\.id) == [2, 1])
        model.favoriteIDs = [1]; model.playFilter = .favorites
        #expect(model.filteredGames.map(\.id) == [1])
        model.filter = .linux
        #expect(model.filteredGames.isEmpty)
    }

    @Test func delayedStopFailureDoesNotResurrectExitedGame() {
        let model = AppModel()
        model.launchPhases[42] = "stopping"
        model.requestCommands["stop-request"] = "stop_game"
        model.receive(["event": "launch_status", "payload": ["appid": 42, "state": "exited"]])
        model.receive(["id": "stop-request", "event": "error", "payload": ["appid": 42, "code": "stop_failed", "message": "Process already exited"]])
        #expect(model.launchPhases[42] == "exited")
        #expect(model.launchStates[42] == "Игра закрыта")
        model.launchPhases[42] = "stopping"
        model.requestCommands["second-stop"] = "stop_game"
        model.receive(["id": "second-stop", "event": "error", "payload": ["appid": 42, "code": "stop_failed", "message": "Termination refused"]])
        #expect(model.launchPhases[42] == "running")
    }

    @Test func embeddedBrowserRejectsClientProtocolAndDeceptiveOrigins() {
        #expect(SteamWebPolicy.allows(URL(string: "https://store.steampowered.com/app/1942280/")!))
        for value in ["steam://run/1942280", "http://steamcommunity.com/", "https://steamcommunity.com.evil.example/", "https://evil.example@steamcommunity.com/", "https://steamcommunity.com:444/"] {
            #expect(!SteamWebPolicy.allows(URL(string: value)!))
        }
    }

    @Test func requirementsPreserveEverySupportedPlatformAndMissingPublisherText() {
        let details = StoreAppDetails(json: ["appid": 1, "platforms": ["windows": true, "mac": true, "linux": true], "requirements": ["windows": ["minimum": "Fixture CPU", "recommended": "Fixture RAM"], "mac": [:], "linux": ["minimum": "Fixture Linux"]]])!
        #expect(details.requirements.map(\.id) == ["windows", "mac", "linux"])
        #expect(details.requirements[1].minimum.isEmpty)
        #expect(details.requirements[2].minimum == "Fixture Linux")
    }
}
