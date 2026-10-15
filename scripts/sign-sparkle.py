#!/usr/bin/env python3
"""Validate and sign SkinOS's pinned Sparkle framework, from the inside out.

Never uses --deep for signing. The original entitlement dictionaries are read
before any mutation and preserved independently for each component. No extra
permissions or sandbox exceptions are introduced.
"""
import argparse
import json
import os
import pathlib
import plistlib
import stat
import subprocess
import tempfile

FRAMEWORK_PATH = "Contents/Frameworks/Sparkle.framework"
SPARKLE_VERSION = "2.10.0"
NATIVE_PATHS = tuple(FRAMEWORK_PATH + "/" + path for path in (
    "Versions/B/Autoupdate",
    "Versions/B/Updater.app/Contents/MacOS/Updater",
    "Versions/B/XPCServices/Downloader.xpc/Contents/MacOS/Downloader",
    "Versions/B/XPCServices/Installer.xpc/Contents/MacOS/Installer",
    "Versions/B/Sparkle",
))
SIGNING_PATHS = tuple(FRAMEWORK_PATH + "/" + path for path in (
    "Versions/B/Autoupdate",
    "Versions/B/Updater.app",
    "Versions/B/XPCServices/Downloader.xpc",
    "Versions/B/XPCServices/Installer.xpc",
)) + (FRAMEWORK_PATH,)
MACHO_MAGICS = {
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe", b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca", b"\xca\xfe\xba\xbf", b"\xbf\xba\xfe\xca",
}


def validate_framework(app: pathlib.Path) -> dict:
    app = app.resolve(strict=True)
    framework = app / FRAMEWORK_PATH
    if framework.is_symlink() or not framework.is_dir():
        raise ValueError("Missing or symlinked Sparkle.framework")
    for directory in (app / "Contents", app / "Contents/Frameworks", framework / "Versions", framework / "Versions/B"):
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError("Unsafe framework ancestor: " + str(directory))
    native = set()
    executables = set()
    symlinks = {}
    for path in framework.rglob("*"):
        relative = str(path.relative_to(app))
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            target = os.readlink(path)
            if pathlib.Path(target).is_absolute():
                raise ValueError("Absolute framework symlink: " + relative)
            if not path.resolve(strict=True).is_relative_to(framework):
                raise ValueError("Escaping framework symlink: " + relative)
            symlinks[relative] = target
        elif stat.S_ISREG(mode):
            if mode & 0o111:
                executables.add(relative)
            with path.open("rb") as source:
                if source.read(4) in MACHO_MAGICS:
                    if path.stat().st_nlink != 1:
                        raise ValueError("Hard-linked framework code: " + relative)
                    native.add(relative)
        elif not stat.S_ISDIR(mode):
            raise ValueError("Unexpected special framework file: " + relative)
    expected = set(NATIVE_PATHS)
    if native != expected:
        raise ValueError("Unexpected Mach-O inventory: " + repr(sorted(native.symmetric_difference(expected))))
    if executables != expected:
        raise ValueError("Unexpected executable inventory: " + repr(sorted(executables.symmetric_difference(expected))))
    info_path = framework / "Versions/B/Resources/Info.plist"
    with info_path.open("rb") as source:
        info = plistlib.load(source)
    if info.get("CFBundleShortVersionString") != SPARKLE_VERSION:
        raise ValueError("Unexpected Sparkle version; update the audited pin before signing")
    return {"version": SPARKLE_VERSION, "native_paths": list(NATIVE_PATHS), "symlinks": symlinks}


def read_entitlements(path: pathlib.Path) -> dict:
    result = subprocess.run(["codesign", "--display", "--entitlements", "-", "--xml", str(path)],
                            capture_output=True, check=True)
    if not result.stdout.strip():
        return {}
    value = plistlib.loads(result.stdout)
    if not isinstance(value, dict):
        raise ValueError("Unexpected entitlement format: " + str(path))
    return value


def sign_framework(app: pathlib.Path, identity: str) -> dict:
    app = app.resolve(strict=True)
    before = validate_framework(app)
    entitlements = {path: read_entitlements(app / path) for path in SIGNING_PATHS}
    if identity != "-" and any(value.get("com.apple.security.get-task-allow") for value in entitlements.values()):
        raise ValueError("Refusing to distribute framework code with get-task-allow")
    with tempfile.TemporaryDirectory(prefix="skinos-sparkle-entitlements-") as directory:
        for index, relative in enumerate(SIGNING_PATHS):
            args = ["codesign", "--force", "--sign", identity, "--options", "runtime",
                    "--timestamp=none" if identity == "-" else "--timestamp"]
            # Empty dictionaries need no entitlement blob. Keeping the others
            # byte-equivalent as dictionaries preserves optional sandbox policy.
            if entitlements[relative]:
                entitlement_file = pathlib.Path(directory) / f"{index}.plist"
                entitlement_file.write_bytes(plistlib.dumps(entitlements[relative]))
                args += ["--entitlements", str(entitlement_file)]
            subprocess.run(args + [str(app / relative)], check=True)
    after = validate_framework(app)
    if before != after:
        raise ValueError("Framework paths or symlinks changed while signing")
    for relative in SIGNING_PATHS:
        if read_entitlements(app / relative) != entitlements[relative]:
            raise ValueError("Framework entitlements changed: " + relative)
        subprocess.run(["codesign", "--verify", "--strict", str(app / relative)], check=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app / FRAMEWORK_PATH)], check=True)
    return dict(after, ad_hoc=identity == "-", entitlements_preserved=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", required=True, type=pathlib.Path)
    parser.add_argument("--identity", required=True, help="Developer ID identity or - for ad-hoc development")
    parser.add_argument("--check-only", action="store_true", help="Validate inventory without modifying signatures")
    args = parser.parse_args()
    app = args.app.resolve(strict=True)
    if not app.is_dir():
        raise SystemExit("--app must be an existing application directory")
    report = validate_framework(app) if args.check_only else sign_framework(app, args.identity)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
