#!/usr/bin/env python3
"""Collect original license texts for the locked Rust target dependency graph.

No license text is synthesized. Missing registry license texts fail collection.
The output is deterministic for a Cargo.lock, target and installed toolchain;
only public package names, versions, URLs and relative paths enter its index.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MARKER = 'skinos-third-party-licenses-v1'
MAX_FILE_BYTES = 2 * 1024 * 1024
MAX_TOTAL_BYTES = 32 * 1024 * 1024
MAX_ENTRIES = 50000
PREFIXES = ('LICENSE', 'LICENCE', 'COPYING', 'NOTICE', 'COPYRIGHT', 'UNLICENSE', 'AUTHORS')


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True)


def read_text_file(path, root):
    relative = path.relative_to(root)
    checked = root
    for part in relative.parts:
        checked = checked / part
        if checked.is_symlink():
            raise RuntimeError('License text cannot be a symlink: ' + relative.as_posix())
    if not path.is_file() or path.stat().st_size > MAX_FILE_BYTES:
        raise RuntimeError('License file is missing or exceeds size bound: ' + relative.as_posix())
    data = path.read_bytes()
    if not data.strip() or b'\0' in data:
        raise RuntimeError('License file is empty or binary: ' + relative.as_posix())
    data.decode('utf-8')
    return data


def license_files(root):
    result = {}
    visited = 0
    for directory, directories, files in os.walk(root, followlinks=False):
        directories[:] = sorted(name for name in directories if name not in ('.git', 'target', '__pycache__'))
        for name in sorted(files):
            visited += 1
            if visited > MAX_ENTRIES:
                raise RuntimeError('Package file inventory exceeds bounds')
            path = Path(directory) / name
            relative = path.relative_to(root)
            in_license_directory = any(part.upper().startswith(('LICENSE', 'LICENCE')) for part in relative.parts[:-1])
            if name.upper().startswith(PREFIXES) or in_license_directory:
                result[relative.as_posix()] = read_text_file(path, root)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True,
                        help='Generated app resource directory, e.g. APP/Contents/Resources/licenses/Rust')
    parser.add_argument('--target', help='Cargo target triple; defaults to the active rustc host')
    args = parser.parse_args()
    version = command('rustc', '--version').strip()
    host = next(line.split(': ', 1)[1] for line in command('rustc', '-vV').splitlines() if line.startswith('host: '))
    target = args.target or host
    if not re.fullmatch(r'[A-Za-z0-9_\-.]+', target):
        raise RuntimeError('Invalid target triple')
    lock_before = (ROOT / 'Cargo.lock').read_bytes()
    metadata = json.loads(command('cargo', 'metadata', '--locked', '--format-version', '1', '--filter-platform', target))
    if (ROOT / 'Cargo.lock').read_bytes() != lock_before:
        raise RuntimeError('Cargo.lock changed during license inventory')
    resolved = {node['id'] for node in metadata['resolve']['nodes']}
    packages = sorted((package for package in metadata['packages']
                       if package['id'] in resolved and (package.get('source') or '').startswith('registry+')),
                      key=lambda package: (package['name'], package['version']))
    records = []
    output_files = {}
    missing = []
    for package in packages:
        key = package['name'] + '-' + package['version']
        if not re.fullmatch(r'[A-Za-z0-9_.+\-]+', key):
            raise RuntimeError('Invalid registry package name or version')
        root = Path(package['manifest_path']).parent
        texts = license_files(root)
        if package.get('license_file'):
            file = root / package['license_file']
            if not file.resolve().is_relative_to(root.resolve()):
                raise RuntimeError('Package license_file escapes its source directory: ' + key)
            texts[file.relative_to(root).as_posix()] = read_text_file(file, root)
        # Attribution/NOTICE alone is not a substitute for a license grant.
        has_license = package.get('license_file') or any(
            any(part.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'UNLICENSE'))
                for part in Path(name).parts) for name in texts)
        if not texts or not has_license:
            missing.append(key)
            continue
        names = []
        for relative, data in sorted(texts.items()):
            destination = key + '/' + relative
            output_files[destination] = data
            names.append({'path': destination, 'sha256': hashlib.sha256(data).hexdigest()})
        records.append({'name': package['name'], 'version': package['version'],
                        'license': package.get('license'), 'repository': package.get('repository'),
                        'package_url': 'https://crates.io/crates/' + package['name'] + '/' + package['version'],
                        'files': names})
    if missing:
        raise RuntimeError('Registry packages missing original license texts; review upstream before release: ' + ', '.join(missing))
    sysroot = Path(command('rustc', '--print', 'sysroot').strip())
    toolchain_files = {}
    for relative in ['LICENSE-MIT', 'LICENSE-APACHE', 'COPYRIGHT',
                     'share/doc/rust/COPYRIGHT', 'share/doc/rust/LICENSE-MIT', 'share/doc/rust/LICENSE-APACHE',
                     'share/doc/rustc/COPYRIGHT-library.html']:
        path = sysroot / relative
        if path.is_file():
            toolchain_files[relative] = read_text_file(path, sysroot)
    # When rust-src is installed, retain its component notices as well as the
    # toolchain's generated standard-library copyright inventory.
    library = sysroot / 'lib/rustlib/src/rust/library'
    if library.is_dir():
        for relative, data in license_files(library).items():
            toolchain_files['library-source/' + relative] = data
    unwind = sysroot / 'lib/rustlib/src/rust/src/llvm-project/libunwind/LICENSE.TXT'
    if unwind.is_file():
        toolchain_files['libunwind/LICENSE.TXT'] = read_text_file(unwind, sysroot)
    for relative, data in sorted(toolchain_files.items()):
        output_files['RustStandardLibrary/' + relative] = data
    total_bytes = sum(len(data) for data in output_files.values())
    if total_bytes > MAX_TOTAL_BYTES:
        raise RuntimeError('Collected license text exceeds total size bound')
    inventory = {
        'generated_by': MARKER,
        'target': target,
        'cargo_lock_sha256': hashlib.sha256(lock_before).hexdigest(),
        'scope': 'Cargo resolved registry dependencies for this target, including build and test dependencies; not a claim that every listed package is linked into the final executable.',
        'packages': records,
        'rust_toolchain': {'version': version, 'source': 'https://github.com/rust-lang/rust',
                           'license_files_available': bool(toolchain_files),
                           'files': [{'path': 'RustStandardLibrary/' + name, 'sha256': hashlib.sha256(data).hexdigest()}
                                     for name, data in sorted(toolchain_files.items())]},
        'license_text_files': len(output_files),
        'license_text_bytes': total_bytes,
    }
    output_files['index.json'] = (json.dumps(inventory, indent=2, sort_keys=True) + '\n').encode()
    output_files['README.txt'] = (
        'Original third-party Rust license and copyright notices\n\n'
        'See index.json for target, Cargo.lock SHA-256, declared license expressions,\n'
        'upstream repository URLs and exact copied license-file hashes.\n'
        'License texts retain the bytes published in the resolved crate packages.\n'
        'The inventory includes build/test dependencies as well as runtime dependencies.\n'
        'SkinOS does not relicense these components under its own project license.\n'
    ).encode()
    output = args.output.absolute()
    if output.is_symlink():
        raise RuntimeError('Output must not be a symlink')
    if output.exists():
        index = output / 'index.json'
        if (not output.is_dir() or not index.is_file() or index.is_symlink()
                or json.loads(index.read_text()).get('generated_by') != MARKER):
            raise RuntimeError('Refusing to replace an output directory not generated by this collector')
        previous_inventory = json.loads(index.read_text())
        recorded_paths = {'index.json', 'README.txt'}
        for package in previous_inventory['packages']:
            recorded_paths.update(item['path'] for item in package['files'])
        recorded_paths.update(item['path'] for item in previous_inventory['rust_toolchain']['files'])
        old_files = [path for path in output.rglob('*') if path.is_file() or path.is_symlink()]
        if any(path.is_symlink() for path in output.rglob('*')):
            raise RuntimeError('Refusing symlink inside generated output')
        if any(path.relative_to(output).as_posix() not in recorded_paths for path in old_files):
            raise RuntimeError('Refusing to discard unrecognized files inside generated output')
        if ({path.relative_to(output).as_posix() for path in old_files} == set(output_files)
                and all((output / name).read_bytes() == data for name, data in output_files.items())):
            print(json.dumps({'packages': len(records), 'license_text_files': len(output_files) - 2,
                              'bytes': total_bytes, 'target': target, 'reused': True}))
            return
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.skinos-licenses-', dir=output.parent) as temporary:
        staged = Path(temporary) / 'Rust'
        staged.mkdir()
        for name, data in sorted(output_files.items()):
            path = staged / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        if output.exists():
            previous = Path(temporary) / 'previous'
            output.rename(previous)
            try:
                staged.rename(output)
            except BaseException:
                previous.rename(output)
                raise
        else:
            staged.rename(output)
    print(json.dumps({'packages': len(records), 'license_text_files': len(output_files) - 2,
                      'bytes': total_bytes, 'target': target, 'reused': False}))


if __name__ == '__main__':
    main()
