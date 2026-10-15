#!/bin/bash
set -euo pipefail
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-15.0}"

SKINOS_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SKINOS_CONFIGURATION="${SKINOS_CONFIGURATION:-debug}"
export SKINOS_DISTRIBUTION="${SKINOS_DISTRIBUTION:-public}"
SKINOS_OUTPUT="${SKINOS_OUTPUT:-$SKINOS_ROOT/dist/SkinOS.app}"
SKINOS_CORE="${SKINOS_CORE_PATH:-$SKINOS_ROOT/target/$SKINOS_CONFIGURATION/skinos-core}"
SKINOS_RUNTIME_HOST="${SKINOS_RUNTIME_HOST_PATH:-$SKINOS_ROOT/target/$SKINOS_CONFIGURATION/skinos-runtime-host}"
SKINOS_SDK_PROBE="${SKINOS_SDK_PROBE_PATH:-$SKINOS_ROOT/target/$SKINOS_CONFIGURATION/skinos-sdk-probe}"

if [[ "$SKINOS_CONFIGURATION" != "debug" && "$SKINOS_CONFIGURATION" != "release" ]]; then
    printf 'SKINOS_CONFIGURATION must be debug or release.\n' >&2
    exit 2
fi
if [[ "$SKINOS_DISTRIBUTION" != "public" && "$SKINOS_DISTRIBUTION" != "bundled" ]]; then
    printf 'SKINOS_DISTRIBUTION must be public or bundled.\n' >&2
    exit 2
fi
if [[ ! -x "$SKINOS_CORE" ]]; then
    printf 'Build the Rust executables first: cargo build -p skinos-core -p skinos-runtime-host -p skinos-sdk-probe\nMissing executable: %s\n' "$SKINOS_CORE" >&2
    exit 1
fi
if [[ ! -x "$SKINOS_SDK_PROBE" ]]; then
    printf 'Missing SDK probe: %s\nBuild it with cargo build -p skinos-sdk-probe (add --release for release packaging).\n' "$SKINOS_SDK_PROBE" >&2
    exit 1
fi
if [[ ! -x "$SKINOS_RUNTIME_HOST" ]]; then
    printf 'Missing runtime host: %s\nBuild it with cargo build -p skinos-runtime-host (add --release for release packaging).\n' "$SKINOS_RUNTIME_HOST" >&2
    exit 1
fi
# This query only resolves a local artifact path. It never loads Valve code,
# initializes the Steam runtime, opens a session, or starts a game.
if [[ -n "${SKINOS_TRACE_PATH:-}" ]]; then
    SKINOS_TRACE="$SKINOS_TRACE_PATH"
else
    SKINOS_TRACE="$("$SKINOS_RUNTIME_HOST" --print-trace-path)"
fi
if [[ ! -f "$SKINOS_TRACE" ]]; then
    printf 'Missing runtime helper library: %s\nRebuild skinos-runtime-host before packaging.\n' "$SKINOS_TRACE" >&2
    exit 1
fi

# Build the pinned compatibility payload separately from CrossOver. The latter
# remains a user-selected local installation and is never bundled here.
python3 "$SKINOS_ROOT/scripts/build-notproton.py"
bash "$SKINOS_ROOT/scripts/build-input-output-wine.sh"
python3 "$SKINOS_ROOT/scripts/build-wine-input.py"
# Feedback is packaged with input; launching it still requires the per-game
# Steam Input selection and a verified source-to-output route.
python3 "$SKINOS_ROOT/scripts/build-wine-input-rumble.py"
python3 "$SKINOS_ROOT/scripts/build-input-rumble-sink.py"
if [[ "$SKINOS_DISTRIBUTION" == "bundled" ]]; then
    python3 "$SKINOS_ROOT/scripts/build-runtime-payload.py" --output-dir "$SKINOS_ROOT/target/runtime-payload"
fi
SKINOS_NOTPROTON="$SKINOS_ROOT/target/notproton"
for SKINOS_REQUIRED_RESOURCE in skinos-notproton-setup sdk-probe.exe sdk-probe32.exe input-probe.exe input-probe32.exe input-probe-manifest.json run UPSTREAM.json bridge-manifest.json LICENSE; do
    if [[ ! -f "$SKINOS_NOTPROTON/$SKINOS_REQUIRED_RESOURCE" ]]; then
        printf 'Missing NotProton payload: %s\n' "$SKINOS_NOTPROTON/$SKINOS_REQUIRED_RESOURCE" >&2
        exit 1
    fi
done

swift build --disable-keychain --package-path "$SKINOS_ROOT/apps/macos" --configuration "$SKINOS_CONFIGURATION"
SKINOS_SWIFT_BIN="$(swift build --disable-keychain --package-path "$SKINOS_ROOT/apps/macos" --configuration "$SKINOS_CONFIGURATION" --show-bin-path)"
mkdir -p "$SKINOS_OUTPUT/Contents/MacOS" "$SKINOS_OUTPUT/Contents/Resources"
cp "$SKINOS_SWIFT_BIN/SkinOS" "$SKINOS_OUTPUT/Contents/MacOS/SkinOS"
cp "$SKINOS_CORE" "$SKINOS_OUTPUT/Contents/MacOS/skinos-core"
cp "$SKINOS_RUNTIME_HOST" "$SKINOS_OUTPUT/Contents/MacOS/skinos-runtime-host"
cp "$SKINOS_SDK_PROBE" "$SKINOS_OUTPUT/Contents/MacOS/skinos-sdk-probe"
cp "$SKINOS_TRACE" "$SKINOS_OUTPUT/Contents/MacOS/libskinos_trace.dylib"
cp "$SKINOS_ROOT/target/input-output-wine/libsteaminput_wine.dylib" "$SKINOS_OUTPUT/Contents/MacOS/libsteaminput_wine.dylib"
mkdir -p "$SKINOS_OUTPUT/Contents/Frameworks" "$SKINOS_OUTPUT/Contents/Resources/licenses"
ditto --norsrc --noextattr "$SKINOS_SWIFT_BIN/Sparkle.framework" "$SKINOS_OUTPUT/Contents/Frameworks/Sparkle.framework"
cp "$SKINOS_ROOT/apps/macos/.build/artifacts/sparkle/Sparkle/LICENSE" "$SKINOS_OUTPUT/Contents/Resources/licenses/Sparkle-LICENSE.txt"
cp "$SKINOS_ROOT/LICENSE" "$SKINOS_OUTPUT/Contents/Resources/licenses/SkinOS-LICENSE.txt"
cp "$SKINOS_ROOT/NOTICE" "$SKINOS_OUTPUT/Contents/Resources/licenses/SkinOS-NOTICE.txt"
python3 - "$SKINOS_NOTPROTON" "$SKINOS_OUTPUT/Contents/Resources/notproton" \
    "$SKINOS_ROOT/target/runtime-payload" "$SKINOS_OUTPUT/Contents/Resources/runtime-payload" \
    "$SKINOS_ROOT/crates/runtime-host/resources/runtime-payload-pin.json" <<'PYTHON'
import gzip
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys

source, destination, runtime_source, runtime_destination, runtime_pin = map(pathlib.Path, sys.argv[1:])
public = os.environ['SKINOS_DISTRIBUTION'] == 'public'
# Remove obsolete payload files from earlier pins instead of merging versions.
if destination.is_symlink():
    destination.unlink()
elif destination.exists():
    shutil.rmtree(destination)
# Package the runtime payload, not the build workspace. Object/import libraries
# and generated Swift sources remain in target/notproton for incremental builds.
runtime_files = (
    'skinos-notproton-setup', 'run', 'UPSTREAM.json',
    'bridge-manifest.json', 'fonts-manifest.json',
    'sdk-probe.exe', 'sdk-probe32.exe',
    'input-probe.exe', 'input-probe32.exe', 'input-probe-manifest.json',
    'LICENSE', 'LICENSE.fonts', 'LICENSE.lsteamclient',
    'LICENSE.steam-shim', 'NOTICE',
)
runtime_directories = ('NotProtonResources.bundle',) if public else ('NotProtonResources.bundle', 'bridge', 'fonts')
destination.mkdir(parents=True)
for name in runtime_files:
    artifact = source / name
    if not artifact.is_file() or artifact.is_symlink():
        raise SystemExit('Missing or unsafe NotProton runtime file: ' + name)
    shutil.copy2(artifact, destination / name)
for name in runtime_directories:
    artifact = source / name
    if not artifact.is_dir() or artifact.is_symlink() or any(path.is_symlink() for path in artifact.rglob('*')):
        raise SystemExit('Missing or unsafe NotProton runtime directory: ' + name)
    shutil.copytree(artifact, destination / name)
# The Wine bus is an owned, opt-in transport; only PE payloads from the checked
# manifest are bundled. Build import libraries and test executables stay local.
wine_source = source.parent / 'wine-input'
wine_destination = destination / 'wine-input'
wine_manifest = json.loads((wine_source / 'manifest.json').read_text())
for name, expected in wine_manifest['files'].items():
    relative = pathlib.PurePosixPath(name)
    if relative.is_absolute() or '..' in relative.parts or relative.suffix not in ('.exe', '.sys', '.inf'):
        raise SystemExit('Invalid Wine input payload path')
    artifact = wine_source / relative
    if artifact.is_symlink() or hashlib.sha256(artifact.read_bytes()).hexdigest() != expected:
        raise SystemExit('Wine input payload hash mismatch: ' + name)
    target = wine_destination / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(artifact, target)
shutil.copyfile(wine_source / 'manifest.json', wine_destination / 'manifest.json')
license_source = source.parent.parent / 'third_party/notproton/integration/wine-input'
for name in ('LICENSE.Wine', 'README.md'):
    shutil.copyfile(license_source / name, wine_destination / name)
# Keep the feedback assets separate from the basic input transport.
# No compiler intermediates or arbitrary manifest paths are packaged.
experiments = [
    ('wine-input-rumble-experiment', 'owned_wine_haptics_feedback', 'wine-input-rumble-experiment'),
    ('input-rumble-sink-experiment', 'retained_genuine_sdk_rumble_sink', 'input-rumble-sink-experiment'),
]
for folder, kind, suffix in experiments:
    origin = source.parent / folder
    data = json.loads((origin/'manifest.json').read_text())
    if data.get('schema_version') != 1 or data.get('experiment') != kind:
        raise SystemExit('Unexpected rumble experiment manifest: ' + folder)
    target_root = destination / suffix
    selected = {}
    for name, expected in data['files'].items():
        relative = pathlib.PurePosixPath(name)
        if relative.is_absolute() or '..' in relative.parts:
            raise SystemExit('Unsafe experimental artifact path')
        if kind == 'retained_genuine_sdk_rumble_sink':
            if name not in ('pe/input-probe.exe', 'pe/input-probe32.exe', 'pe/input-probe-manifest.json'):
                continue
        elif relative.suffix not in ('.exe', '.sys', '.inf'):
            raise SystemExit('Unexpected experimental transport artifact')
        artifact = origin / relative
        if artifact.is_symlink() or hashlib.sha256(artifact.read_bytes()).hexdigest() != expected:
            raise SystemExit('Experimental rumble artifact checksum mismatch: ' + name)
        target = target_root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(artifact, target)
        selected[name] = expected
    if not selected:
        raise SystemExit('Empty experimental rumble payload')
    (target_root/'manifest.json').write_text(json.dumps(dict(data, files=selected), indent=2)+'\n')
shutil.copyfile(license_source/'LICENSE.Wine', destination/'wine-input-rumble-experiment/LICENSE.Wine')
subprocess.run(['codesign', '--force', '--sign', '-', str(destination / 'skinos-notproton-setup')], check=True)
manifest = json.loads((destination / 'bridge-manifest.json').read_text())
if manifest.get('encoding') != 'gzip' or not manifest.get('files'):
    raise SystemExit('NotProton bridge must be a non-empty gzip payload')
for name, expected in manifest['files'].items():
    artifact = source / 'bridge' / (name + '.gz')
    if not artifact.resolve().is_relative_to((source / 'bridge').resolve()):
        raise SystemExit('Bridge manifest path escapes bundled payload')
    if hashlib.sha256(gzip.decompress(artifact.read_bytes())).hexdigest() != expected:
        raise SystemExit('NotProton bridge hash mismatch: ' + name)
# Wine requires the original x86_64 Unix bridge layout. Development bundles use
# compressed resources; package-dmg.py exposes their code as signed carriers for
# notarization and retains exact-byte restoration against the original hashes.
for artifact in destination.rglob('*'):
    if artifact.is_file():
        kind = subprocess.check_output(['/usr/bin/file', '-b', str(artifact)], text=True)
        if 'Mach-O' in kind:
            if artifact.is_relative_to(destination / 'bridge'):
                raise SystemExit('Uncompressed Mach-O bridge must not be packaged: ' + str(artifact))
            subprocess.run(['codesign', '--verify', '--strict', str(artifact)], check=True)
# The pinned, generic runtime payload excludes account/config/session state.
# Its builder validates file hashes; retain only its two distribution artifacts.
if runtime_destination.is_symlink():
    runtime_destination.unlink()
elif runtime_destination.exists():
    shutil.rmtree(runtime_destination)
runtime_destination.mkdir(parents=True)
shutil.copyfile(runtime_pin, runtime_destination / 'manifest.json')
for name in (() if public else ('payload.gz',)):
    artifact = runtime_source / name
    if not artifact.is_file() or artifact.is_symlink():
        raise SystemExit('Missing or unsafe runtime payload file: ' + name)
    shutil.copyfile(artifact, runtime_destination / name)
# A reused output directory must not retain previously bundled private payloads.
for name in ('NativeRuntimeCode', 'native-delivery.json'):
    obsolete = destination.parent / name
    if obsolete.is_symlink() or obsolete.is_file(): obsolete.unlink()
    elif obsolete.is_dir(): shutil.rmtree(obsolete)
(destination.parent / 'distribution.json').write_text(json.dumps({
    'schema_version': 1, 'kind': 'public' if public else 'bundled',
    'external_dependencies': 'publisher_pinned_downloads' if public else 'bundled',
}) + '\n')
PYTHON
SKINOS_ICON_TEMP="$(mktemp -d -t skinos-icon)"
trap 'rm -rf "$SKINOS_ICON_TEMP"' EXIT
swift "$SKINOS_ROOT/apps/macos/Tools/MakeIcon.swift" "$SKINOS_ICON_TEMP/SkinOS.iconset"
iconutil -c icns "$SKINOS_ICON_TEMP/SkinOS.iconset" -o "$SKINOS_OUTPUT/Contents/Resources/SkinOS.icns"

cat > "$SKINOS_OUTPUT/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>app.skinos.client</string>
  <key>CFBundleName</key><string>SkinOS</string>
  <key>CFBundleDisplayName</key><string>SkinOS</string>
  <key>CFBundleExecutable</key><string>SkinOS</string>
  <key>CFBundleIconFile</key><string>SkinOS</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.3</string>
  <key>CFBundleVersion</key><string>4</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict></plist>
PLIST

if [[ "$SKINOS_CONFIGURATION" == "release" ]]; then
    python3 "$SKINOS_ROOT/scripts/configure-updates.py" \
        --info-plist "$SKINOS_OUTPUT/Contents/Info.plist" \
        --feed-url "https://github.com/SkinOS-Community/SkinOS/releases/latest/download/appcast.xml" \
        --public-key "$(cat "$SKINOS_ROOT/release/update-public-key.txt")"
fi
python3 "$SKINOS_ROOT/scripts/sign-sparkle.py" --app "$SKINOS_OUTPUT" --identity -

# Local developer build. Distribution signing/notarization is a separate step.
codesign --force --sign - "$SKINOS_OUTPUT/Contents/MacOS/libskinos_trace.dylib"
codesign --force --sign - "$SKINOS_OUTPUT/Contents/MacOS/libsteaminput_wine.dylib"
codesign --force --sign - "$SKINOS_OUTPUT/Contents/MacOS/skinos-runtime-host"
codesign --force --sign - "$SKINOS_OUTPUT/Contents/MacOS/skinos-sdk-probe"
codesign --force --sign - "$SKINOS_OUTPUT/Contents/MacOS/skinos-core"
codesign --force --sign - "$SKINOS_OUTPUT"
codesign --verify --strict --deep "$SKINOS_OUTPUT"
# A successful build must resolve its bundled library, not the Cargo target path
# embedded for development. -ef also handles symlinked output directories.
SKINOS_PACKAGED_TRACE="$("$SKINOS_OUTPUT/Contents/MacOS/skinos-runtime-host" --print-trace-path)"
if [[ ! "$SKINOS_PACKAGED_TRACE" -ef "$SKINOS_OUTPUT/Contents/MacOS/libskinos_trace.dylib" ]]; then
    printf 'Bundle verification failed: runtime helper resolves outside the application.\n' >&2
    exit 1
fi
printf 'Built %s\n' "$SKINOS_OUTPUT"
