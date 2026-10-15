#!/usr/bin/env python3
"""Copy matching public Valve input components into SkinOS's private source.

No Steam account/config/userdata is read. Nothing is executed and the original
Steam installation is never modified. The engine fingerprint must match first.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import time

ENGINE = '035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840'
COMPONENTS = {
    'libSDL3.dylib': '56fad2994d5c1c618862459d3aaa6e7c6048a83f1a4dc3e3ef543edf2272dfed',
    'libsteaminput.dylib': '7b09276160dfc42d1b8bb950a70b2f6a6802fa322728fe958f5ae4761b6eb585',
}


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def stage(source, owned_root):
    source = source.resolve(strict=True)
    owned_root = owned_root.resolve(strict=True)
    current = json.loads((owned_root / 'current.json').read_text())
    destination = Path(current['runtime_directory']).resolve(strict=True)
    if not destination.is_relative_to(owned_root) or current['runtime_sha256'] != ENGINE:
        raise RuntimeError('Owned engine identity/path differs from the audited version')
    if digest(source / 'steamclient.dylib') != ENGINE or digest(destination / 'steamclient.dylib') != ENGINE:
        raise RuntimeError('Source and private engines must both match the audited fingerprint')
    files = {}
    for name, expected in COMPONENTS.items():
        path = source / name
        if path.is_symlink() or digest(path) != expected:
            raise RuntimeError('Input component does not match the audited engine cohort: ' + name)
        files[name] = expected
    controllers = source / 'controller_base'
    if controllers.is_symlink() or not controllers.is_dir():
        raise RuntimeError('Missing regular controller_base resource directory')
    for path in sorted(controllers.rglob('*')):
        if path.is_symlink():
            raise RuntimeError('Linked controller resources are not accepted')
        if path.is_dir():
            continue
        if not path.is_file():
            raise RuntimeError('Unsupported controller resource kind')
        name = path.relative_to(source).as_posix()
        files[name] = digest(path)
    if not 2 < len(files) <= 4000:
        raise RuntimeError('Unexpected controller resource count')
    manifest = dict(schema_version=1, runtime_sha256=ENGINE,
                    source_kind='user_installed_valve_binaries', account_data_copied=False,
                    files=files)
    manifest_path = destination / 'steam-input-source.json'
    if manifest_path.is_file() and json.loads(manifest_path.read_text()) == manifest:
        if all((destination / name).is_file() and not (destination / name).is_symlink()
               and digest(destination / name) == expected for name, expected in files.items()):
            return dict(changed=False, file_count=len(files), directory=str(destination))
    with tempfile.TemporaryDirectory(prefix='.steam-input-stage-', dir=owned_root) as temporary:
        candidate = Path(temporary)
        for name, expected in files.items():
            target = candidate / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / name, target)
            if digest(target) != expected:
                raise RuntimeError('Source changed while copying: ' + name)
        (candidate / 'steam-input-source.json').write_text(json.dumps(manifest, indent=2) + '\n')
        backup = owned_root / 'input-backups' / str(time.time_ns())
        backup.mkdir(parents=True)
        moved = []
        try:
            for name in [*COMPONENTS, 'controller_base', 'steam-input-source.json']:
                current_path = destination / name
                prior = backup / name
                had_previous = current_path.exists() or current_path.is_symlink()
                if had_previous:
                    current_path.rename(prior)
                moved.append((current_path, prior, had_previous, False))
                (candidate / name).rename(current_path)
                moved[-1] = (current_path, prior, had_previous, True)
        except BaseException:
            for path, prior, had_previous, installed in reversed(moved):
                if installed:
                    if path.is_dir() and not path.is_symlink():
                        shutil.rmtree(path)
                    else:
                        path.unlink()
                if had_previous:
                    prior.rename(path)
            raise
    return dict(changed=True, file_count=len(files), directory=str(destination), backup=str(backup))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True, help='Matching Steam Contents/MacOS binary directory')
    parser.add_argument('--owned-root', type=Path,
                        default=Path.home() / 'Library/Application Support/SkinOs/runtime-source')
    args = parser.parse_args()
    print(json.dumps(stage(args.source, args.owned_root), indent=2))


if __name__ == '__main__':
    main()
