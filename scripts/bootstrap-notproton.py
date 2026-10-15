#!/usr/bin/env python3
"""Fetch the exact NotProton source and optional build inputs from upstream.

Only pinned HTTPS archives and their allowlisted members are used. This command
does not inspect installed Steam/NotProton applications or account directories.
Downloaded inputs stay in ignored local build directories, not public releases.
"""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import stat
import tempfile
import urllib.parse
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / 'third_party/notproton'
MAX_ARCHIVE = 512 * 1024 * 1024
MAX_MEMBER = 128 * 1024 * 1024
HOSTS = {
    'github.com', 'codeload.github.com', 'release-assets.githubusercontent.com',
    'objects.githubusercontent.com', 'client-update.akamai.steamstatic.com',
    'steamcdn-a.akamaihd.net', 'media.steampowered.com',
}


def checked_url(value):
    url = urllib.parse.urlsplit(value)
    if (url.scheme != 'https' or url.hostname not in HOSTS
            or url.username or url.password or url.fragment or url.port not in (None, 443)):
        raise RuntimeError('Unexpected upstream download URL')
    return value


class Redirects(urllib.request.HTTPRedirectHandler):
    max_redirections = 5

    def redirect_request(self, request, fp, code, message, headers, new_url):
        checked_url(new_url)
        return super().redirect_request(request, fp, code, message, headers, new_url)


def safe_name(value):
    if (not value or '\\' in value or '\0' in value or ':' in value
            or PurePosixPath(value).is_absolute()
            or any(part in ('', '.', '..') for part in value.split('/'))):
        raise RuntimeError('Unsafe archive or output path')
    return value


def checked_path(root, name):
    safe_name(name)
    path = root
    for part in [None, *PurePosixPath(name).parts]:
        if part is not None:
            path = path / part
        if path.is_symlink():
            raise RuntimeError('Refusing linked bootstrap path: ' + str(path))
    return path


def digest_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def valid_archive(path, spec):
    return (not path.is_symlink() and path.is_file()
            and path.stat().st_size == spec['size']
            and digest_file(path) == spec['sha256'])


def archive(cache, spec):
    if not 0 < spec['size'] <= MAX_ARCHIVE:
        raise RuntimeError('Archive pin exceeds download limit')
    digest = spec['sha256']
    if len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest):
        raise RuntimeError('Invalid archive hash')
    target = checked_path(cache, digest + '.zip')
    if valid_archive(target, spec):
        return target
    cache.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix='.notproton-', dir=cache)
    temporary = Path(temporary_name)
    try:
        request = urllib.request.Request(checked_url(spec['url']), headers={'User-Agent': 'SkinOS-dependency-bootstrap'})
        with os.fdopen(descriptor, 'wb') as output:
            with urllib.request.build_opener(Redirects()).open(request, timeout=60) as response:
                checked_url(response.geturl())
                total = 0
                while block := response.read(1024 * 1024):
                    total += len(block)
                    if total > spec['size']:
                        raise RuntimeError('Upstream archive exceeds its pinned size')
                    output.write(block)
            output.flush()
            os.fsync(output.fileno())
        if not valid_archive(temporary, spec):
            raise RuntimeError('Upstream archive size or SHA-256 mismatch')
        temporary.replace(target)
        return target
    finally:
        temporary.unlink(missing_ok=True)


def atomic_write(path, data, executable=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix='.notproton-', dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, 'wb') as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        temporary.chmod(0o755 if executable else 0o644)
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def member_bytes(bundle, name, expected_hash, expected_size):
    safe_name(name)
    matches = [entry for entry in bundle.infolist() if entry.filename == name]
    if len(matches) != 1:
        raise RuntimeError('Missing or duplicate pinned archive member: ' + name)
    entry = matches[0]
    if (entry.is_dir() or stat.S_ISLNK(entry.external_attr >> 16)
            or entry.file_size != expected_size or not 0 < expected_size <= MAX_MEMBER):
        raise RuntimeError('Invalid pinned archive member: ' + name)
    with bundle.open(entry) as stream:
        data = stream.read(expected_size + 1)
    if len(data) != expected_size or hashlib.sha256(data).hexdigest() != expected_hash:
        raise RuntimeError('Archive member checksum mismatch: ' + name)
    return data


def stage_source(source_archive, source, spec):
    # The complete source ZIP is pinned. Never extract filesystem paths or
    # permissions from it directly, even though it comes from upstream.
    with zipfile.ZipFile(source_archive) as bundle:
        if len(bundle.infolist()) > 10000:
            raise RuntimeError('Source archive has too many entries')
        files = {}
        seen = set()
        total = 0
        for entry in bundle.infolist():
            if not entry.filename.startswith(spec['prefix']):
                raise RuntimeError('Unexpected source archive root')
            relative = entry.filename.removeprefix(spec['prefix']).rstrip('/')
            if not relative:
                continue
            safe_name(relative)
            if relative.casefold() in seen:
                raise RuntimeError('Duplicate source archive path')
            seen.add(relative.casefold())
            mode = entry.external_attr >> 16
            if stat.S_ISLNK(mode):
                raise RuntimeError('Source archive contains a symlink')
            if entry.is_dir():
                continue
            total += entry.file_size
            if entry.file_size > MAX_MEMBER or total > MAX_ARCHIVE:
                raise RuntimeError('Source archive exceeds extraction bounds')
            with bundle.open(entry) as stream:
                data = stream.read(entry.file_size + 1)
            if len(data) != entry.file_size:
                raise RuntimeError('Invalid source member size')
            files[relative] = (data, bool(mode & 0o111))
    if source.exists() or source.is_symlink():
        if source.is_symlink() or not source.is_dir():
            raise RuntimeError('Source destination must be a regular directory')
        for name, (data, _) in files.items():
            path = checked_path(source, name)
            if not path.is_file() or path.read_bytes() != data:
                raise RuntimeError('Existing NotProton source differs from the pin; use a fresh --source-root instead of overwriting local edits')
        for path in source.rglob('*'):
            if path.is_symlink() or (path.is_file() and path.relative_to(source).as_posix() not in files):
                raise RuntimeError('Existing NotProton source contains extra files; use a fresh --source-root')
        return
    source.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.notproton-source-', dir=source.parent) as temporary:
        staged = Path(temporary) / 'upstream'
        staged.mkdir()
        for name, (data, executable) in files.items():
            atomic_write(checked_path(staged, name), data, executable)
        staged.rename(source)


def stage_dependencies(base, cache, pin, bridge_pin):
    files = pin['files']
    if ({name: spec['sha256'] for name, spec in files.items()} != bridge_pin['files']
            or pin['revision'] != bridge_pin['revision']
            or pin['version'] != bridge_pin['version']):
        raise RuntimeError('Dependency sources differ from the bridge manifest')
    destination = checked_path(base, 'bridge')
    opened = {}
    try:
        for name, spec in files.items():
            target = checked_path(destination, name + '.gz')
            if target.is_file():
                try:
                    with gzip.open(target, 'rb') as stream:
                        existing = stream.read(spec['size'] + 1)
                    if len(existing) == spec['size'] and hashlib.sha256(existing).hexdigest() == spec['sha256']:
                        continue
                except (OSError, EOFError):
                    pass
            key = spec['archive']
            if key not in opened:
                opened[key] = zipfile.ZipFile(archive(cache, pin['archives'][key]))
            data = member_bytes(opened[key], spec['member'], spec['sha256'], spec['size'])
            atomic_write(target, gzip.compress(data, compresslevel=6, mtime=0))
        manifest = checked_path(destination, 'manifest.json')
        encoded = (json.dumps(bridge_pin, indent=2) + '\n').encode()
        if not manifest.is_file() or manifest.read_bytes() != encoded:
            atomic_write(manifest, encoded)
    finally:
        for bundle in opened.values():
            bundle.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-root', type=Path, default=BASE,
                        help='Destination containing upstream/ and bridge/; existing changed source is never overwritten')
    parser.add_argument('--cache-dir', type=Path, default=ROOT / 'target/upstream-downloads')
    parser.add_argument('--source-only', action='store_true',
                        help='Fetch source only; omit local bridge/font build inputs')
    args = parser.parse_args()
    pin = json.loads((BASE / 'assets.json').read_text())
    info = json.loads((BASE / 'UPSTREAM.json').read_text())
    bridge_pin = json.loads((BASE / 'bridge/manifest.json').read_text())
    if pin['schema_version'] != 1 or any(pin[key] != info[key] for key in ('repository', 'revision', 'version')):
        raise RuntimeError('Source and dependency revisions disagree')
    base = args.source_root.absolute()
    cache = args.cache_dir.absolute()
    source = checked_path(base, 'upstream')
    stage_source(archive(cache, pin['source']), source, pin['source'])
    if not args.source_only:
        stage_dependencies(base, cache, pin, bridge_pin)
    print(json.dumps({'revision': pin['revision'], 'source_root': str(base),
                      'bridge_files': 0 if args.source_only else len(pin['files']),
                      'account_data_read': False}))


if __name__ == '__main__':
    main()
