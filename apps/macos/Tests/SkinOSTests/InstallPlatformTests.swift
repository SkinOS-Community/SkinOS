import Foundation
import Testing
@testable import SkinOS

@MainActor
struct InstallPlatformTests {
    private func game(macOS: Bool = true, windows: Bool = true, installed: String? = nil) -> SteamGame {
        var payload: [String: Any] = ["appid": 570, "name": "Fixture", "macos_supported": macOS, "windows_supported": windows]
        if let installed {
            payload["installed_dir"] = "/fixture/game"
            payload["installed_platform"] = installed
        }
        return SteamGame(json: payload)!
    }

    private func model(_ game: SteamGame) -> AppModel {
        let model = AppModel()
        model.account = SteamAccount(id: "100", accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        model.coreReady = true
        model.games = [game]
        return model
    }

    @Test func macOSIsDefaultEvenWhenLaunchSettingsPreferNotProton() throws {
        let game = game()
        let model = model(game)
        var settings = GameSettings()
        settings.compatibility = "notproton"
        model.gameSettings[game.id] = settings
        var commands: [String] = []
        model.commandSink = { command, _ in commands.append(command); return "fixture" }

        model.install(game)

        let request = try #require(model.installationRequest)
        #expect(request.defaultPlatform == .macOS)
        #expect(request.game.availableInstallPlatforms == [.macOS, .windows])
        #expect(commands.isEmpty)
        #expect(model.pendingEnqueueGames.isEmpty)
        #expect(model.downloads.isEmpty)
    }

    @Test func onlySupportedPlatformsAreSelectable() throws {
        for (mac, windows, expected): (Bool, Bool, InstallPlatform) in [(true, false, .macOS), (false, true, .windows)] {
            let game = game(macOS: mac, windows: windows)
            let model = model(game)
            var commands: [String] = []
            model.commandSink = { command, _ in commands.append(command); return "fixture" }
            model.install(game)
            let request = try #require(model.installationRequest)
            #expect(request.defaultPlatform == expected)
            #expect(game.availableInstallPlatforms == [expected])
            let unsupported: InstallPlatform = expected == .macOS ? .windows : .macOS
            #expect(!model.confirmInstallation(unsupported, requestID: request.id))
            #expect(commands.isEmpty)
            #expect(model.downloads.isEmpty)
        }

        let unsupported = game(macOS: false, windows: false)
        let model = model(unsupported)
        model.install(unsupported)
        #expect(model.installationRequest == nil)
        #expect(model.errorMessage != nil)
    }

    @Test func cancellingPickerNeverEnqueuesAndStaleConfirmationIsIgnored() throws {
        let game = game()
        let model = model(game)
        var commands: [String] = []
        model.commandSink = { command, _ in commands.append(command); return "fixture" }
        model.install(game)
        let first = try #require(model.installationRequest)
        model.cancelInstallation()
        #expect(!model.confirmInstallation(.windows, requestID: first.id))
        model.install(game)
        #expect(!model.confirmInstallation(.macOS, requestID: first.id))
        #expect(commands.isEmpty)
        #expect(model.downloads.isEmpty)
        #expect(model.pendingEnqueueGames.isEmpty)
    }

    @Test func confirmationSendsExactChosenPlatformAndOptimisticRowMatchesIt() throws {
        for platform in InstallPlatform.allCases {
            let game = game()
            let model = model(game)
            var commands: [(String, [String: Any])] = []
            model.commandSink = { command, payload in commands.append((command, payload)); return "fixture" }
            model.install(game)
            let request = try #require(model.installationRequest)

            #expect(model.confirmInstallation(platform, requestID: request.id))

            #expect(commands.count == 1)
            #expect(commands.first?.0 == "install_game")
            #expect(commands.first?.1["appid"] as? Int == game.id)
            #expect(commands.first?.1["platform"] as? String == platform.rawValue)
            #expect(model.downloads[game.id]?.platform == platform)
            #expect(model.pendingEnqueueGames.contains(game.id))
            #expect(model.installationRequest == nil)
            #expect(!model.confirmInstallation(platform, requestID: request.id))
            #expect(commands.count == 1)
        }
    }

    @Test func updatePreservesInstalledVersionAndDoesNotPresentPicker() {
        for platform in InstallPlatform.allCases {
            let game = game(installed: platform.rawValue)
            let model = model(game)
            var settings = GameSettings()
            settings.compatibility = platform == .macOS ? "notproton" : "native"
            model.gameSettings[game.id] = settings
            var commands: [(String, [String: Any])] = []
            model.commandSink = { command, payload in commands.append((command, payload)); return "fixture" }

            model.install(game, update: true)

            #expect(model.installationRequest == nil)
            #expect(commands.count == 1)
            #expect(commands.first?.0 == "update_game")
            #expect(commands.first?.1["platform"] == nil)
            #expect(model.downloads[game.id]?.platform == platform)
            #expect(model.downloads[game.id]?.isUpdate == true)
        }
    }

    @Test func accountChangeOrNewInstalledVersionInvalidatesOpenPicker() throws {
        let game = game()
        let model = model(game)
        var commands: [String] = []
        model.commandSink = { command, _ in commands.append(command); return "fixture" }
        model.install(game)
        let request = try #require(model.installationRequest)
        model.account = SteamAccount(id: "200", accountName: "other", displayName: "Other", avatarURL: nil)
        #expect(!model.confirmInstallation(.macOS, requestID: request.id))
        model.account = SteamAccount(id: "100", accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        model.games[0].installedDirectory = "/fixture/game"
        model.games[0].installedPlatform = "windows"
        #expect(!model.confirmInstallation(.macOS, requestID: request.id))
        #expect(commands.isEmpty)
    }

    @Test func queueAndCompletionShowActualVersionInsteadOfStoreAvailability() throws {
        let game = game()
        let model = model(game)
        model.receive(["event": "download_queue", "payload": [
            "steamid": "100", "revision": 1, "items": [["appid": 570, "state": "downloading", "platform": "windows"]]
        ]])
        #expect(model.downloads[570]?.platform == .windows)
        model.receive(["event": "install_complete", "payload": ["appid": 570, "install_dir": "/fixture/game"]])
        #expect(model.games[0].installedVersion == .windows)
        #expect(model.games[0].supportsMacOS)
        let legacy = try #require(GameDownload(queueJSON: ["appid": 570, "state": "paused"]))
        #expect(legacy.platform == nil)
    }

    @Test func failedSendLeavesPickerAvailableAndDoesNotInventQueuedDownload() throws {
        let game = game()
        let model = model(game)
        model.commandSink = { _, _ in nil }
        model.install(game)
        let request = try #require(model.installationRequest)
        #expect(!model.confirmInstallation(.macOS, requestID: request.id))
        #expect(model.installationRequest?.id == request.id)
        #expect(model.downloads.isEmpty)
        #expect(model.pendingEnqueueGames.isEmpty)
    }
}
