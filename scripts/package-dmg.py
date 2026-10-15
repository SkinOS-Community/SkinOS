#!/usr/bin/env python3
"""Prepare/sign, package, and verify a shareable SkinOS DMG.

Notarization is deliberately a separate resumable CLI step: submit the prepared
app ZIP, staple the accepted app, create/submit/staple the DMG, then verify it.
No account files, credentials, installed games or CrossOver installation are read.
"""
import argparse
import hashlib
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import importlib.util

NATIVE = [
    'Contents/MacOS/libskinos_trace.dylib',
    'Contents/MacOS/libsteaminput_wine.dylib',
    'Contents/Resources/notproton/skinos-notproton-setup',
    'Contents/MacOS/skinos-runtime-host',
    'Contents/MacOS/skinos-sdk-probe',
    'Contents/MacOS/skinos-core',
]
RUNTIME = {'skinos-runtime-host', 'skinos-sdk-probe'}
_sparkle_spec = importlib.util.spec_from_file_location('skinos_sign_sparkle', Path(__file__).with_name('sign-sparkle.py'))
_sparkle = importlib.util.module_from_spec(_sparkle_spec)
sys.modules[_sparkle_spec.name] = _sparkle
_sparkle_spec.loader.exec_module(_sparkle)


def is_public(app):
    path = app / 'Contents/Resources/distribution.json'
    if not path.exists():
        return False
    value = json.loads(path.read_text())
    if value.get('schema_version') != 1 or value.get('kind') not in ('public', 'bundled'):
        raise RuntimeError('Invalid distribution profile')
    return value['kind'] == 'public'


def sparkle_paths(app):
    if (app / 'Contents/Frameworks/Sparkle.framework').is_dir():
        return _sparkle.NATIVE_PATHS
    return []


def run(*args):
    subprocess.run([str(x) for x in args], check=True)


def copy_app(source, target):
    if target.exists() or target.is_symlink():
        raise RuntimeError('Refusing existing app destination: ' + str(target))
    target.parent.mkdir(parents=True, exist_ok=True)
    run('/usr/bin/ditto', '--norsrc', '--noextattr', source, target)


def native_carriers(app):
    resources = app / 'Contents/Resources'
    if is_public(app):
        for relative in ('native-delivery.json', 'NativeRuntimeCode', 'runtime-payload/payload.gz', 'notproton/bridge', 'notproton/fonts'):
            path = resources / relative
            if path.exists() or path.is_symlink():
                raise RuntimeError('Public distribution contains a bundled external payload: ' + relative)
        return {}
    manifest = json.loads((resources / 'native-delivery.json').read_text())
    if manifest.get('schema_version') != 1:
        raise RuntimeError('Unsupported native delivery manifest')
    carriers = {}
    for group in ('runtime', 'bridge'):
        for record in manifest[group].values():
            relative = Path(record['carrier'])
            if (relative.is_absolute() or '..' in relative.parts
                    or len(relative.parts) < 2 or relative.parts[0] != 'NativeRuntimeCode'):
                raise RuntimeError('Unsafe native carrier path')
            digest = hashlib.sha256((resources / relative).read_bytes()).hexdigest()
            if digest != record['carrier_sha256']:
                raise RuntimeError('Signed native carrier changed: ' + str(relative))
            carriers[str(Path('Contents/Resources') / relative)] = digest
    if not carriers:
        raise RuntimeError('No signed native carriers were prepared')
    return carriers


def check_native_inventory(app):
    expected = set(NATIVE) | {'Contents/MacOS/SkinOS'} | set(native_carriers(app)) | set(sparkle_paths(app))
    actual = set()
    magic = {b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf',
             b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xce',
             b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca',
             b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}
    for path in app.rglob('*'):
        if path.is_symlink():
            framework = app / 'Contents/Frameworks/Sparkle.framework'
            if not path.is_relative_to(framework) or not path.resolve(strict=True).is_relative_to(framework.resolve()):
                raise RuntimeError('Unexpected app symlink: ' + str(path))
            continue
        if path.is_file():
            with path.open('rb') as stream:
                if stream.read(4) in magic:
                    actual.add(str(path.relative_to(app)))
    if actual != expected:
        raise RuntimeError('Native signing inventory changed: ' + repr(actual ^ expected))


def prepare(args):
    app = args.work / 'SkinOS.app'
    copy_app(args.app, app)
    if not is_public(app):
        run(sys.executable, Path(__file__).with_name('prepare-native-delivery.py'),
            '--app', app, '--identity', args.identity)
    check_native_inventory(app)
    # The transformer signs carriers before recording their hashes. Do not
    # re-sign them here: recipes refer to the exact signed bytes reviewed by
    # notarization, while runtime reconstruction retains upstream ABI pins.
    for relative in native_carriers(app):
        run('/usr/bin/codesign', '--verify', '--strict', app / relative)
    entitlements = args.work / 'runtime.entitlements.plist'
    entitlements.write_bytes(plistlib.dumps({
        'com.apple.security.cs.allow-dyld-environment-variables': True,
        'com.apple.security.cs.disable-library-validation': True,
    }))
    for relative in NATIVE:
        path = app / relative
        command = ['/usr/bin/codesign', '--force', '--sign', args.identity,
                   '--timestamp', '--options', 'runtime']
        if path.name in RUNTIME:
            command += ['--entitlements', str(entitlements)]
        run(*command, path)
        run('/usr/bin/codesign', '--verify', '--strict', path)
    if sparkle_paths(app):
        run(sys.executable, Path(__file__).with_name('sign-sparkle.py'), '--app', app, '--identity', args.identity)
    run('/usr/bin/codesign', '--force', '--sign', args.identity,
        '--timestamp', '--options', 'runtime', app)
    run('/usr/bin/codesign', '--verify', '--deep', '--strict', app)
    archive = args.work / 'SkinOS-notary.zip'
    if archive.exists():
        raise RuntimeError('Refusing existing notarization archive')
    run('/usr/bin/ditto', '-c', '-k', '--keepParent', '--norsrc', '--noextattr', app, archive)
    print(json.dumps({'app': str(app), 'notarization_archive': str(archive)}))


def make_dmg(args):
    app = args.work / 'SkinOS.app'
    version = plistlib.loads((app / 'Contents/Info.plist').read_bytes())['CFBundleShortVersionString']
    run('/usr/bin/codesign', '--verify', '--deep', '--strict', app)
    run('xcrun', 'stapler', 'validate', app)
    volume = args.work / 'volume'
    if volume.exists() or args.output.exists():
        raise RuntimeError('Refusing existing volume staging directory or DMG')
    volume.mkdir()
    copy_app(app, volume / 'SkinOS.app')
    run('xcrun', 'stapler', 'validate', volume / 'SkinOS.app')
    (volume / 'Applications').symlink_to('/Applications')
    (volume / 'Установка.txt').write_text(
        f'SkinOS {version}\n\n'
        'Mac с Apple Silicon (M1 или новее), macOS 15 или новее.\n\n'
        '1. Перетащите SkinOS в Applications («Программы»).\n'
        '2. Откройте SkinOS из «Программ» и войдите в свой Steam через QR-код.\n\n'
        'Для Windows-игр нужен отдельно установленный совместимый CrossOver\n'
        'или CrossOver Preview. Интеграция NotProton включена в клиент.\n'
        'Внешние компоненты при первом запуске загружаются напрямую\n'
        'из источников Valve и NotProton с проверкой целостности.\n'
        'Вариант FEX требует macOS 26+; на macOS 15 используется Rosetta.\n\n'
        'Аккаунты, сохранения и игры автора в образ не входят.\n', encoding='utf-8')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    run('/usr/bin/hdiutil', 'create', '-volname', 'SkinOS', '-srcfolder', volume,
        '-fs', 'HFS+', '-format', 'UDZO', '-imagekey', 'zlib-level=9', args.output)
    run('/usr/bin/codesign', '--sign', args.identity, '--timestamp',
        '--identifier', 'app.skinos.client.diskimage', args.output)
    print(json.dumps({'dmg': str(args.output)}))


def verify(args):
    run('/usr/bin/hdiutil', 'verify', args.output)
    run('/usr/bin/codesign', '--verify', '--strict', args.output)
    run('xcrun', 'stapler', 'validate', args.output)
    run('/usr/sbin/spctl', '--assess', '--type', 'open', '--context',
        'context:primary-signature', '--verbose=4', args.output)
    mount = args.work / 'mounted'
    mount.mkdir(exist_ok=False)
    extracted = args.work / 'extracted/SkinOS.app'
    run('/usr/bin/hdiutil', 'attach', '-readonly', '-nobrowse', '-noautoopen',
        '-mountpoint', mount, args.output)
    try:
        assert (mount / 'Applications').is_symlink()
        assert (mount / 'Applications').readlink() == Path('/Applications')
        copy_app(mount / 'SkinOS.app', extracted)
        check_native_inventory(extracted)
        for relative in [*NATIVE, *native_carriers(extracted), *sparkle_paths(extracted)]:
            run('/usr/bin/codesign', '--verify', '--strict', extracted / relative)
        run('/usr/bin/codesign', '--verify', '--deep', '--strict', extracted)
        run('xcrun', 'stapler', 'validate', extracted)
        run('/usr/sbin/spctl', '--assess', '--type', 'execute', '--verbose=4', extracted)
    finally:
        run('/usr/bin/hdiutil', 'detach', mount)
    digest = hashlib.sha256(args.output.read_bytes()).hexdigest()
    args.output.with_suffix('.dmg.sha256').write_text(digest + '  ' + args.output.name + '\n')
    report = {'dmg': str(args.output), 'sha256': digest,
              'size_bytes': args.output.stat().st_size, 'extracted_app': str(extracted),
              'dmg_signature_verified': True, 'dmg_ticket_verified': True,
              'extracted_app_signature_verified': True, 'extracted_app_ticket_verified': True,
              'gatekeeper_accepted': True, 'architecture': 'arm64', 'minimum_macos': '15.0'}
    (args.work / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['prepare', 'dmg', 'verify'])
    parser.add_argument('--work', type=Path, required=True)
    parser.add_argument('--app', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--identity')
    args = parser.parse_args()
    args.work = args.work.absolute()
    if args.mode == 'prepare' and (not args.app or not args.identity):
        parser.error('prepare requires --app and --identity')
    if args.mode in ['dmg', 'verify'] and not args.output:
        parser.error('dmg/verify requires --output')
    if args.mode == 'dmg' and not args.identity:
        parser.error('dmg requires --identity')
    args.work.mkdir(parents=True, exist_ok=True)
    {'prepare': prepare, 'dmg': make_dmg, 'verify': verify}[args.mode](args)


if __name__ == '__main__':
    main()
