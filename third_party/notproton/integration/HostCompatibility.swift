// SkinOS selects the runner without upstream's graphical tool picker.
// Apply the same Sequoia/Tahoe boundary before cloning or patching any runner.
enum HostCompatibility {
    static func requireSupportedSystem(_ major: Int) throws {
        guard major >= 15 else {
            throw StepFailure(step: "Prepare NotProton", detail: "This NotProton version requires macOS 15 or later.")
        }
    }

    static func runnerFlavor(buildFlavor: String?, systemMajor: Int) throws -> String {
        try requireSupportedSystem(systemMajor)
        return buildFlavor == "fex" && systemMajor >= 26 ? "fex" : "rosetta"
    }
}
