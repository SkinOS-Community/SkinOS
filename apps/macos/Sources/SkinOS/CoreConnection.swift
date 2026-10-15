import Foundation

/// NDJSON transport. Only structured events reach the UI. Credentials are never logged.
final class CoreConnection {
    var onEvent: (([String: Any]) -> Void)?
    var onExit: ((Int32) -> Void)?
    private var process: Process?
    private var input: FileHandle?
    private let readerQueue = DispatchQueue(label: "app.skinos.core.events", qos: .userInitiated)
    private let diagnosticsQueue = DispatchQueue(label: "app.skinos.core.diagnostics", qos: .utility)

    func start() throws {
        guard process == nil else { return }
        let task = Process()
        let executable = try executableURL()
        task.executableURL = executable
        // The core owns all state paths and credentials; UI never reads Steam account files.
        let inbound = Pipe(), outbound = Pipe(), errors = Pipe()
        task.standardInput = inbound
        task.standardOutput = outbound
        task.standardError = errors
        task.terminationHandler = { [weak self] task in
            DispatchQueue.main.async { self?.onExit?(task.terminationStatus) }
        }
        try task.run()
        process = task
        input = inbound.fileHandleForWriting
        readerQueue.async { [weak self] in
            var pending = Data()
            let output = outbound.fileHandleForReading
            while true {
                let chunk = output.availableData
                if chunk.isEmpty { break }
                pending.append(chunk)
                // Bound incomplete lines without limiting legitimate large library snapshots.
                if pending.count > 32 * 1024 * 1024 { pending.removeAll(keepingCapacity: false) }
                while let newline = pending.firstIndex(of: 0x0A) {
                    let line = pending.prefix(upTo: newline)
                    pending.removeSubrange(...newline)
                    guard let event = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else { continue }
                    DispatchQueue.main.async { self?.onEvent?(event) }
                }
            }
        }
        diagnosticsQueue.async {
            // Drain to prevent child pipe blockage. Do not surface raw helper output: it
            // may contain sensitive authentication details from a third-party dependency.
            while !errors.fileHandleForReading.availableData.isEmpty {}
        }
    }

    @discardableResult
    func send(_ command: String, payload: [String: Any] = [:]) -> String? {
        guard let input, process?.isRunning == true else { return nil }
        let id = UUID().uuidString
        guard var data = try? JSONSerialization.data(withJSONObject: [
            "id": id, "command": command, "payload": payload
        ]) else { return nil }
        data.append(0x0A)
        do { try input.write(contentsOf: data); return id } catch { return nil }
    }

    func stop() {
        guard let task = process else { return }
        send("quit")
        try? input?.close()
        input = nil
        process = nil
        DispatchQueue.global(qos: .utility).asyncAfter(deadline: .now() + 2) {
            if task.isRunning { task.terminate() }
        }
    }

    private func executableURL() throws -> URL {
        let environment = ProcessInfo.processInfo.environment
        let candidates = [
            environment["SKINOS_CORE_PATH"].map { URL(fileURLWithPath: $0) },
            Bundle.main.executableURL?.deletingLastPathComponent().appendingPathComponent("skinos-core"),
            URL(fileURLWithPath: FileManager.default.currentDirectoryPath).appendingPathComponent("target/debug/skinos-core")
        ].compactMap { $0 }
        guard let executable = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0.path) }) else {
            throw NSError(domain: "SkinOS", code: 1, userInfo: [NSLocalizedDescriptionKey:
                "Не найден модуль SkinOS Core. Соберите приложение скриптом scripts/build-app.sh."])
        }
        return executable
    }
}
