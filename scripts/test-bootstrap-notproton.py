#!/usr/bin/env python3
"""Offline bootstrap boundary tests; uses only synthetic ZIPs and temp files."""
import hashlib
import importlib.util
from pathlib import Path
import stat
import tempfile
import unittest
import zipfile

module = importlib.util.spec_from_file_location('bootstrap', Path(__file__).with_name('bootstrap-notproton.py'))
bootstrap = importlib.util.module_from_spec(module)
module.loader.exec_module(bootstrap)


class BootstrapTests(unittest.TestCase):
    def test_rejects_non_publisher_or_insecure_urls_and_paths(self):
        for url in ['http://github.com/a', 'https://evil.example/a',
                    'https://github.com.evil.example/a', 'https://user@github.com/a']:
            with self.assertRaises(RuntimeError):
                bootstrap.checked_url(url)
        for name in ['/absolute', '../outside', 'a/../b', 'a\\b', 'a//b', 'C:/outside']:
            with self.assertRaises(RuntimeError):
                bootstrap.safe_name(name)

    def test_exact_member_hash_is_required(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'fixture.zip'
            with zipfile.ZipFile(path, 'w') as archive:
                archive.writestr('allowed', b'hello')
            with zipfile.ZipFile(path) as archive:
                self.assertEqual(bootstrap.member_bytes(archive, 'allowed', hashlib.sha256(b'hello').hexdigest(), 5), b'hello')
                with self.assertRaises(RuntimeError):
                    bootstrap.member_bytes(archive, 'allowed', '0' * 64, 5)

    def test_source_does_not_overwrite_local_edits(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / 'fixture.zip'
            with zipfile.ZipFile(path, 'w') as archive:
                archive.writestr('source/a.swift', b'upstream')
            source = root / 'upstream'
            bootstrap.stage_source(path, source, {'prefix': 'source/'})
            (source / 'a.swift').write_bytes(b'my changes')
            with self.assertRaises(RuntimeError):
                bootstrap.stage_source(path, source, {'prefix': 'source/'})
            self.assertEqual((source / 'a.swift').read_bytes(), b'my changes')

    def test_rejects_archive_traversal_and_symlinks_before_publishing(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for name, mode in [('source/../outside', 0), ('source/link', stat.S_IFLNK | 0o777)]:
                path = root / 'fixture.zip'
                with zipfile.ZipFile(path, 'w') as archive:
                    entry = zipfile.ZipInfo(name)
                    entry.external_attr = mode << 16
                    archive.writestr(entry, b'outside')
                with self.assertRaises(RuntimeError):
                    bootstrap.stage_source(path, root / 'upstream', {'prefix': 'source/'})
                self.assertFalse((root / 'upstream').exists())
                self.assertFalse((root / 'outside').exists())

    def test_refuses_linked_output(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / 'outside').mkdir()
            (root / 'bridge').symlink_to(root / 'outside', target_is_directory=True)
            with self.assertRaises(RuntimeError):
                bootstrap.checked_path(root, 'bridge/member.gz')


if __name__ == '__main__':
    unittest.main()
