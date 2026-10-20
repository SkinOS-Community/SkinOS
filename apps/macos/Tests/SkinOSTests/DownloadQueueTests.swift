import Foundation
import Testing
@testable import SkinOS

@MainActor
struct DownloadQueueTests {
    private func model() -> AppModel {
        let model = AppModel()
        model.account = SteamAccount(id: "100", accountName: "fixture", displayName: "Fixture", avatarURL: nil)
        return model
    }
    private func item(_ appID: Int, _ state: String, position: Int = 0) -> [String: Any] {
        ["appid": appID, "state": state, "queue_position": position, "is_update": false, "downloaded_bytes": 200,
         "total_bytes": 1000, "bytes_per_second": 20, "eta_seconds": 40, "reused_bytes": 100,
         "processed_bytes": 600, "install_total_bytes": 2000, "files_completed": 3, "total_files": 10, "progress": 0.2]
    }
    private func queue(_ revision: Int, _ rows: [[String: Any]], steamID: String = "100", requestID: String? = nil) -> [String: Any] {
        var event: [String: Any] = ["event": "download_queue", "payload": ["steamid": steamID, "items": rows, "max_concurrent": 2, "revision": revision]]
        if let requestID { event["id"] = requestID }
        return event
    }

    @Test func authoritativeQueuePreservesOrderAndTerminalStates() {
        let model = model()
        model.receive(queue(1, [item(9, "downloading"), item(5, "queued", position: 1), item(2, "queued", position: 2)]))
        #expect(model.orderedDownloads.map(\.id) == [9, 5, 2])
        #expect(model.runningDownloadCount == 1)
        model.receive(queue(2, [item(9, "completed"), item(2, "queued", position: 1), item(5, "paused", position: 2), item(4, "cancelled", position: 3), item(3, "failed", position: 4)]))
        #expect(model.orderedDownloads.map(\.id) == [9, 2, 5, 4, 3])
        #expect(model.downloads[9]?.complete == true)
        #expect(model.downloads[5]?.canResume == true)
        #expect(model.downloads[4]?.canRetry == true)
        #expect(model.downloads[4]?.isActive == false)
        #expect(model.activeDownloads == 2)
        #expect(model.queuedDownloads.map(\.id) == [2])
    }

    @Test func delayedSnapshotAcknowledgesControlWithoutOverwritingNewerState() {
        let model = model()
        model.receive(queue(10, [item(1, "paused")]))
        model.pendingDownloadControls.insert(1)
        model.downloadControlRequests["pause"] = 1
        model.requestCommands["pause"] = "pause_download"
        model.receive(queue(9, [item(1, "downloading")], requestID: "pause"))
        #expect(model.downloads[1]?.phase == "paused")
        #expect(model.downloadQueueRevision == 10)
        #expect(model.pendingDownloadControls.isEmpty)
        #expect(model.downloadControlRequests.isEmpty)
        #expect(model.requestCommands["pause"] == nil)
    }

    @Test func acceptedEnqueueAndLegacyCompletionAreNotFailedInstalls() {
        let model = model()
        model.receive(queue(1, [item(1, "queued")]))
        model.gameRequests["enqueue"] = 1
        model.requestCommands["enqueue"] = "install_game"
        model.receive(["event": "operation_complete", "id": "enqueue", "payload": ["appid": 1]])
        #expect(model.downloads[1]?.phase == "queued")
        #expect(model.downloads[1]?.failed == false)
        model.gameRequests["ack"] = 1
        model.requestCommands["ack"] = "install_game"
        model.receive(["event": "download_enqueued", "id": "ack", "payload": ["appid": 1, "queue_id": "fixture"]])
        #expect(model.gameRequests["ack"] == nil)
        #expect(model.requestCommands["ack"] == nil)
        #expect(model.downloads[1]?.isActive == true)
    }

    @Test func rejectedEnqueueClearsPendingWithoutInventingQueueRow() {
        let model = model()
        model.receive(queue(1, []))
        model.pendingEnqueueGames.insert(42)
        model.gameRequests["enqueue"] = 42
        model.requestCommands["enqueue"] = "install_game"
        model.receive(["event": "error", "id": "enqueue", "payload": ["code": "enqueue_failed", "message": "Not available"]])
        #expect(model.pendingEnqueueGames.isEmpty)
        #expect(model.downloads.isEmpty)
        #expect(model.gameRequests["enqueue"] == nil)
        #expect(model.errorMessage == "Not available")
    }

    @Test func legacyProgressDoesNotRevivePausedQueueEntry() {
        let model = model()
        model.receive(queue(4, [item(1, "paused")]))
        model.receive(["event": "install_progress", "payload": ["appid": 1, "phase": "downloading", "downloaded_bytes": 900, "progress": 0.9]])
        #expect(model.downloads[1]?.phase == "paused")
        #expect(model.downloads[1]?.downloadedBytes == 200)
        model.receive(["event": "install_complete", "payload": ["appid": 1, "install_dir": "/fixture"]])
        #expect(model.downloads[1]?.phase == "paused")
    }

    @Test func accountSwitchDiscardsOldQueueAndRevision() {
        let model = model()
        model.receive(queue(20, [item(1, "downloading")]))
        model.receive(["event": "status", "payload": ["state": "signed_out"]])
        #expect(model.downloads.isEmpty)
        model.receive(queue(21, [item(1, "completed")]))
        #expect(model.downloads.isEmpty)
        model.account = SteamAccount(id: "200", accountName: "another", displayName: "Another", avatarURL: nil)
        model.receive(queue(22, [item(1, "completed")]))
        #expect(model.downloads.isEmpty)
        model.receive(queue(1, [item(2, "paused")], steamID: "200"))
        #expect(model.downloads.keys.sorted() == [2])
        #expect(model.downloadQueueRevision == 1)
    }

    @Test func stoppedProgressMatchesDisplayedCountersAndHasNoTransferRate() {
        for state in ["queued", "paused", "failed", "cancelled"] {
            var payload = item(1, state)
            payload["progress"] = 0.8 // Last verification report, not network progress.
            let download = GameDownload(queueJSON: payload)!
            #expect(download.displayProgress == 0.2)
            #expect(download.byteDescription.hasPrefix("Скачано:"))
            #expect(!download.isNetworkPhase)
        }
        var diskOnly = item(1, "paused")
        diskOnly["total_bytes"] = 0
        let paused = GameDownload(queueJSON: diskOnly)!
        #expect(paused.displayProgress == 0.3)
        #expect(paused.byteDescription.hasPrefix("Обработано:"))
        #expect(paused.canResume && paused.canCancel && !paused.canPause)
    }

    @Test func reorderRequiresLiveConnectionAndWaitingJob() {
        let model = model()
        model.receive(queue(1, [item(1, "queued"), item(2, "downloading", position: 1), item(3, "paused", position: 2)]))
        #expect(!model.canReorderDownload(1))
        model.coreReady = true
        #expect(model.canReorderDownload(1))
        #expect(!model.canReorderDownload(2))
        #expect(!model.canReorderDownload(3))
        model.pendingDownloadControls.insert(1)
        #expect(!model.canReorderDownload(1))
    }

    @Test func verificationUsesDiskWorkInsteadOfNetworkProgress() {
        let verifying = GameDownload(queueJSON: item(1, "verifying"))!
        #expect(verifying.displayProgress == 0.3)
        #expect(verifying.byteDescription.contains("Проверено"))
        #expect(!verifying.byteDescription.contains("Определяем"))
        let downloading = GameDownload(queueJSON: item(1, "downloading"))!
        #expect(downloading.displayProgress == 0.2)
        #expect(downloading.canPause)
        let cancelling = GameDownload(queueJSON: item(1, "cancelling"))!
        #expect(!cancelling.canCancel)
        #expect(!cancelling.canPause)
    }

    @Test func initialFailureAndSuccessfulEmptyQueueAreDifferentStates() {
        let model = model()
        model.commandSink = { _, _ in "refresh" }
        #expect(model.downloadFeedState == .initial)
        model.refreshDownloadQueue()
        #expect(model.downloadFeedState == .loading)
        model.receive(["event": "error", "id": "refresh",
                       "payload": ["code": "download_queue_failed", "message": "Fixture request failure"]])
        #expect(model.downloadFeedState == .failed)
        #expect(!model.downloadQueueReady)
        #expect(model.downloadQueueRequests.isEmpty)
        model.refreshDownloadQueue()
        #expect(model.downloadFeedState == .loading)
        #expect(model.downloadQueueError != nil)
        model.receive(queue(1, [], requestID: "refresh"))
        #expect(model.downloadFeedState == .empty)
        #expect(model.downloadQueueError == nil)
        #expect(model.downloadQueueReady)
    }

    @Test func failedRefreshKeepsRowsAndEqualRevisionSuccessClearsStaleness() {
        let model = model()
        model.commandSink = { _, _ in "refresh" }
        model.receive(queue(10, [item(42, "paused")]))
        model.refreshDownloadQueue()
        #expect(model.downloadFeedState == .refreshing)
        model.receive(["event": "error", "id": "refresh", "payload": ["message": "Fixture timeout"]])
        #expect(model.downloadFeedState == .stale)
        #expect(model.downloads[42]?.phase == "paused")
        model.refreshDownloadQueue()
        #expect(model.downloadFeedState == .refreshing)
        #expect(model.downloadQueueError != nil)
        model.receive(queue(10, [item(42, "paused")], requestID: "refresh"))
        #expect(model.downloadFeedState == .content)
        #expect(model.downloadQueueError == nil)
        #expect(model.downloadQueueRevision == 10)
        #expect(model.downloads[42]?.phase == "paused")
    }

    @Test func malformedQueueCannotEraseCachedRowsOrCompleteFirstLoad() {
        let invalidPayloads: [[String: Any]] = [
            ["steamid": "100", "revision": 2],
            ["steamid": "100", "items": []],
            ["steamid": "100", "revision": 2, "items": [["appid": 42, "state": "unknown"]]],
            ["steamid": "100", "revision": 2, "items": [item(42, "paused"), item(42, "queued")]]
        ]
        for populated in [false, true] {
            for payload in invalidPayloads {
                let model = model()
                if populated { model.receive(queue(1, [item(42, "paused")])) }
                model.commandSink = { _, _ in "refresh" }
                model.refreshDownloadQueue()
                model.receive(["event": "download_queue", "id": "refresh", "payload": payload])
                #expect(model.downloadFeedState == (populated ? .stale : .failed))
                #expect(model.downloadQueueRequests.isEmpty)
                #expect(model.downloads[42]?.phase == (populated ? "paused" : nil))
                #expect(model.downloadQueueReady == populated)
            }
        }
    }

    @Test func staleSnapshotAndStartingControlDoNotHideRefreshFailure() {
        let model = model()
        model.coreReady = true
        model.receive(queue(10, [item(42, "paused")]))
        model.downloadQueueError = "Fixture refresh failure"
        model.commandSink = { command, _ in command }
        model.controlDownload("resume_download", appID: 42)
        #expect(model.downloadFeedState == .stale)
        model.refreshDownloadQueue()
        model.receive(queue(9, [item(42, "downloading")], requestID: "get_download_queue"))
        #expect(model.downloadFeedState == .stale)
        #expect(model.downloads[42]?.phase == "paused")
        #expect(model.downloadQueueError != nil)
    }

    @Test func failedQueueSendDoesNotLookLikeSuccessfulEmptyLoad() {
        let model = model()
        model.commandSink = { _, _ in nil }
        model.refreshDownloadQueue()
        #expect(model.downloadFeedState == .failed)
        #expect(model.downloadQueueRequests.isEmpty)
        #expect(!model.downloadQueueLoading)
        #expect(!model.downloadQueueReady)
    }
}
