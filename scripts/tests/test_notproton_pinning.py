#!/usr/bin/env python3
"""Offline regressions for immutable NotProton bridge pins and update rollback."""
import gzip
import hashlib
import importlib.util
import json
import pathlib
import shutil
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


builder = load('bridge_builder_under_test', ROOT / 'scripts/build-notproton.py')
updater = load('bridge_updater_under_test', ROOT / 'scripts/update-notproton.py')
OLD = '1' * 40
NEW = '2' * 40


class NotProtonPinTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.base = self.root / 'third_party/notproton'
        (self.base / 'upstream/dylib').mkdir(parents=True)
        (self.base / 'upstream/dylib/version.h').write_text('#define NOTPROTON_VERSION "1.0"\n')
        (self.base / 'integration').mkdir()
        (self.base / 'integration/marker').write_text('keep integration')
        (self.root / 'scripts').mkdir()
        shutil.copy2(ROOT / 'scripts/build-notproton.py', self.root / 'scripts/build-notproton.py')
        self.metadata = dict(repository=updater.REPOSITORY, revision=OLD, version='1.0', integration_version=1)
        (self.base / 'UPSTREAM.json').write_text(json.dumps(self.metadata))
        self.old_raw = self.create_bridge('old-raw', b'old ')
        self.new_raw = self.create_bridge('new-raw', b'new ')
        builder.stage_bridge_pin(self.old_raw, self.base / 'bridge', self.metadata)
        self.old_payload = self.root / 'target/notproton'
        self.old_payload.mkdir(parents=True)
        (self.old_payload / 'marker').write_bytes(b'old payload must survive')
        self.checkout = self.root / 'checkout'
        (self.checkout / 'dylib').mkdir(parents=True)
        (self.checkout / 'dylib/version.h').write_text('#define NOTPROTON_VERSION "2.0"\n')
        self.before = self.snapshot()

    def create_bridge(self, directory, prefix):
        root = self.root / directory
        for name in builder.BRIDGE_NAMES:
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(prefix + name.encode())
        for name in builder.REQUIRED_FONT_NAMES:
            path = root / 'fonts' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(prefix + name.encode())
        return root

    def snapshot(self):
        return {str(p.relative_to(self.root)): p.read_bytes()
                for root in (self.base, self.old_payload) for p in root.rglob('*') if p.is_file()}

    def fake_validator(self, project_root, candidate, payload):
        # Even while building a new revision, the live source and payload are old.
        self.assertEqual(self.snapshot(), self.before)
        metadata = json.loads((candidate / 'UPSTREAM.json').read_text())
        _, manifest = builder.read_pinned_bridge(candidate / 'bridge', metadata)
        payload.mkdir()
        for name in ('skinos-notproton-setup', 'sdk-probe.exe', 'sdk-probe32.exe', 'run'):
            (payload / name).write_bytes(b'mocked compiler output')
        shutil.copy2(candidate / 'UPSTREAM.json', payload / 'UPSTREAM.json')
        shutil.copytree(candidate / 'bridge', payload / 'bridge')
        font_names = set(manifest.get('font_files', []))
        (payload / 'bridge-manifest.json').write_text(json.dumps(dict(
            encoding='gzip', revision=metadata['revision'], version=metadata['version'],
            files={name: digest for name, digest in manifest['files'].items() if name not in font_names})))
        fonts = {}
        for name in font_names:
            destination = payload / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(gzip.decompress((candidate / 'bridge' / (name + '.gz')).read_bytes()))
            fonts[name.removeprefix('fonts/')] = manifest['files'][name]
        (payload / 'fonts-manifest.json').write_text(json.dumps(dict(
            schema_version=1, revision=metadata['revision'], version=metadata['version'], files=fonts)))

    def update(self, validator=None):
        return updater.update_from_checkout(self.checkout, NEW, self.new_raw, self.root,
                                            validator=validator or self.fake_validator)

    def test_success_persists_new_bridge_for_all_subsequent_builds(self):
        metadata, backup = self.update()
        self.assertEqual(metadata['revision'], NEW)
        self.assertTrue((backup / 'payload/marker').is_file())
        # Both original external locations can disappear. Rebuild input remains complete.
        shutil.rmtree(self.new_raw)
        shutil.rmtree(self.old_raw)
        data, manifest = builder.read_pinned_bridge(self.base / 'bridge', metadata)
        self.assertEqual(manifest['revision'], NEW)
        self.assertEqual(data['steam.exe'], b'new steam.exe')
        updater.verify_payload(self.old_payload, metadata, manifest)
        self.assertEqual((self.base / 'integration/marker').read_text(), 'keep integration')

    def test_failed_build_leaves_source_pin_and_payload_byte_identical(self):
        def fail(project_root, candidate, payload):
            self.fake_validator(project_root, candidate, payload)
            raise RuntimeError('mocked compiler failed')
        with self.assertRaisesRegex(RuntimeError, 'compiler failed'):
            self.update(fail)
        self.assertEqual(self.snapshot(), self.before)

    def test_bad_candidate_payload_never_publishes(self):
        def corrupt(project_root, candidate, payload):
            self.fake_validator(project_root, candidate, payload)
            (payload / 'bridge/steam.exe.gz').write_bytes(gzip.compress(b'wrong artifact'))
        with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
            self.update(corrupt)
        self.assertEqual(self.snapshot(), self.before)

    def test_bad_candidate_font_never_publishes(self):
        def corrupt(project_root, candidate, payload):
            self.fake_validator(project_root, candidate, payload)
            (payload / 'fonts/alt/arial.ttf').write_bytes(b'wrong font')
        with self.assertRaisesRegex(RuntimeError, 'font checksum mismatch'):
            self.update(corrupt)
        self.assertEqual(self.snapshot(), self.before)

    def test_font_licenses_and_variants_survive_source_cache_removal(self):
        metadata, _ = self.update()
        shutil.rmtree(self.new_raw)
        artifacts, manifest = builder.read_pinned_bridge(self.base / 'bridge', metadata)
        self.assertEqual(artifacts['fonts/alt/arial.ttf'], b'new alt/arial.ttf')
        self.assertEqual(artifacts['fonts/noto/LICENSE'], b'new noto/LICENSE')
        updater.verify_payload(self.old_payload, metadata, manifest)

    def test_font_inventory_cannot_escape_payload(self):
        manifest = json.loads((self.base / 'bridge/manifest.json').read_text())
        for name in ['fonts/../../outside', '/fonts/arial.ttf', 'fonts/./arial.ttf', 'fonts\\arial.ttf']:
            with self.subTest(name=name), self.assertRaisesRegex(RuntimeError, 'Unsafe pinned font path'):
                builder.font_names(dict(manifest, font_files=[name]))

    def test_missing_font_or_license_cannot_publish(self):
        (self.new_raw / 'fonts/alt/arial.ttf').unlink()
        with self.assertRaisesRegex(RuntimeError, 'complete Proton font and license set'):
            self.update()
        self.assertEqual(self.snapshot(), self.before)

    def test_publication_failure_restores_all_four_previous_components(self):
        real_move = updater.move
        def fail_final_publish(source, destination):
            if destination == self.old_payload and source.name == 'payload' and '.notproton-candidate-' in str(source):
                raise OSError('mocked payload rename failed')
            real_move(source, destination)
        with mock.patch.object(updater, 'move', side_effect=fail_final_publish):
            with self.assertRaisesRegex(OSError, 'rename failed'):
                self.update()
        self.assertEqual(self.snapshot(), self.before)

    def test_partial_new_bridge_cannot_change_pin(self):
        (self.new_raw / 'steamclient64.dll').unlink()
        with self.assertRaises(FileNotFoundError):
            self.update()
        self.assertEqual(self.snapshot(), self.before)

    def test_revision_mismatch_fails_closed(self):
        with self.assertRaisesRegex(RuntimeError, 'revision does not match'):
            builder.read_pinned_bridge(self.base / 'bridge', dict(self.metadata, revision=NEW))

    def test_tampered_pin_cannot_build(self):
        path = self.base / 'bridge/lsteamclient.so.gz'
        path.write_bytes(gzip.compress(b'tampered'))
        with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
            builder.read_pinned_bridge(self.base / 'bridge', self.metadata)

    def test_release_provenance_is_published_with_exact_artifact_hashes(self):
        provenance = dict(repository=updater.REPOSITORY, revision=NEW, version='2.0',
                          kind='official-release-fixture', asset_sha256='a' * 64,
                          files={path.relative_to(self.new_raw).as_posix(): {'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
                                 for path in self.new_raw.rglob('*') if path.is_file()})
        metadata, _ = updater.update_from_checkout(self.checkout, NEW, self.new_raw, self.root,
            validator=self.fake_validator, bridge_provenance=provenance)
        _, manifest = builder.read_pinned_bridge(self.base / 'bridge', metadata)
        self.assertEqual(manifest['provenance'], provenance)

    def test_wrong_release_provenance_cannot_change_any_live_component(self):
        for revision, file_hash in [(OLD, None), (NEW, '0' * 64)]:
            provenance = dict(repository=updater.REPOSITORY, revision=revision, version='2.0',
                              files={path.relative_to(self.new_raw).as_posix(): {'sha256': file_hash or hashlib.sha256(path.read_bytes()).hexdigest()}
                                     for path in self.new_raw.rglob('*') if path.is_file()})
            with self.assertRaisesRegex(RuntimeError, 'provenance'):
                updater.update_from_checkout(self.checkout, NEW, self.new_raw, self.root,
                    validator=self.fake_validator, bridge_provenance=provenance)
            self.assertEqual(self.snapshot(), self.before)


if __name__ == '__main__':
    unittest.main()
