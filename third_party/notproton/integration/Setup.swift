// SkinOS adapter. Upstream patching code is compiled unchanged from the pinned tree.
import Foundation

enum PrefixArch { case arm64, x86_64 }
enum RunnerStore { static func installedBuilds() -> [RunnerBuild] { [] } }
struct CrossOverInstall { let crossOverRoot: URL }
enum AppLog { static func note(_ message: String) {} }
enum SupportPaths {
    static var support: URL { URL(filePath: CommandLine.arguments[3]) }
    static var bridge: URL { support.appending(path: "bridge") }
    static var runners: URL { support.appending(path: "runners") }
    static func clonedRoot(forBuild id: String, runners: URL) -> URL {
        runners.appending(path: "crossover-\(id)/CrossOver")
    }
}
extension Bundle {
    static var module: Bundle { Bundle(path: CommandLine.arguments[2])! }
}

@main struct Setup {
    static func main() {
        do { try run() }
        catch { FileHandle.standardError.write(Data((error.localizedDescription + "\n").utf8)); exit(1) }
    }
    static func run() throws {
        guard CommandLine.arguments.count == 4 else {
            throw StepFailure(step: "Prepare NotProton", detail: "Expected CrossOver bundle, resource bundle and support directory.")
        }
        let systemMajor = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
        try HostCompatibility.requireSupportedSystem(systemMajor)
        let bundle = URL(filePath: CommandLine.arguments[1])
        let source = bundle.appending(path: "Contents/SharedSupport/CrossOver")
        let loader = source.appending(path: "lib/wine/x86_64-unix/wine")
        let hash = try Digest.sha256(of: loader)
        guard let build = SupportedRunners.build(loaderSHA256: hash) else {
            throw StepFailure(step: "Prepare NotProton", detail: "This CrossOver build is not supported by the bundled NotProton revision. Update NotProton or choose a supported CrossOver installation.")
        }
        try CrossOverLicense.requireValid(for: CrossOverInstall(crossOverRoot: source))
        let fm = FileManager.default
        let destination = SupportPaths.clonedRoot(forBuild: build.id, runners: SupportPaths.runners)
        if !fm.fileExists(atPath: destination.path) {
            let parent = destination.deletingLastPathComponent()
            let stage = SupportPaths.runners.appending(path: ".\(build.id)-\(UUID().uuidString)")
            try fm.createDirectory(at: stage, withIntermediateDirectories: true)
            defer { try? fm.removeItem(at: stage) }
            // Keep the original application intact. Preserve extended attributes/security state.
            _ = try Shell.check("/bin/cp", ["-c", "-R", source.path, stage.appending(path: "CrossOver").path])
            try fm.moveItem(at: stage, to: parent)
        }
        guard try Digest.sha256(of: Clean.copy(of: destination.appending(path: "lib/wine/x86_64-unix/wine"))) == hash else {
            throw StepFailure(step: "Prepare NotProton", detail: "The owned CrossOver copy does not match its original loader.")
        }
        _ = try NtdllPatcher.stage(build: build, runnerRoot: destination, bridge: SupportPaths.bridge)
        _ = try RunnerPatcher.install(build: build, root: destination, bridge: SupportPaths.bridge)
        let problems = RunnerPatcher.verify(build: build, root: destination, bridge: SupportPaths.bridge)
        guard problems.isEmpty else { throw StepFailure(step: "Verify NotProton", detail: problems.joined(separator: "; ")) }
        let flavor = try HostCompatibility.runnerFlavor(buildFlavor: build.flavor, systemMajor: systemMajor)
        let result: [String: String] = ["build": build.id, "flavor": flavor, "runner_root": destination.path]
        FileHandle.standardOutput.write(try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]))
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
}
