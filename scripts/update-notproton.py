#!/usr/bin/env python3
"""Transactionally refresh NotProton source, vendored bridge pin and built payload.

Usage: scripts/update-notproton.py --revision COMMIT --bridge-source DIR
DIR must contain bridge binaries built from that exact upstream revision. All
candidate work happens separately; failed builds leave the current pin and
payload untouched. Failed publication restores all four previous components.
"""
import argparse
import fcntl
import gzip
import hashlib
import importlib.util
import json
import pathlib
import re
import shutil
import subprocess
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
REPOSITORY = 'https://github.com/NotProtonNot/NotProton'


def builder_module(project_root=ROOT):
    spec = importlib.util.spec_from_file_location('skinos_notproton_builder', project_root / 'scripts/build-notproton.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(project_root, *args):
    subprocess.run(list(map(str, args)), cwd=project_root, check=True)


def validate_candidate(project_root, candidate, payload):
    run(project_root, 'python3', project_root / 'scripts/build-notproton.py',
        '--source-root', candidate, '--output-dir', payload)
    run(project_root, 'cargo', 'test', '-p', 'skinos-compat')
    run(project_root, 'cargo', 'clippy', '-p', 'skinos-compat', '--all-targets', '--', '-D', 'warnings')


def verify_payload(payload, metadata, pinned_manifest):
    for name in ('skinos-notproton-setup', 'sdk-probe.exe', 'sdk-probe32.exe', 'run', 'UPSTREAM.json', 'bridge-manifest.json', 'fonts-manifest.json'):
        if not (payload / name).is_file():
            raise RuntimeError('Candidate payload is incomplete: ' + name)
    if json.loads((payload / 'UPSTREAM.json').read_text()) != metadata:
        raise RuntimeError('Candidate payload source metadata differs from the candidate pin')
    manifest = json.loads((payload / 'bridge-manifest.json').read_text())
    fonts = set(pinned_manifest.get('font_files', []))
    expected_bridge = {name: digest for name, digest in pinned_manifest['files'].items() if name not in fonts}
    if (manifest.get('encoding') != 'gzip'
            or manifest.get('revision') != metadata['revision']
            or manifest.get('version') != metadata['version']
            or manifest.get('files') != expected_bridge):
        raise RuntimeError('Candidate payload bridge pin differs from the candidate source')
    for name, expected in manifest['files'].items():
        data = gzip.decompress((payload / 'bridge' / (name + '.gz')).read_bytes())
        if hashlib.sha256(data).hexdigest() != expected:
            raise RuntimeError('Candidate payload bridge checksum mismatch: ' + name)
    font_manifest = json.loads((payload / 'fonts-manifest.json').read_text())
    expected_fonts = {name.removeprefix('fonts/'): pinned_manifest['files'][name] for name in fonts}
    if (font_manifest.get('schema_version') != 1
            or font_manifest.get('revision') != metadata['revision']
            or font_manifest.get('version') != metadata['version']
            or font_manifest.get('files') != expected_fonts):
        raise RuntimeError('Candidate payload font pin differs from the candidate source')
    for name, expected in expected_fonts.items():
        path = payload / 'fonts' / name
        if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise RuntimeError('Candidate payload font checksum mismatch: ' + name)


def move(source, destination):
    source.rename(destination)


def publish_transaction(replacements, backup_directory):
    """Move checked candidates into place and restore every old component on error."""
    backup_directory.mkdir(parents=True, exist_ok=False)
    journal = backup_directory / 'transaction.json'
    entries = []
    try:
        for label, source, destination in replacements:
            entry = {'label': label, 'destination': destination, 'backup': backup_directory / label,
                     'old_moved': False, 'new_moved': False}
            entries.append(entry)
            if destination.exists() or destination.is_symlink():
                move(destination, entry['backup'])
                entry['old_moved'] = True
            move(source, destination)
            entry['new_moved'] = True
            journal.write_text(json.dumps([dict(e, destination=str(e['destination']), backup=str(e['backup'])) for e in entries], indent=2) + '\n')
        journal.write_text(json.dumps({'status': 'published', 'previous_components': [entry['label'] for entry in entries if entry['old_moved']]}, indent=2) + '\n')
    except BaseException:
        rollback_errors = []
        for entry in reversed(entries):
            try:
                if entry['new_moved']:
                    current = entry['destination']
                    if current.is_dir() and not current.is_symlink():
                        shutil.rmtree(current)
                    else:
                        current.unlink()
                if entry['old_moved']:
                    move(entry['backup'], entry['destination'])
            except OSError as error:
                rollback_errors.append(str(error))
        if rollback_errors:
            raise RuntimeError('Rollback needs recovery from ' + str(backup_directory) + ': ' + '; '.join(rollback_errors))
        journal.write_text(json.dumps({'status': 'rolled-back'}, indent=2) + '\n')
        raise


def update_from_checkout(checkout, revision, bridge_source, project_root=ROOT, validator=None, bridge_provenance=None):
    """Build and validate a detached candidate, then publish source/pin/payload together."""
    if not re.fullmatch('[0-9a-f]{40}', revision):
        raise ValueError('An exact full upstream commit is required')
    base = project_root / 'third_party/notproton'
    target = project_root / 'target'
    target.mkdir(parents=True, exist_ok=True)
    previous_metadata = json.loads((base / 'UPSTREAM.json').read_text())
    builder = builder_module(project_root)
    with tempfile.TemporaryDirectory(prefix='.notproton-candidate-', dir=target) as temp:
        workspace = pathlib.Path(temp)
        candidate = workspace / 'source'
        candidate.mkdir()
        shutil.copytree(checkout, candidate / 'upstream', ignore=shutil.ignore_patterns('.git'))
        shutil.copytree(base / 'integration', candidate / 'integration')
        version_text = (candidate / 'upstream/dylib/version.h').read_text()
        match = re.search(r'#define NOTPROTON_VERSION "([^"]+)"', version_text)
        if not match:
            raise RuntimeError('Upstream version definition changed; inspect before updating')
        metadata = dict(previous_metadata, repository=REPOSITORY, revision=revision, version=match.group(1),
            bridge_artifacts='Pinned gzip artifacts in bridge/manifest.json; source revision/version and SHA-256 validated before every build. Update source and artifacts together with scripts/update-notproton.py.')
        (candidate / 'UPSTREAM.json').write_text(json.dumps(metadata, indent=2) + '\n')
        pin = builder.stage_bridge_pin(bridge_source, candidate / 'bridge', metadata, bridge_provenance)
        payload = workspace / 'payload'
        (validator or validate_candidate)(project_root, candidate, payload)
        verify_payload(payload, metadata, pin)
        backup = target / 'notproton-update-backups' / (str(time.time_ns()) + '-' + previous_metadata['revision'][:12])
        publish_transaction([
            ('upstream', candidate / 'upstream', base / 'upstream'),
            ('bridge', candidate / 'bridge', base / 'bridge'),
            ('UPSTREAM.json', candidate / 'UPSTREAM.json', base / 'UPSTREAM.json'),
            ('payload', payload, target / 'notproton'),
        ], backup)
    return metadata, backup


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--revision', required=True, help='Exact full 40-character upstream Git commit')
    parser.add_argument('--bridge-source', type=pathlib.Path, required=True,
        help='Bridge binaries built from this same revision; never silently reuse the old binaries')
    parser.add_argument('--bridge-provenance', type=pathlib.Path,
        help='Optional release provenance JSON with matching repository, revision, version and per-file SHA-256')
    args = parser.parse_args()
    if not re.fullmatch('[0-9a-f]{40}', args.revision):
        parser.error('--revision must be a complete immutable Git commit')
    bridge = args.bridge_source.resolve(strict=True)
    provenance = json.loads(args.bridge_provenance.read_text()) if args.bridge_provenance else None
    target = ROOT / 'target'
    target.mkdir(parents=True, exist_ok=True)
    with (target / '.notproton-update.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise RuntimeError('Another NotProton update is already running')
        with tempfile.TemporaryDirectory(prefix='.notproton-checkout-', dir=target) as temp:
            checkout = pathlib.Path(temp) / 'upstream'
            run(ROOT, 'git', 'clone', '--filter=blob:none', '--no-checkout', REPOSITORY + '.git', checkout)
            run(ROOT, 'git', '-C', checkout, 'checkout', '--detach', args.revision)
            resolved = subprocess.check_output(['git', '-C', str(checkout), 'rev-parse', 'HEAD'], text=True).strip()
            if resolved != args.revision:
                raise RuntimeError('Upstream revision mismatch')
            metadata, backup = update_from_checkout(checkout, resolved, bridge, bridge_provenance=provenance)
        print('Pinned NotProton', metadata['revision'], 'version', metadata['version'])
        print('Previous source, bridge pin, metadata and payload retained at', backup)
        print('Every future build uses the new vendored bridge; installed NotProton is never consulted.')
        print('Next: rebuild SkinOS and run the actual Windows SDK/Soundpad integration check.')


if __name__ == '__main__':
    main()
