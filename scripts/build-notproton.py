#!/usr/bin/env python3
"""Bundle pinned NotProton sources, bridge artifacts and a headless runner installer.

The source pin is independent of local CrossOver. CrossOver is copied from the
user-selected installed app at first use, never redistributed by this script.
"""
import argparse
import hashlib
import gzip
import json
import pathlib
import platform
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
BASE = ROOT / 'third_party/notproton'
UP = BASE / 'upstream'
OUT = ROOT / 'target/notproton'
BRIDGE_NAMES = (
    'steam.exe', 'steamclient.dll', 'steamclient64.dll', 'tier0_s64.dll', 'vstdlib_s64.dll',
    'lsteamclient.dll', 'lsteamclient.so', 'i386-windows/lsteamclient.dll',
    'x86_64-windows/lsteamclient.dll', 'x86_64-unix/lsteamclient.so', 'aarch64-unix/lsteamclient.so',
    'legacycompat/Steam.dll',
)
REQUIRED_FONT_NAMES = (
    'arial.ttf', 'arialbd.ttf', 'cour.ttf', 'courbd.ttf', 'georgia.ttf',
    'lucon.ttf', 'malgun.ttf', 'micross.ttf', 'msgothic.ttc', 'msyh.ttf',
    'msyhbd.ttf', 'nirmala.ttf', 'simsun.ttc', 'times.ttf', 'alt/arial.ttf',
    'liberation-fonts/LICENSE', 'noto/LICENSE', 'source-han-sans/LICENSE.txt', 'ume/license.html',
)


def font_names(manifest):
    names = manifest.get('font_files', [])
    if not isinstance(names, list) or not all(isinstance(name, str) for name in names) or len(set(names)) != len(names):
        raise RuntimeError('Invalid pinned font inventory')
    for name in names:
        parts = pathlib.PurePosixPath(name).parts if isinstance(name, str) else ()
        if (len(parts) < 2 or parts[0] != 'fonts' or '\\' in name
                or any(part in ('', '.', '..') for part in parts)
                or '/'.join(parts) != name):
            raise RuntimeError('Unsafe pinned font path')
    if not {'fonts/' + name for name in REQUIRED_FONT_NAMES}.issubset(names):
        raise RuntimeError('Pinned fonts must include the complete Proton font and license set')
    return names


def read_pinned_bridge(source, metadata):
    """Read only the immutable, revision-bound bridge pin. Never use an installed app."""
    manifest = json.loads((source / 'manifest.json').read_text())
    if manifest.get('encoding') != 'gzip':
        raise RuntimeError('Pinned bridge must use gzip encoding')
    for field in ('revision', 'version'):
        if not metadata.get(field) or manifest.get(field) != metadata[field]:
            raise RuntimeError('Pinned bridge ' + field + ' does not match UPSTREAM.json')
    hashes = manifest.get('files', {})
    names = (*BRIDGE_NAMES, *font_names(manifest))
    if set(hashes) != set(names):
        raise RuntimeError('Pinned bridge manifest must contain the complete expected artifact set')
    result = {}
    for name in names:
        expected = hashes[name]
        if not isinstance(expected, str) or not re.fullmatch('[0-9a-f]{64}', expected):
            raise RuntimeError('Invalid bridge checksum for ' + name)
        data = gzip.decompress((source / (name + '.gz')).read_bytes())
        if hashlib.sha256(data).hexdigest() != expected:
            raise RuntimeError('Pinned bridge checksum mismatch: ' + name)
        result[name] = data
    return result, manifest


def stage_bridge_pin(source, destination, metadata, provenance=None):
    """Capture an explicitly supplied matching build for transactional update."""
    destination.mkdir(parents=True, exist_ok=False)
    hashes = {}
    fonts = source / 'fonts'
    if fonts.is_symlink():
        raise RuntimeError('Font source cannot be linked')
    font_files = []
    if fonts.is_dir():
        for path in sorted(fonts.rglob('*')):
            if path.is_symlink():
                raise RuntimeError('Font source contains a linked entry')
            if path.is_file():
                font_files.append('fonts/' + path.relative_to(fonts).as_posix())
    font_names({'font_files': font_files})
    for name in (*BRIDGE_NAMES, *font_files):
        data = (source / name).read_bytes()
        packed = destination / (name + '.gz')
        packed.parent.mkdir(parents=True, exist_ok=True)
        packed.write_bytes(gzip.compress(data, compresslevel=6, mtime=0))
        hashes[name] = hashlib.sha256(data).hexdigest()
    if provenance is not None:
        for field in ('repository', 'revision', 'version'):
            if provenance.get(field) != metadata[field]:
                raise RuntimeError('Bridge provenance ' + field + ' differs from the requested pin')
        sources = provenance.get('files', {})
        if set(sources) != set(hashes) or any(sources[name].get('sha256') != digest for name, digest in hashes.items()):
            raise RuntimeError('Bridge provenance file checksums differ from supplied binaries')
    manifest = {
        'schema_version': 1,
        'repository': metadata['repository'],
        'revision': metadata['revision'],
        'version': metadata['version'],
        'encoding': 'gzip',
        'font_files': font_files,
        'provenance': provenance or {
            'kind': 'operator-supplied-matching-build',
            'source': str(source),
            'description': 'The updater requires a bridge built from the requested source revision; SHA-256 pins the supplied bytes.',
        },
        'files': hashes,
    }
    (destination / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    read_pinned_bridge(destination, metadata)
    return manifest

def build_sdk_probes(base, out, lld):
    """Build both PE widths; a 32-bit game's original DLL needs a 32-bit host."""
    out.mkdir(parents=True, exist_ok=True)
    imports = {'LoadLibraryA': 4, 'GetProcAddress': 8, 'GetEnvironmentVariableA': 12,
               'GetStdHandle': 4, 'WriteFile': 20, 'GetTickCount64': 0, 'Sleep': 4, 'ExitProcess': 4}
    definition = out / 'kernel32.def'
    definition.write_text('LIBRARY KERNEL32.dll\nEXPORTS\n' + '\n'.join(imports) + '\n')
    for arch, triple, name in (('x64', 'x86_64-pc-windows-msvc', 'sdk-probe'),
                               ('x86', 'i686-pc-windows-msvc', 'sdk-probe32')):
        library = out / ('kernel32-' + arch + '.lib')
        subprocess.run([str(lld), '-flavor', 'link', '/lib', '/machine:' + arch,
                        '/def:' + str(definition), '/out:' + str(library)], check=True)
        common = ['clang', '-target', triple, '-c', '-O2', '-Wall', '-Wextra', '-Werror',
                  '-fno-stack-protector', '-ffreestanding']
        objects = []
        for source in ('sdk-probe.c', 'sdk-client.cpp'):
            obj = out / (name + '-' + pathlib.Path(source).stem + '.obj')
            flags = ['-std=c++11', '-fno-exceptions', '-fno-rtti'] if source.endswith('.cpp') else []
            subprocess.run(common + flags + ['-o', str(obj), str(base / 'integration' / source)], check=True)
            objects.append(str(obj))
        # Win32 API calls are stdcall in x86 code, while kernel32 exports its
        # names undecorated. Resolve the decorated import symbols to the import
        # library without changing their calling convention or DLL entry names.
        aliases = ['/alternatename:__imp__%s@%d=__imp__%s' % (symbol, size, symbol)
                   for symbol, size in imports.items()] if arch == 'x86' else []
        subprocess.run([str(lld), '-flavor', 'link', '/machine:' + arch,
                        '/entry:mainCRTStartup', '/subsystem:console', '/nodefaultlib',
                        '/out:' + str(out / (name + '.exe'))] + aliases + objects + [str(library)], check=True)


def supported_runner_compile_source(source):
    # Swift 6.3 cannot infer this new upstream flatMap/filter chain when the
    # model is compiled outside the full app. Only explicit collection types
    # are added; retain the exact upstream file in the vendor source snapshot.
    anchor = 'let served = all.filter { installed.contains($0.id) }.flatMap { build in'
    if source.count(anchor) != 1:
        raise RuntimeError('Upstream runner collection changed; review the compile adapter')
    return source.replace(anchor, 'let served: [InstalledTool] = all.filter { installed.contains($0.id) }.flatMap { build -> [InstalledTool] in', 1)


PREFIX_SESSION_HELPERS = r'''# SkinOS prepared-prefix diagnostic handshake (v1).
# These inputs are captured before upstream appends its bridge overrides.
skinos_session_dllpath="$WINEDLLPATH"
skinos_session_overrides="$WINEDLLOVERRIDES"
skinos_session_failure() {
  echo "SkinOS: prepared prefix session is unavailable or changed; refusing a reset while the input feeder is running" >&2
  exit 1
}
skinos_session_identity_data() (
  # Keep the original runner and prefix architecture checks independently of
  # this receipt. The receipt binds one successful SDK preparation to exactly
  # this prefix, build, staged loader, bridge and immutable launch settings.
  [ -n "$STEAM_COMPAT_DATA_PATH" ] && [ -d "$WINEPREFIX" ] && [ ! -L "$WINEPREFIX" ] || return 1
  [ -s "$WINEPREFIX/system.reg" ] && [ ! -L "$WINEPREFIX/system.reg" ] || return 1
  stat -f '%d:%i' "$WINEPREFIX" || return 1
  for value in "$STEAM_COMPAT_DATA_PATH" "$WINEPREFIX" "$np_support" "$np_build" "$np_flavor" \
      "$WINELOADER" "$WINESERVER" "$wine_unix" "$CX_ROOT" "$app_id" \
      "$STEAM_COMPAT_INSTALL_PATH" "$STEAM_COMPAT_LIBRARY_PATHS" "$WINEMSYNC" \
      "${NOTPROTON_RETINA:-0}" "${NOTPROTON_RAW_CONTROLLERS:-0}" "${SKINOS_INPUT_FILTER_EXACT:-0}" \
      "$SDL_GAMECONTROLLER_IGNORE_DEVICES" "$SDL_GAMECONTROLLER_IGNORE_DEVICES_EXCEPT" \
      "$CX_GRAPHICS_BACKEND" "$skinos_session_dllpath" "$skinos_session_overrides"; do
    printf '%s:%s\n' "${#value}" "$value" || return 1
  done
  template_identity || return 1
  for file in "$WINEPREFIX/drive_c/windows/system32/ntdll.dll" \
      "$WINEPREFIX/drive_c/windows/system32/winedevice.exe"; do
    [ -f "$file" ] && [ ! -L "$file" ] || return 1
    /usr/bin/shasum -a 256 "$file" || return 1
  done
  for name in notproton-build notproton-msync notproton-hidden-controllers notproton-game-drive; do
    file="$STEAM_COMPAT_DATA_PATH/$name"
    if [ -e "$file" ] || [ -L "$file" ]; then
      [ -f "$file" ] && [ ! -L "$file" ] || return 1
      /usr/bin/shasum -a 256 "$file" || return 1
    else
      printf 'absent:%s\n' "$name" || return 1
    fi
  done
  printf 'drive-s:%s\n' "$(readlink "$WINEPREFIX/dosdevices/s:" 2>/dev/null || true)" || return 1
  for name in steamclient64.dll steamclient.dll tier0_s64.dll vstdlib_s64.dll \
      lsteamclient.dll lsteamclient.so steam.exe; do
    relative="$name"
    [ "$name" != lsteamclient.so ] || relative="${wine_unix##*/}/$name"
    source="$np_support/bridge/$relative"
    selected="$WINEPREFIX/drive_c/Program Files (x86)/Steam/$name"
    [ -f "$source" ] && [ -f "$selected" ] && [ ! -L "$selected" ] || return 1
    cmp -s "$source" "$selected" || return 1
    /usr/bin/shasum -a 256 "$source" "$selected" || return 1
  done
)
skinos_session_identity() {
  skinos_identity_data=$(skinos_session_identity_data) || return 1
  skinos_identity_digest=$(printf '%s\n' "$skinos_identity_data" | /usr/bin/shasum -a 256) || return 1
  printf 'skinos-prefix-session-v1:%s\n' "${skinos_identity_digest%% *}"
}
case "${SKINOS_PREFIX_SESSION_READY:-}" in ''|1) ;; *) skinos_session_failure ;; esac
if [ -n "${SKINOS_PREFIX_SESSION_RECEIPT:-}" ] || [ "${SKINOS_PREFIX_SESSION_READY:-}" = 1 ]; then
  [ "$verb" = waitforexitandrun ] && [ -n "$STEAM_COMPAT_DATA_PATH" ] || skinos_session_failure
  case "$SKINOS_PREFIX_SESSION_RECEIPT" in /*) ;; *) skinos_session_failure ;; esac
fi
'''

PREFIX_SESSION_REUSE = r'''if [ "${SKINOS_PREFIX_SESSION_READY:-}" = 1 ]; then
  export WINEPREFIX="$STEAM_COMPAT_DATA_PATH/pfx"
  # Refuse missing state before upstream's lock helper could create anything.
  [ -d "$WINEPREFIX" ] && [ ! -L "$WINEPREFIX" ] && [ -s "$WINEPREFIX/system.reg" ] || skinos_session_failure
  stage_step="prepared prefix lock"
  prepare_prefix_directory
  if [ -z "$WINEMSYNC" ] && [ -r "$STEAM_COMPAT_DATA_PATH/notproton-msync" ]; then
    WINEMSYNC=$(tr -d ' \t\n' < "$STEAM_COMPAT_DATA_PATH/notproton-msync" 2>/dev/null || true)
  fi
  export WINEMSYNC="${WINEMSYNC:-0}"
  stage_step="prepared prefix build check"
  refuse_other_build
  stage_step="prepared prefix arch check"
  refuse_foreign_prefix
  stage_step="prepared prefix receipt check"
  receipt="$SKINOS_PREFIX_SESSION_RECEIPT"
  [ -f "$receipt" ] && [ ! -L "$receipt" ] && [ "$(stat -f '%Lp' "$receipt")" = 600 ] || skinos_session_failure
  [ "$(stat -f '%u' "$receipt")" = "$(id -u)" ] || skinos_session_failure
  [ "$(stat -f '%z' "$receipt")" -le 96 ] || skinos_session_failure
  expected=$(skinos_session_identity) || skinos_session_failure
  [ "$(cat "$receipt")" = "$expected" ] || skinos_session_failure
  echo "=== SkinOS reused the verified SDK prefix session without restart ===" >> "$log" 2>&1 || true
else
'''

PREFIX_SESSION_PUBLISH = r'''# SkinOS prepared-prefix receipt publication: only after all bridge staging.
if [ -n "${SKINOS_PREFIX_SESSION_RECEIPT:-}" ] && [ "${SKINOS_PREFIX_SESSION_READY:-}" != 1 ]; then
  stage_step="prepared prefix receipt publication"
  identity=$(skinos_session_identity) || skinos_session_failure
  receipt="$SKINOS_PREFIX_SESSION_RECEIPT"
  [ ! -e "$receipt" ] && [ ! -L "$receipt" ] || skinos_session_failure
  # The worker provides a fresh private run-state path. No existing receipt is
  # replaced, and only the owning user may read/write the opaque digest.
  (umask 077; set -C; printf '%s\n' "$identity" > "$receipt") || skinos_session_failure
fi
unset SKINOS_PREFIX_SESSION_READY SKINOS_PREFIX_SESSION_RECEIPT
'''


def prepared_prefix_adapter(script):
    """Add a diagnostic-only reuse branch; preserve the complete first path.

    Never skip a lone -k: import_prefix_settings and map_game_drive can restart
    Wine too. Review the pinned preparation and settings functions on updates.
    """
    marker = 'if [ -n "$STEAM_COMPAT_DATA_PATH" ]; then\n  export WINEPREFIX='
    if script.count(marker) != 1:
        raise RuntimeError('Upstream prefix preparation entry changed; review session reuse')
    start = script.index(marker)
    end = script.index('\nbridge_src=', start)
    original = script[start:end]
    if hashlib.sha256(original.encode()).hexdigest() != '85ed30738189791a1b5bf7bfaad16040745f2a65d17e472e0334f9f6db41d510':
        raise RuntimeError('Upstream prefix preparation changed; review all reset paths')
    settings_start = script.index('import_prefix_settings() {')
    settings_end = script.index('\n}\n', settings_start) + 3
    if hashlib.sha256(script[settings_start:settings_end].encode()).hexdigest() != 'febc290e80ea39b46acf6a626dd9db6eda504a555759054b31782bec254ba61f':
        raise RuntimeError('Upstream prefix settings changed; review nested reset paths')
    script = script[:start] + PREFIX_SESSION_HELPERS + PREFIX_SESSION_REUSE + original + '\nfi\n' + script[end:]
    publish_marker = 'export WINEDEBUG="${WINEDEBUG:-err+all,fixme-all}"'
    if script.count(publish_marker) != 1:
        raise RuntimeError('Upstream final preparation boundary changed; review session receipt')
    return script.replace(publish_marker, PREFIX_SESSION_PUBLISH + '\n' + publish_marker, 1)


def exact_controller_filter_adapter(script):
    """Our mapped-device policy must not hide unrelated Nintendo models."""
    automatic = '    controllers_wanted=$(printf \'%s\\n\' "$controllers_wanted" 057e/2006 057e/2007 057e/2009 | sed \'/^$/d\' | sort -u)'
    if script.count(automatic) != 1:
        raise RuntimeError('Upstream controller filtering changed; review exact device policy')
    return script.replace(automatic,
        '    if [ "${SKINOS_INPUT_FILTER_EXACT:-0}" != 1 ]; then\n' + automatic + '\n    fi', 1)

def main():
    global BASE, UP, OUT
    parser = argparse.ArgumentParser()
    parser.add_argument('--source-root', type=pathlib.Path, default=BASE,
        help='Pinned source root containing upstream, integration, UPSTREAM.json and bridge')
    parser.add_argument('--output-dir', type=pathlib.Path, default=OUT)
    parser.add_argument('--bridge-source', type=pathlib.Path,
        help='Explicit pinned bridge directory with manifest.json; defaults to SOURCE_ROOT/bridge')
    args = parser.parse_args()
    BASE = args.source_root.resolve()
    UP = BASE / 'upstream'
    OUT = args.output_dir.resolve()
    bridge_source = (args.bridge_source or BASE / 'bridge').resolve()
    metadata = json.loads((BASE / 'UPSTREAM.json').read_text())
    artifacts, pinned_manifest = read_pinned_bridge(bridge_source, metadata)
    OUT.mkdir(parents=True, exist_ok=True)
    resources = OUT / 'NotProtonResources.bundle'
    resources.mkdir(exist_ok=True)
    for path in (UP / 'app/Sources/NotProtonApp/Resources').glob('*.bin'):
        shutil.copy2(path, resources / path.name)
    (resources / 'Info.plist').write_text('<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>app.skinos.notproton.resources</string></dict></plist>')
    src = UP / 'app/Sources/NotProtonApp'
    files = ['Model/NtdllPatcher.swift', 'Model/RunnerPatcher.swift',
             'Model/CrossOverLicense.swift', 'Support/Digest.swift', 'Support/Shell.swift',
             'Support/AtomicReplace.swift', 'Support/Clean.swift', 'Support/StepFailure.swift',
             'Support/WriteRefused.swift']
    compiled_sources = OUT / 'build-sources'
    compiled_sources.mkdir(exist_ok=True)
    runner_model = compiled_sources / 'SupportedRunner.swift'
    runner_model.write_text(supported_runner_compile_source((src / 'Model/SupportedRunner.swift').read_text()))
    subprocess.run(['swiftc', '-swift-version', '6', '-target', platform.machine() + '-apple-macosx15.0',
                    '-O', '-parse-as-library', '-o', str(OUT / 'skinos-notproton-setup'),
                    str(BASE / 'integration/Setup.swift'), str(BASE / 'integration/HostCompatibility.swift'), str(runner_model)]
                   + [str(src / f) for f in files], check=True)
    sysroot = pathlib.Path(subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip())
    host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
    lld = sysroot / 'lib/rustlib' / host / 'bin/rust-lld'
    if not lld.is_file():
        candidates = list((pathlib.Path.home() / '.rustup/toolchains').glob('*/lib/rustlib/' + host + '/bin/rust-lld'))
        if not candidates: raise RuntimeError('rust-lld for the host is needed to build the Windows SDK probe')
        lld = candidates[0]
    build_sdk_probes(BASE, OUT, lld)
    subprocess.run(['python3', str(ROOT / 'scripts/build-input-probe.py'),
                    '--source-root', str(BASE), '--output-dir', str(OUT)], check=True)
    bridge = OUT / 'bridge'
    bridge.mkdir(exist_ok=True)
    hashes = {name: pinned_manifest['files'][name] for name in BRIDGE_NAMES}
    fonts = OUT / 'fonts'
    fonts.mkdir(exist_ok=True)
    font_hashes = {}
    for name, data in artifacts.items():
        if name.startswith('fonts/'):
            relative = name.removeprefix('fonts/')
            destination = fonts / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
            font_hashes[relative] = pinned_manifest['files'][name]
            continue
        destination = bridge / (name + '.gz')
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(gzip.compress(data, compresslevel=6, mtime=0))
        # Wine needs the original unsigned x86_64 Unix bridge at runtime.
        legacy = bridge / name
        if legacy.is_file(): legacy.unlink()
    for name in ['LICENSE', 'LICENSE.lsteamclient', 'LICENSE.steam-shim', 'LICENSE.fonts', 'NOTICE']:
        shutil.copy2(UP / name, OUT / name)
    shutil.copy2(BASE / 'UPSTREAM.json', OUT / 'UPSTREAM.json')
    (OUT / 'bridge-manifest.json').write_text(json.dumps({'source': 'vendored-pinned-bridge', 'revision': metadata['revision'], 'version': metadata['version'], 'encoding': 'gzip', 'files': hashes}, indent=2) + '\n')
    (OUT / 'fonts-manifest.json').write_text(json.dumps({'schema_version': 1, 'revision': metadata['revision'], 'version': metadata['version'], 'files': font_hashes}, indent=2) + '\n')
    script = (UP / 'dylib/feats/compat_run.sh').read_text()
    script = script.replace('np_support="$HOME/Library/Application Support/notproton"', 'np_support="${SKINOS_NOTPROTON_SUPPORT:?Missing owned NotProton support path}"', 1)
    early = 'np_support="${SKINOS_NOTPROTON_SUPPORT:?Missing owned NotProton support path}"'
    script = script.replace(early, early + '\nexport DYLD_INSERT_LIBRARIES="${SKINOS_GAME_DYLD_INSERT_LIBRARIES:?Missing SkinOS IPC adapter}"', 1)
    loader_export = 'export WINELOADER WINESERVER'
    if script.count(loader_export) != 1: raise RuntimeError('Upstream loader selection changed')
    script = script.replace(loader_export, 'WINELOADER="${SKINOS_WINELOADER:?Missing staged SkinOS Wine loader}"\n' + loader_export, 1)
    start = script.index('show_alert() {')
    end = script.index('\n}', start) + 2
    script = script[:start] + 'show_alert() { printf "%s: %s\\n" "$1" "$2" >&2; }' + script[end:]
    fallback = 'native_client="$HOME/Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS"'
    script = script.replace(fallback, 'echo "Missing SkinOS runtime path" >&2; exit 1', 1)
    # Core prepares the stable common/Steam.dll link before entering the runtime
    # sandbox, preserving any existing file/link. Upstream's ln -sfn would both
    # overwrite foreign links and point at a disposable per-launch runtime.
    legacy_start = script.index('link_legacy_steam_dll() {')
    legacy_end = script.index('\n}', legacy_start) + 2
    script = script[:legacy_start] + '''link_legacy_steam_dll() {
  [ -f "$bridge_src/legacycompat/Steam.dll" ] || {
    echo "Missing verified SkinOS legacy Steam bridge" >&2
    return 1
  }
  # The stable shared-library alias is validated by SkinOS before launch.
}''' + script[legacy_end:]
    old = '"$WINELOADER" "$@" >> "$log" 2>&1 || status=$?'
    if script.count(old) != 1: raise RuntimeError('Upstream helper launch changed; inspect before integrating')
    script = script.replace(old, '"$WINELOADER" "$@" || status=$?', 1)
    script = script.replace('if [ "$foreground" = 0 ]; then', 'export DYLD_INSERT_LIBRARIES="${SKINOS_GAME_DYLD_INSERT_LIBRARIES:?Missing SkinOS IPC adapter}"\n\nif [ "$foreground" = 0 ]; then', 1)
    marker = '# Fixes CrossOver window focus issues'
    at = script.index(marker)
    # Keep upstream prefix/bridge setup, but let the owning SkinOS supervisor hold
    # and reap the actual Wine command. No system open/Steam URL fallback.
    script = script[:at] + '''# SkinOS owns the parent process and publishes its own Steam IPC broker.
[ -n "$STEAM_COMPAT_CLIENT_INSTALL_PATH" ] || { echo "Missing SkinOS runtime" >&2; exit 1; }
[ -n "$SKINOS_GAME_DYLD_INSERT_LIBRARIES" ] || { echo "Missing SkinOS IPC adapter" >&2; exit 1; }
export DYLD_INSERT_LIBRARIES="$SKINOS_GAME_DYLD_INSERT_LIBRARIES"
exec "$WINELOADER" "$shim_exe" "$@"
'''
    script = prepared_prefix_adapter(script)
    script = exact_controller_filter_adapter(script)
    (OUT / 'run').write_text(script)
    (OUT / 'run').chmod(0o755)
    subprocess.run(['/bin/sh', '-n', str(OUT / 'run')], check=True)
    print(OUT)

if __name__ == '__main__': main()
