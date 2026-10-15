import Foundation
import Testing
@testable import SkinOS

@MainActor
struct AppUpdatesTests {
    private var validInfo: [String: Any] {
        ["SUFeedURL": "https://github.com/example/client/releases/latest/download/appcast.xml",
         "SUPublicEDKey": Data(repeating: 42, count: 32).base64EncodedString(),
         "SUVerifyUpdateBeforeExtraction": true, "SURequireSignedFeed": true,
         "SUSignedFeedFailureExpirationInterval": 0, "SUAllowsAutomaticUpdates": false]
    }

    @Test func onlyCompleteSignedHTTPSConfigurationEnablesUpdates() {
        #expect(AppUpdateConfiguration(info: validInfo) != nil)
        for key in validInfo.keys {
            var info = validInfo
            info.removeValue(forKey: key)
            #expect(AppUpdateConfiguration(info: info) == nil)
        }
        for feed in ["http://example.com/appcast.xml", "file:///tmp/appcast.xml", "https://user:pass@example.com/feed", "https://example.com/feed#fragment"] {
            var info = validInfo; info["SUFeedURL"] = feed
            #expect(AppUpdateConfiguration(info: info) == nil)
        }
        for key in ["not a key", Data(repeating: 0, count: 31).base64EncodedString()] {
            var info = validInfo; info["SUPublicEDKey"] = key
            #expect(AppUpdateConfiguration(info: info) == nil)
        }
        var info = validInfo; info["SUSignedFeedFailureExpirationInterval"] = 1728000
        #expect(AppUpdateConfiguration(info: info) == nil)
    }

    @Test func installationWaitsForGameAndPendingFileOperations() {
        let model = AppModel()
        model.coreReady = true
        #expect(model.appUpdateBlockReason == nil)
        for phase in ["authenticating", "authenticated", "starting", "running", "stopping", "finishing"] {
            model.launchPhases = [10: phase]
            #expect(model.appUpdateBlockReason != nil)
        }
        model.launchPhases = [10: "exited"]
        for phase in ["queued", "preparing", "verifying", "downloading", "installing", "pausing", "cancelling"] {
            model.downloads = [10: GameDownload(id: 10, phase: phase)]
            #expect(model.appUpdateBlockReason != nil)
        }
        model.downloads = [10: GameDownload(id: 10, phase: "paused")]
        #expect(model.appUpdateBlockReason == nil)
        model.pendingEnqueueGames.insert(20)
        #expect(model.appUpdateBlockReason != nil)
        model.pendingEnqueueGames = []
        model.uninstallingGames.insert(30)
        #expect(model.appUpdateBlockReason != nil)
        model.uninstallingGames = []
        model.coreReady = false
        #expect(model.appUpdateBlockReason != nil)
    }

    @Test func deferredInstallNeedsExplicitRetryAndRechecksActivity() {
        let model = AppModel()
        model.coreReady = true
        model.launchPhases = [10: "running"]
        let updates = AppUpdates(model: model)
        var installs = 0
        #expect(updates.shouldAllowApplicationTermination()) // ordinary quit is unchanged
        #expect(updates.postponeInstallationIfBusy { installs += 1 })
        #expect(updates.pendingInstallation)
        #expect(!updates.shouldAllowApplicationTermination())
        updates.installPendingUpdate()
        #expect(installs == 0)
        model.launchPhases = [10: "exited"]
        #expect(installs == 0) // becoming idle never restarts the app by itself
        model.downloads = [20: GameDownload(id: 20, phase: "downloading")]
        updates.installPendingUpdate()
        #expect(installs == 0)
        model.downloads[20]?.phase = "paused"
        updates.installPendingUpdate()
        updates.installPendingUpdate()
        #expect(installs == 1)
        #expect(!updates.pendingInstallation)
        #expect(updates.shouldAllowApplicationTermination())
        model.launchPhases[10] = "starting"
        #expect(!updates.shouldAllowApplicationTermination()) // final termination race guard
    }
}
