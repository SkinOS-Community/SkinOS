import Foundation
import Testing
@testable import SkinOS

@MainActor
struct ReconnectTests {
    @Test func connectionDropPreservesLibraryAndRunningGame() {
        let model = AppModel()
        model.account = SteamAccount(id: "76561198000000000", accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        model.games = [SteamGame(json: ["appid": 42, "name": "Fixture"])!]
        model.launchPhases[42] = "running"
        model.receive(["event": "status", "payload": ["state": "disconnected", "retry_in_seconds": 10]])
        #expect(model.account?.id == "76561198000000000")
        #expect(model.games.count == 1)
        #expect(model.launchPhases[42] == "running")
    }
}

@MainActor
struct AutomaticSignInTests {
    private func configuredModel(_ commands: @escaping (String, [String: Any]) -> Void) -> AppModel {
        let model = AppModel()
        model.commandSink = { command, payload in commands(command, payload); return UUID().uuidString }
        return model
    }
    private var saved: [String: Any] { ["steamid": "76561198000000000", "account_name": "fixture", "last_used": 99] }

    @Test func initialStatusStartsAutomaticLoginOnlyOnce() {
        var commands: [String] = []
        let model = configuredModel { command, _ in commands.append(command) }
        let request = model.send("status")!
        let status: [String: Any] = ["event": "status", "id": request, "payload": ["state": "signed_out", "accounts": [saved], "auto_login_disabled": false]]
        model.receive(status)
        #expect(commands.filter { $0 == "auto_login" }.count == 1)
        #expect(model.isAuthenticating)
        model.receive(status)
        #expect(commands.filter { $0 == "auto_login" }.count == 1)
    }

    @Test func explicitLogoutAndPersistedOptOutDoNotRestartAutomaticLogin() {
        var commands: [String] = []
        let model = configuredModel { command, _ in commands.append(command) }
        let request = model.send("status")!
        model.logout()
        model.receive(["event": "status", "id": request, "payload": ["state": "signed_out", "accounts": [saved]]])
        #expect(!commands.contains("auto_login"))
        let nextBoot = configuredModel { command, _ in commands.append(command) }
        let nextRequest = nextBoot.send("status")!
        nextBoot.receive(["event": "status", "id": nextRequest, "payload": ["state": "signed_out", "accounts": [saved], "auto_login_disabled": true]])
        #expect(!commands.contains("auto_login"))
    }

    @Test func cachedLibraryAppearsWhileAccountRestoresAndSameAccountKeepsOperations() {
        let model = configuredModel { _, _ in }
        model.receive(["event": "status", "payload": ["state": "connecting", "account": saved]])
        model.receive(["event": "library", "payload": ["steamid": saved["steamid"]!, "cached": true, "games": [["appid": 42, "name": "Cached fixture"]]]])
        #expect(model.account?.id == "76561198000000000")
        #expect(model.games.count == 1)
        #expect(model.libraryIsCached)
        #expect(model.isAuthenticating)
        #expect(!model.steamConnected)
        model.hasAuthenticatedAccount = true
        model.launchPhases[42] = "running"
        model.downloads[42] = GameDownload(id: 42, phase: "paused")
        model.receive(["event": "status", "payload": ["state": "reconnecting", "retry_in_seconds": 10]])
        #expect(model.reconnectSeconds == 10)
        model.receive(["event": "authenticated", "payload": saved])
        #expect(model.launchPhases[42] == "running")
        #expect(model.downloads[42]?.phase == "paused")
        #expect(model.steamConnected)
        #expect(model.reconnectSeconds == nil)
    }

    @Test func revokedSessionShowsQrFallbackAndCancelsCountdownWithoutRetryLoop() {
        var commands: [String] = []
        let model = configuredModel { command, _ in commands.append(command) }
        model.receive(["event": "status", "payload": ["state": "reconnecting", "retry_in_seconds": 10]])
        model.receive(["event": "error", "payload": ["code": "auth_required", "message": "Token rejected"]])
        #expect(model.needsFreshLogin)
        #expect(!model.isReconnecting)
        #expect(model.reconnectSeconds == nil)
        #expect(commands.isEmpty)
        model.retryConnection()
        #expect(commands == ["login_qr"])
    }

    @Test func cancelAndManualRetrySendExactlyOneControl() {
        var commands: [String] = []
        let model = configuredModel { command, _ in commands.append(command) }
        model.receive(["event": "status", "payload": ["state": "reconnecting", "retry_in_seconds": 10]])
        model.retryConnection()
        #expect(commands == ["retry_connection"])
        model.cancelAuth()
        #expect(commands == ["retry_connection", "cancel_auth"])
        #expect(!model.isReconnecting)
        #expect(model.reconnectSeconds == nil)
    }
}
