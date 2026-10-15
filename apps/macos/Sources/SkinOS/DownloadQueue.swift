import Foundation

extension GameDownload {
    init?(queueJSON json: [String: Any]) {
        guard let appID = json["appid"] as? Int, let state = json["state"] as? String,
              ["queued", "preparing", "verifying", "downloading", "installing", "pausing", "paused", "cancelling", "cancelled", "completed", "failed"].contains(state) else { return nil }
        self.init(id: appID)
        phase = state
        queuePosition = max(0, json["queue_position"] as? Int ?? 0)
        isUpdate = json["is_update"] as? Bool ?? false
        progress = (json["progress"] as? NSNumber)?.doubleValue
        downloadedBytes = max(0, (json["downloaded_bytes"] as? NSNumber)?.int64Value ?? 0)
        totalBytes = max(0, (json["total_bytes"] as? NSNumber)?.int64Value ?? 0)
        bytesPerSecond = (json["bytes_per_second"] as? NSNumber)?.doubleValue ?? 0
        etaSeconds = (json["eta_seconds"] as? NSNumber)?.intValue
        reusedBytes = max(0, (json["reused_bytes"] as? NSNumber)?.int64Value ?? 0)
        processedBytes = max(0, (json["processed_bytes"] as? NSNumber)?.int64Value ?? 0)
        installTotalBytes = max(0, (json["install_total_bytes"] as? NSNumber)?.int64Value ?? 0)
        filesCompleted = max(0, json["files_completed"] as? Int ?? 0)
        totalFiles = max(0, json["total_files"] as? Int ?? 0)
        chunksCompleted = max(0, json["chunks_completed"] as? Int ?? 0)
        totalChunks = max(0, json["total_chunks"] as? Int ?? 0)
        message = json["error"] as? String ?? json["message"] as? String
        complete = state == "completed"
        failed = state == "failed"
    }
}

@MainActor
extension AppModel {
    var orderedDownloads: [GameDownload] {
        let known = downloadOrder.compactMap { downloads[$0] }
        let knownIDs = Set(downloadOrder)
        return known + downloads.values.filter { !knownIDs.contains($0.id) }.sorted { $0.queuePosition < $1.queuePosition }
    }
    var queuedDownloads: [GameDownload] { orderedDownloads.filter { $0.phase == "queued" } }
    var runningDownloadCount: Int { downloads.values.filter(\.isRunning).count }
    var downloadSpeed: String { GameDownload.bytes(Int64(min(downloads.values.filter { $0.phase == "downloading" }.reduce(0) { $0 + ($1.bytesPerSecond.isFinite ? max(0, $1.bytesPerSecond) : 0) }, Double(Int64.max - 1024)))) + "/с" }

    func resetDownloadQueue() {
        downloads = [:]; downloadOrder = []; downloadQueueReady = false; downloadQueueLoading = false
        downloadQueueRevision = -1; downloadQueueError = nil; pendingDownloadControls = []; pendingEnqueueGames = []
        downloadControlRequests = [:]; downloadQueueRequests = []; downloadLimitRequests = []; downloadLimitPending = false
        maxConcurrentDownloads = 2
    }

    func refreshDownloadQueue() {
        guard account != nil, !downloadQueueLoading else { return }
        downloadQueueLoading = true; downloadQueueError = nil
        if let id = send("get_download_queue") { downloadQueueRequests.insert(id) }
        else { downloadQueueLoading = false }
    }

    func controlDownload(_ command: String, appID: Int) {
        guard account != nil, coreReady, !pendingDownloadControls.contains(appID) else { return }
        downloadQueueError = nil
        if let id = send(command, payload: ["appid": appID]) {
            downloadControlRequests[id] = appID; pendingDownloadControls.insert(appID)
        }
    }

    func canReorderDownload(_ appID: Int) -> Bool {
        account != nil && coreReady && downloads[appID]?.phase == "queued"
        && downloads[appID]?.isActive == true && !pendingDownloadControls.contains(appID)
    }

    func moveDownload(_ appID: Int, to position: Int) {
        guard canReorderDownload(appID), downloadOrder.indices.contains(position) else { return }
        downloadQueueError = nil
        if let id = send("move_download", payload: ["appid": appID, "position": position]) {
            downloadControlRequests[id] = appID; pendingDownloadControls.insert(appID)
        }
    }

    func moveDownload(_ appID: Int, by offset: Int) {
        let queued = queuedDownloads
        guard let index = queued.firstIndex(where: { $0.id == appID }), queued.indices.contains(index + offset) else { return }
        moveDownload(appID, to: queued[index + offset].queuePosition)
    }

    func setDownloadLimit(_ limit: Int) {
        guard account != nil, coreReady, (1...4).contains(limit), !downloadLimitPending, limit != maxConcurrentDownloads else { return }
        downloadQueueError = nil
        if let id = send("set_download_limit", payload: ["limit": limit]) {
            downloadLimitRequests.insert(id); downloadLimitPending = true
        }
    }

    func receiveDownloadQueue(_ event: String, payload: [String: Any], requestID: String?) -> Bool {
        if event == "download_queue" {
            guard let account, payload["steamid"] as? String == account.id,
                  let revision = (payload["revision"] as? NSNumber)?.int64Value else { return true }
            acknowledgeDownloadRequest(requestID)
            guard revision > downloadQueueRevision else { return true }
            guard let rows = payload["items"] as? [[String: Any]] else { return true }
            let items = rows.compactMap(GameDownload.init(queueJSON:))
            guard items.count == rows.count, Set(items.map(\.id)).count == items.count else {
                downloadQueueError = "Получена некорректная очередь загрузок. Обновите её."
                return true
            }
            notifyDownloadTransitions(previous: downloads, items: items, revision: revision)
            downloads = Dictionary(uniqueKeysWithValues: items.map { ($0.id, $0) })
            downloadOrder = items.map(\.id)
            maxConcurrentDownloads = min(4, max(1, payload["max_concurrent"] as? Int ?? 2))
            downloadQueueRevision = revision
            downloadQueueReady = true; downloadQueueLoading = false
            return true
        }
        if event == "download_enqueued" {
            if let requestID {
                if let appID = gameRequests[requestID] { pendingEnqueueGames.remove(appID) }
                gameRequests.removeValue(forKey: requestID)
                acknowledgeDownloadRequest(requestID)
            }
            return true
        }
        if event == "error", let requestID {
            guard downloadControlRequests[requestID] != nil || downloadQueueRequests.contains(requestID) || downloadLimitRequests.contains(requestID) else { return false }
            acknowledgeDownloadRequest(requestID)
            downloadQueueError = payload["message"] as? String ?? "Не удалось изменить очередь загрузок."
            return true
        }
        return false
    }

    private func acknowledgeDownloadRequest(_ requestID: String?) {
        guard let requestID else { return }
        if let appID = downloadControlRequests.removeValue(forKey: requestID) {
            pendingDownloadControls.remove(appID)
        }
        downloadQueueRequests.remove(requestID)
        downloadLimitRequests.remove(requestID)
        downloadLimitPending = !downloadLimitRequests.isEmpty
        downloadQueueLoading = !downloadQueueRequests.isEmpty
    }
}
