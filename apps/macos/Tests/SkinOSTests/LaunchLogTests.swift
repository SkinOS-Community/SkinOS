import Foundation
import Testing
@testable import SkinOS

@MainActor
struct LaunchLogTests {
    private func request(_ id: String, appID: Int, model: AppModel) {
        model.gameLogRequests[id] = appID
        model.latestGameLogRequests[appID] = id
        model.loadingGameLogs.insert(appID)
        model.requestCommands[id] = "get_game_log"
    }

    @Test func logRepliesStayWithTheirRequestedGame() {
        let model = AppModel()
        request("a", appID: 1, model: model)
        request("b", appID: 2, model: model)
        model.receive(["id": "b", "event": "game_log", "payload": ["appid": 2, "log_id": "second", "text": "[10:00:00] Game B", "updated_at": 1_700_000_000, "truncated": true]])
        model.receive(["id": "a", "event": "game_log", "payload": ["appid": 1, "text": "[10:00:01] Game A", "truncated": false]])
        #expect(model.gameLogs[1]?.text == "[10:00:01] Game A")
        #expect(model.gameLogs[2]?.text == "[10:00:00] Game B")
        #expect(model.gameLogs[2]?.truncated == true)
        #expect(model.gameLogs[2]?.updatedAt == Date(timeIntervalSince1970: 1_700_000_000))
        #expect(model.loadingGameLogs.isEmpty)
        #expect(model.requestCommands.isEmpty)
    }

    @Test func staleRequestCannotOverwriteLatestLog() {
        let model = AppModel()
        request("old", appID: 1, model: model)
        request("latest", appID: 1, model: model)
        model.receive(["id": "latest", "event": "game_log", "payload": ["appid": 1, "text": "Current launch", "truncated": false]])
        model.receive(["id": "old", "event": "game_log", "payload": ["appid": 1, "text": "Previous launch", "truncated": false]])
        #expect(model.gameLogs[1]?.text == "Current launch")
        #expect(model.gameLogRequests.isEmpty)
    }

    @Test func malformedCrossGameReplyIsRejectedAndRetryable() {
        let model = AppModel()
        request("request", appID: 1, model: model)
        model.receive(["id": "request", "event": "game_log", "payload": ["appid": 2, "text": "Unexpected game", "truncated": false]])
        #expect(model.gameLogs.isEmpty)
        #expect(model.gameLogErrors[1] != nil)
        #expect(!model.loadingGameLogs.contains(1))
    }

    @Test func repliesFromLoggedOutAccountAreDiscarded() {
        let model = AppModel()
        request("request", appID: 1, model: model)
        model.resetCommunity()
        model.receive(["id": "request", "event": "game_log", "payload": ["appid": 1, "text": "Previous account", "truncated": false]])
        #expect(model.gameLogs.isEmpty)
    }
}
