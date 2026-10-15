"""Pure host selection fixture; never clones a runner or touches a prefix."""
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]


class NotProtonHostTests(unittest.TestCase):
    def test_minimum_os_and_fex_boundary(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = pathlib.Path(directory)
            source = directory / 'Check.swift'
            source.write_text('''
@main struct Check {
    static func main() throws {
        do {
            try HostCompatibility.requireSupportedSystem(14)
            fatalError("macOS 14 must fail before cloning or patching")
        } catch {}
        for major in [15, 25] {
            let flavor = try HostCompatibility.runnerFlavor(buildFlavor: "fex", systemMajor: major)
            precondition(flavor == "rosetta")
        }
        for major in [26, 27] {
            let flavor = try HostCompatibility.runnerFlavor(buildFlavor: "fex", systemMajor: major)
            precondition(flavor == "fex")
        }
        for build: String? in [nil, "rosetta"] {
            let flavor = try HostCompatibility.runnerFlavor(buildFlavor: build, systemMajor: 27)
            precondition(flavor == "rosetta")
        }
    }
}
''')
            binary = directory / 'check'
            subprocess.run(['swiftc', '-O', '-parse-as-library', '-o', str(binary), str(source),
                str(ROOT / 'third_party/notproton/integration/HostCompatibility.swift'),
                str(ROOT / 'third_party/notproton/upstream/app/Sources/NotProtonApp/Support/StepFailure.swift')], check=True)
            subprocess.run([str(binary)], check=True)


if __name__ == '__main__':
    unittest.main()
