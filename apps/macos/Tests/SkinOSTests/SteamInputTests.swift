import Foundation
import Testing
@testable import SkinOS

@MainActor
struct SteamInputTests {
    private let owner = "76561198000000000"
    private func game(_ id: Int = 629520) -> SteamGame {
        SteamGame(json: ["appid": id, "name": "Input fixture", "windows_supported": true,
                        "macos_supported": false, "installed_dir": "/fixture/game", "installed_platform": "windows"])!
    }
    private func model() -> AppModel {
        let model = AppModel()
        model.account = SteamAccount(id: owner, accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        model.coreReady = true; model.steamConnected = true
        model.commandSink = { command, _ in command + "-fixture" }
        return model
    }
    private func report(fixture: Bool = false, count: Int = 0) -> [String: Any] {
        ["appid": 629520, "steamid": owner, "fixture": fixture,
         "summary": ["api_initialized": true, "identity_verified": true, "scan_completed": true,
                     "controller_seen": count > 0, "max_controllers": count, "xinput_route_observed": false,
                     "steam_input_activity_verified": false, "physical_device_verified": false],
         "samples": []]
    }

    @Test func zeroControllerReportDoesNotImplyPhysicalSuccessAndWaitsForDrain() {
        let model = model()
        model.diagnoseSteamInput(for: game())
        #expect(model.steamInputBusy)
        let id = "diagnose_input-fixture"
        model.receive(["event": "steam_input_status", "id": id,
                       "payload": ["appid": 629520, "state": "authenticating", "steamid": NSNull()]])
        #expect(model.steamInputErrors[629520] == nil)
        model.receive(["event": "steam_input_diagnostics", "id": id, "payload": report()])
        #expect(model.steamInputReports[629520]?.controllerDescription == "Контроллеры не подключены")
        #expect(model.steamInputReports[629520]?.physicalDeviceVerified == false)
        #expect(model.steamInputReports[629520]?.apiInitialized == true)
        #expect(model.steamInputBusy)
        model.receive(["event": "operation_complete", "id": id, "payload": [:]])
        #expect(!model.steamInputBusy)
        #expect(model.steamInputErrors[629520] == nil)
        #expect(model.requestCommands[id] == nil)
    }

    @Test func gameAndConcurrentScanBlockAnotherDiagnosticAndLaunch() {
        let model = model()
        model.launchPhases[42] = "running"
        model.diagnoseSteamInput(for: game())
        #expect(!model.steamInputBusy)
        #expect(model.steamInputBlockReason(for: game()) == "Закройте запущенную игру перед проверкой.")
        model.launchPhases = [:]
        model.diagnoseSteamInput(for: game())
        model.diagnoseSteamInput(for: game(42))
        #expect(model.steamInputRequests.count == 1)
        model.launch(game(42))
        #expect(model.launchPhases[42] == nil)
    }

    @Test func errorsStayLocalAndMalformedReportNeverBecomesSuccessful() {
        let model = model()
        model.diagnoseSteamInput(for: game())
        let id = "diagnose_input-fixture"
        model.receive(["event": "error", "id": id, "payload": ["code": "operation_failed", "message": "SDK fixture failed"]])
        #expect(model.steamInputErrors[629520] == "SDK fixture failed")
        #expect(model.errorMessage == nil)
        #expect(!model.steamInputBusy)
        model.diagnoseSteamInput(for: game())
        model.receive(["event": "steam_input_diagnostics", "id": id, "payload": ["appid": 629520, "steamid": owner, "summary": [:]]])
        model.receive(["event": "operation_complete", "id": id, "payload": [:]])
        #expect(model.steamInputReports[629520] == nil)
        #expect(model.steamInputErrors[629520] != nil)
        #expect(!model.steamInputBusy)
    }

    @Test func wrongAccountAndWrongGameCannotReplaceReportAndLogoutDiscardsPending() {
        let model = model()
        model.diagnoseSteamInput(for: game())
        var wrong = report(); wrong["steamid"] = "76561198000000001"
        model.receive(["event": "steam_input_diagnostics", "id": "diagnose_input-fixture", "payload": wrong])
        #expect(model.steamInputReports.isEmpty)
        #expect(model.steamInputErrors[629520] != nil)
        wrong = report(); wrong["appid"] = 42
        model.receive(["event": "steam_input_diagnostics", "id": "diagnose_input-fixture", "payload": wrong])
        #expect(model.steamInputReports.isEmpty)
        model.resetCommunity()
        #expect(!model.steamInputBusy)
        model.receive(["event": "steam_input_diagnostics", "id": "diagnose_input-fixture", "payload": report()])
        #expect(model.steamInputReports.isEmpty)
        #expect(model.steamInputErrors.isEmpty)
    }

    @Test func fixtureCannotVerifyPhysicalDeviceAndMappingComesFromObservedRoutes() {
        var payload = report(fixture: true, count: 1)
        var summary = payload["summary"] as! [String: Any]
        summary["physical_device_verified"] = true
        summary["xinput_route_observed"] = true
        payload["summary"] = summary
        payload["samples"] = [["event": "steam_input_snapshot", "controllers": [
            ["handle": "123", "gamepad_index": 0, "binding_revision_available": true, "binding_major": 4, "binding_minor": 2]], "xinput": [
                ["steam_input_handle": "123", "slot": 0, "routed": true]]]]
        let parsed = SteamInputReport(json: payload)
        #expect(parsed?.fixture == true)
        #expect(parsed?.physicalDeviceVerified == false)
        #expect(parsed?.devices.first?.mappingDescription == "Steam Input → XInput 1")
        #expect(parsed?.devices.first?.bindingRevisionDescription == "Ревизия раскладки API: 4.2")
        #expect(parsed?.controllerDescription.contains("Тестовые данные") == true)
    }

    @Test func unavailableRevisionDoesNotInventZeroRevisionOrMissingLayout() {
        for revisionFields: [String: Any] in [
            ["binding_revision_available": false, "binding_major": NSNull(), "binding_minor": NSNull()],
            ["binding_loaded": false, "binding_major": 0, "binding_minor": 0],
            ["binding_revision_available": false, "binding_loaded": true, "binding_major": 3, "binding_minor": 7]
        ] {
            var payload = report(count: 1)
            var controller: [String: Any] = ["handle": "123", "gamepad_index": 0]
            controller.merge(revisionFields) { _, new in new }
            payload["samples"] = [["event": "steam_input_snapshot", "controllers": [controller]]]
            let device = SteamInputReport(json: payload)?.devices.first
            #expect(device?.bindingRevisionAvailable == false)
            #expect(device?.bindingMajor == nil)
            #expect(device?.bindingMinor == nil)
            #expect(device?.bindingRevisionDescription == "Ревизия не предоставлена")
        }
    }

    @Test func historicalRevisionAvailabilityStillReadsActualNumbers() {
        var payload = report(count: 1)
        payload["samples"] = [["event": "steam_input_snapshot", "controllers": [
            ["handle": "123", "gamepad_index": 0, "binding_loaded": true, "binding_major": 3, "binding_minor": 7]]]]
        let device = SteamInputReport(json: payload)?.devices.first
        #expect(device?.bindingRevisionAvailable == true)
        #expect(device?.bindingRevisionDescription == "Ревизия раскладки API: 3.7")
    }

    @Test func savedGameSettingsInvalidateCompletedReportButIdenticalRefreshDoesNot() {
        let model = model()
        model.diagnoseSteamInput(for: game())
        let id = "diagnose_input-fixture"
        model.receive(["event": "steam_input_diagnostics", "id": id, "payload": report()])
        model.receive(["event": "operation_complete", "id": id, "payload": [:]])
        model.receive(["event": "game_settings", "payload": ["appid": 629520, "settings": GameSettings().payload]])
        #expect(model.steamInputReports[629520] != nil)
        var changed = GameSettings(); changed.crossoverPath = "/Applications/CrossOver Preview.app"
        model.receive(["event": "game_settings", "payload": ["appid": 629520, "settings": changed.payload]])
        #expect(model.steamInputReports[629520] == nil)
    }

    @Test func steamInputSettingRoundTripsAndInvalidatesOldReport() {
        #expect(!GameSettings(json: [:]).steamInput)
        var changed = GameSettings(); changed.steamInput = true
        #expect(GameSettings(json: changed.payload).steamInput)
        let model = model()
        model.diagnoseSteamInput(for: game())
        model.receive(["event": "steam_input_diagnostics", "id": "diagnose_input-fixture", "payload": report()])
        model.receive(["event": "operation_complete", "id": "diagnose_input-fixture", "payload": [:]])
        #expect(model.steamInputReports[629520] != nil)
        model.receive(["event": "game_settings", "payload": ["appid": 629520, "settings": changed.payload]])
        #expect(model.steamInputReports[629520] == nil)
    }

    @Test func inputFailureDoesNotTerminateOrReclassifyRunningGame() {
        let model = model()
        model.gameRequests["game-input-test"] = 629520
        model.requestCommands["game-input-test"] = "launch_game"
        model.launchPhases[629520] = "running"
        model.receive(["event": "steam_input_status", "id": "game-input-test", "payload": [
            "appid": 629520, "steamid": model.account!.id, "state": "failed",
            "message": "Контроллер отключён", "game_continues": true]])
        #expect(model.launchPhases[629520] == "running")
        #expect(model.gameRequests["game-input-test"] == 629520)
        #expect(model.steamInputErrors[629520] == "Контроллер отключён")
        model.receive(["event": "steam_input_status", "id": "game-input-test", "payload": [
            "appid": 629520, "steamid": "foreign-account", "state": "failed", "message": "Wrong owner"]])
        #expect(model.steamInputErrors[629520] == "Контроллер отключён")
    }

    @Test func reconnectStatusKeepsGameRunningAndDoesNotEraseAnUnrelatedError() {
        let model = model()
        model.gameRequests["game-input-test"] = 629520
        model.requestCommands["game-input-test"] = "launch_game"
        model.launchPhases[629520] = "running"
        model.steamInputErrors[629520] = "Предыдущая ошибка ввода"
        model.errorMessage = "Ошибка загрузки другой игры"
        for state in ["disconnected", "waiting_reconnect", "reconnecting", "reconnected"] {
            model.receive(["event": "steam_input_status", "id": "game-input-test", "payload": [
                "appid": 629520, "steamid": model.account!.id, "state": state,
                "message": state, "game_continues": true]])
            #expect(model.launchPhases[629520] == "running")
            #expect(model.gameRequests["game-input-test"] == 629520)
        }
        #expect(model.steamInputErrors[629520] == nil)
        #expect(model.steamInputMessages[629520] == "reconnected")
        #expect(model.errorMessage == "Ошибка загрузки другой игры")
    }

    @Test func changedGlobalRuntimeRejectsPendingReportEvenAfterSettingsAreRestored() {
        let model = model()
        model.diagnoseSteamInput(for: game())
        let id = "diagnose_input-fixture"
        let original = model.settings
        var changed = original; changed.crossoverPath = "/Applications/CrossOver Preview.app"
        model.receive(["event": "settings", "payload": ["settings": changed.payload, "crossovers": [], "notproton": [:]]])
        model.receive(["event": "settings", "payload": ["settings": original.payload, "crossovers": [], "notproton": [:]]])
        model.receive(["event": "steam_input_diagnostics", "id": id, "payload": report()])
        #expect(model.steamInputReports[629520] == nil)
        #expect(model.steamInputErrors[629520] != nil)
        #expect(model.steamInputBusy)
        model.receive(["event": "operation_complete", "id": id, "payload": [:]])
        #expect(!model.steamInputBusy)
    }

    @Test func runtimeRevisionRefreshInvalidatesCompletedReport() {
        let model = model()
        model.diagnoseSteamInput(for: game())
        let id = "diagnose_input-fixture"
        model.receive(["event": "steam_input_diagnostics", "id": id, "payload": report()])
        model.receive(["event": "operation_complete", "id": id, "payload": [:]])
        model.receive(["event": "settings", "payload": ["settings": model.settings.payload,
            "crossovers": [], "notproton": ["revision": "updated-runtime", "version": "1.2"]]])
        #expect(model.steamInputReports.isEmpty)
        #expect(!model.steamInputBusy)
    }

    @Test func coreExitClearsBusyWithoutInventingResults() {
        let model = model()
        model.diagnoseSteamInput(for: game())
        model.steamInputConnectionLost()
        #expect(!model.steamInputBusy)
        #expect(model.steamInputReports.isEmpty)
        #expect(model.steamInputErrors[629520] != nil)
    }
}
