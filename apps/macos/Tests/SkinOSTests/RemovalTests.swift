import Foundation
import Testing
@testable import SkinOS

@MainActor
struct RemovalTests {
    private func model() -> AppModel {
        let model = AppModel()
        model.account = SteamAccount(id: "100", accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        model.coreReady = true
        model.games = [SteamGame(json: ["appid": 42, "name": "Fixture", "installed_dir": "/fixture/game", "installed_build_id": "1", "installed_size_bytes": 1024, "installed_platform": "windows", "windows_supported": true])!]
        return model
    }
    private func queue(_ revision: Int, rows: [[String: Any]], id: String? = nil) -> [String: Any] {
        var event: [String: Any] = ["event": "download_queue", "payload": ["steamid": "100", "revision": revision, "max_concurrent": 2, "items": rows]]
        if let id { event["id"] = id }
        return event
    }

    @Test func removalPreviewIncludesEveryDeletedPathAndPreservesSharedDirectoryNotice() {
        let payload: [String: Any] = ["appid": 42, "install_dir": "/games/current", "additional_install_dirs": ["/games/legacy"],
            "partial_dirs": ["/downloads/42"], "remove_bytes": 2048, "shared_appids": [70], "confirmation_token": "synthetic-token"]
        let plan = UninstallPlan(json: payload, requestID: "preview")!
        #expect(plan.confirmationText.contains("Общая папка нужна другим играм и останется."))
        #expect(plan.confirmationText.contains("/games/current"))
        #expect(plan.confirmationText.contains("/games/legacy"))
        #expect(plan.confirmationText.contains("/downloads/42"))
        #expect(plan.confirmationText.contains("безвозвратно"))
        #expect(!plan.confirmationText.contains("synthetic-token"))
        #expect(!plan.confirmationText.contains("Корзин"))
        var missingToken = payload; missingToken.removeValue(forKey: "confirmation_token")
        #expect(UninstallPlan(json: missingToken, requestID: "preview") == nil)
    }

    @Test func previewMustMatchRequestedGameAndDoesNotChangeInstallation() {
        let model = model()
        model.uninstallPlanRequests["preview"] = 42
        model.loadingUninstallPlans.insert(42)
        model.requestCommands["preview"] = "get_uninstall_plan"
        let payload: [String: Any] = ["appid": 42, "install_dir": "/fixture/game", "remove_bytes": 1024, "confirmation_token": "synthetic-token"]
        model.receive(["id": "preview", "event": "uninstall_plan", "payload": payload])
        #expect(model.uninstallPlans[42]?.confirmationToken == "synthetic-token")
        #expect(model.games[0].isInstalled)
        #expect(model.loadingUninstallPlans.isEmpty)
        #expect(model.uninstallingGames.isEmpty)
        #expect(model.requestCommands["preview"] == nil)
        model.uninstallPlanRequests["wrong"] = 7
        model.loadingUninstallPlans.insert(7)
        model.receive(["id": "wrong", "event": "uninstall_plan", "payload": payload])
        #expect(model.uninstallPlans[7] == nil)
        #expect(model.games[0].isInstalled)
    }

    @Test func activeRemovalWaitsForWorkerDrainAndDoesNotUninstallGame() {
        let model = model()
        model.receive(queue(1, rows: [["appid": 42, "state": "downloading", "queue_position": 0]]))
        model.downloadControlRequests["remove"] = 42
        model.requestCommands["remove"] = "remove_download"
        model.pendingDownloadControls.insert(42)
        model.receive(queue(2, rows: [["appid": 42, "state": "cancelling", "queue_position": 0]]))
        #expect(model.downloads[42]?.phase == "cancelling")
        #expect(model.pendingDownloadControls.contains(42))
        model.receive(queue(3, rows: [], id: "remove"))
        #expect(model.downloads.isEmpty)
        #expect(model.downloadOrder.isEmpty)
        #expect(model.pendingDownloadControls.isEmpty)
        #expect(model.downloadControlRequests.isEmpty)
        #expect(model.games[0].isInstalled)
    }

    @Test func failedQueueRemovalKeepsRecordAndAllowsRetry() {
        let model = model()
        model.receive(queue(1, rows: [["appid": 42, "state": "paused", "queue_position": 0]]))
        model.downloadControlRequests["remove"] = 42
        model.pendingDownloadControls.insert(42)
        model.receive(["id": "remove", "event": "error", "payload": ["message": "Cannot save queue"]])
        #expect(model.downloads[42]?.phase == "paused")
        #expect(model.pendingDownloadControls.isEmpty)
        #expect(model.downloadQueueError == "Cannot save queue")
        #expect(model.games[0].isInstalled)
    }

    @Test func uninstallBlockedByRunningOrUnfinishedDownload() {
        let model = model()
        let game = model.games[0]
        #expect(model.canUninstall(game))
        model.launchPhases[42] = "running"
        #expect(!model.canUninstall(game))
        model.launchPhases[42] = "exited"
        for state in ["queued", "preparing", "downloading", "paused", "pausing", "cancelling"] {
            model.downloads[42] = GameDownload(queueJSON: ["appid": 42, "state": state])!
            #expect(!model.canUninstall(game))
        }
        model.downloads[42] = GameDownload(queueJSON: ["appid": 42, "state": "completed"])!
        #expect(model.canUninstall(game))
        model.uninstallingGames.insert(42)
        #expect(!model.canUninstall(game))
    }

    @Test func correlatedUninstallClearsInstallationOnlyAfterSuccess() {
        let model = model()
        model.uninstallRequests["uninstall"] = 42
        model.requestCommands["uninstall"] = "uninstall_game"
        model.uninstallingGames.insert(42)
        model.receive(["id": "uninstall", "event": "game_uninstalled", "payload": ["appid": 42, "shared_directory_retained": true]])
        #expect(!model.games[0].isInstalled)
        #expect(model.uninstallNotices[42] == "Игра удалена из установленных. Общая папка сохранена для других игр.")
        #expect(model.games[0].installedBuildID == nil)
        #expect(model.games[0].installedSizeBytes == nil)
        #expect(model.games[0].installedPlatform == nil)
        #expect(model.uninstallingGames.contains(42))
        #expect(model.games.count == 1)
        model.receive(["id": "uninstall", "event": "operation_complete", "payload": ["appid": 42]])
        #expect(model.uninstallingGames.isEmpty)
        #expect(model.uninstallRequests.isEmpty)
        #expect(model.requestCommands["uninstall"] == nil)
    }

    @Test func failedOrMismatchedUninstallNeverChangesInstalledState() {
        let model = model()
        model.uninstallRequests["failed"] = 42
        model.uninstallingGames.insert(42)
        model.receive(["id": "failed", "event": "error", "payload": ["message": "Game is running"]])
        #expect(model.games[0].isInstalled)
        #expect(model.uninstallingGames.isEmpty)
        model.uninstallRequests["mismatch"] = 42
        model.uninstallingGames.insert(42)
        model.receive(["id": "mismatch", "event": "game_uninstalled", "payload": ["appid": 7]])
        #expect(model.games[0].isInstalled)
        #expect(model.uninstallingGames.isEmpty)
        model.uninstallRequests["old-account"] = 42
        model.resetCommunity()
        model.receive(["id": "old-account", "event": "game_uninstalled", "payload": ["appid": 42]])
        #expect(model.games[0].isInstalled)
    }
}
