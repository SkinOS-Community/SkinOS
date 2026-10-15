#!/usr/bin/env python3
"""Package only the exact pinned, account-free Valve binary/Input cohort.

Does not read current.json, Steam configuration, accounts, credentials or userdata.
payload.gz is a concatenation of files in manifest key order, not an archive with
executable extraction instructions or filesystem paths.
"""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PIN = ROOT / 'crates/runtime-host/resources/runtime-payload-pin.json'


def regular_file(root, name):
    path = root
    for component in Path(name).parts:
        path = path / component
        if path.is_symlink():
            raise RuntimeError('Linked payload component: ' + name)
    if not path.is_file():
        raise RuntimeError('Missing regular payload component: ' + name)
    return path


def checked_bytes(root, name, record):
    path = regular_file(root, name)
    if path.stat().st_size != record['size']:
        raise RuntimeError('Pinned payload size mismatch: ' + name)
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != record['sha256']:
        raise RuntimeError('Pinned payload checksum mismatch: ' + name)
    return data


def validate_payload(directory, pin_bytes, pin):
    if directory.is_symlink() or not directory.is_dir():
        return False
    try:
        if regular_file(directory, 'manifest.json').read_bytes() != pin_bytes:
            return False
        with gzip.open(regular_file(directory, 'payload.gz'), 'rb') as stream:
            for record in pin['files'].values():
                data = stream.read(record['size'])
                if len(data) != record['size'] or hashlib.sha256(data).hexdigest() != record['sha256']:
                    return False
            return not stream.read(1)
    except (OSError, EOFError, RuntimeError):
        return False


def main():
    pin_bytes = PIN.read_bytes()
    pin = json.loads(pin_bytes)
    parser = argparse.ArgumentParser(description=__doc__)
    default = Path.home() / 'Library/Application Support/SkinOs/runtime-source' / pin['runtime_sha256']
    parser.add_argument('--source', type=Path, default=Path(os.environ.get('SKINOS_RUNTIME_SOURCE', str(default))))
    parser.add_argument('--output-dir', type=Path, default=ROOT / 'target/runtime-payload')
    args = parser.parse_args()
    source = args.source
    if source.is_symlink() or not source.is_dir():
        raise RuntimeError('Runtime source must be a regular dedicated directory')
    # Validate every named source before changing the last good packaged payload.
    for name, record in pin['files'].items():
        checked_bytes(source, name, record)
    for name, target in pin['symlinks'].items():
        link = source / name
        if not link.is_symlink() or str(link.readlink()) != target:
            raise RuntimeError('Pinned framework link mismatch: ' + name)
    destination = args.output_dir
    if destination.is_symlink():
        raise RuntimeError('Runtime payload destination must not be a symlink')
    if not validate_payload(destination, pin_bytes, pin):
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix='.runtime-payload-', dir=destination.parent) as temporary:
            staged = Path(temporary) / 'payload'
            staged.mkdir()
            (staged / 'manifest.json').write_bytes(pin_bytes)
            with (staged / 'payload.gz').open('wb') as raw:
                with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0, compresslevel=6) as stream:
                    for name, record in pin['files'].items():
                        stream.write(checked_bytes(source, name, record))
            if not validate_payload(staged, pin_bytes, pin):
                raise RuntimeError('Generated payload verification failed')
            if destination.exists():
                shutil.rmtree(destination)
            staged.rename(destination)
    print(json.dumps({'runtime_sha256': pin['runtime_sha256'], 'file_count': len(pin['files']),
                      'symlink_count': len(pin['symlinks']), 'account_data_copied': False,
                      'compressed_bytes': (destination / 'payload.gz').stat().st_size}))


if __name__ == '__main__':
    main()
