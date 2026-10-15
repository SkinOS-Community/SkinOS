"""Offline launcher-handshake regressions. No Wine, SDK or user prefix is used."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('prepared_builder', ROOT/'scripts/build-notproton.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)
UPSTREAM = (ROOT/'third_party/notproton/upstream/dylib/feats/compat_run.sh').read_text()


def function(name):
    start = UPSTREAM.index(name+'() ')
    closing = ')' if UPSTREAM[start+len(name+'() ')] == '(' else '}'
    return UPSTREAM[start:UPSTREAM.index('\n'+closing, start)+2]


class Harness:
    def __init__(self, folder):
        self.folder = Path(folder)
        self.prefix = self.folder/'compat/pfx'
        self.runner = self.folder/'runner'
        self.support = self.folder/'support'
        self.tool = self.folder/'tool'
        self.receipt = self.folder/'receipt'
        self.actions = self.folder/'actions'
        for file in ['system.reg', 'drive_c/windows/system32/ntdll.dll', 'drive_c/windows/system32/winedevice.exe']:
            self.put(self.prefix/file, file)
        self.put(self.runner/'wine', 'loader')
        self.put(self.runner/'wineserver', 'server')
        self.put(self.runner/'share/wine/wine.inf', 'runner configuration')
        self.put(self.runner/'lib/wine/aarch64-windows/ntdll.dll', 'runner ntdll')
        self.put(self.tool/'run', 'pinned script')
        for name in ['steamclient64.dll', 'steamclient.dll', 'tier0_s64.dll', 'vstdlib_s64.dll', 'lsteamclient.dll', 'lsteamclient.so', 'steam.exe']:
            source = 'aarch64-unix/'+name if name == 'lsteamclient.so' else name
            self.put(self.support/'bridge'/source, name)
            self.put(self.prefix/'drive_c/Program Files (x86)/Steam'/name, name)
        self.env = dict(PATH=os.environ['PATH'], HOME=str(self.folder), ACTIONS=str(self.actions),
            STEAM_COMPAT_DATA_PATH=str(self.prefix.parent), WINEPREFIX=str(self.prefix),
            SKINOS_PREFIX_SESSION_RECEIPT=str(self.receipt), np_support=str(self.support),
            np_tool_dir=str(self.tool), np_build='fixture-build', np_flavor='fex',
            WINELOADER=str(self.runner/'wine'), WINESERVER=str(self.runner/'wineserver'),
            wine_unix=str(self.runner/'lib/wine/aarch64-unix'), CX_ROOT=str(self.runner),
            verb='waitforexitandrun', app_id='648800', log=str(self.folder/'run.log'))
        adapted = builder.prepared_prefix_adapter(UPSTREAM)
        start = adapted.index('# SkinOS prepared-prefix diagnostic handshake')
        end = adapted.index('\nbridge_src=', start)
        stubs = '''
action() { printf '%s\n' "$*" >> "$ACTIONS"; }
prepare_prefix_directory() { action lock; }
refuse_other_build() { action check_build; }
refuse_foreign_prefix() { action check_arch; }
claim_prefix() { action claim; }
without_lock_fds() { action "wine $*"; }
seed_prefix_from_template() { action seed; }
lay_out_proton_profile() { action profile; }
map_game_drive() { action drive; }
import_prefix_settings() { action settings; }
link_fonts() { action fonts; }
show_alert() { action alert; }
'''
        self.script = self.folder/'mock.sh'
        self.script.write_text(stubs+function('template_identity')+'\n'+adapted[start:end]+'\n'+builder.PREFIX_SESSION_PUBLISH+
            '\n[ -z "${SKINOS_PREFIX_SESSION_READY+x}" ] && [ -z "${SKINOS_PREFIX_SESSION_RECEIPT+x}" ] || exit 9\n')

    @staticmethod
    def put(path, data):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(data)

    def run(self, reuse=False, **changes):
        self.actions.write_text('')
        env = self.env.copy()
        if reuse: env['SKINOS_PREFIX_SESSION_READY'] = '1'
        env.update(changes)
        return subprocess.run(['/bin/sh', str(self.script)], env=env, capture_output=True, text=True, timeout=5)

    def calls(self):
        return self.actions.read_text().splitlines()


class PreparedSessionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.harness = Harness(self.temporary.name)

    def first(self):
        result = self.harness.run()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_first_preparation_is_complete_and_verified_reuse_has_no_mutations(self):
        h = self.harness
        self.first()
        self.assertEqual(h.calls(), ['lock', 'check_build', 'check_arch', 'claim',
            'wine '+str(h.runner/'wineserver')+' -k', 'seed', 'profile', 'drive', 'settings', 'fonts', 'drive'])
        receipt = h.receipt.read_bytes()
        self.assertRegex(receipt.decode(), r'^skinos-prefix-session-v1:[0-9a-f]{64}\n$')
        self.assertEqual(h.receipt.stat().st_mode & 0o777, 0o600)
        result = h.run(reuse=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(h.calls(), ['lock', 'check_build', 'check_arch'])
        self.assertEqual(h.receipt.read_bytes(), receipt)

    def test_missing_or_invalid_receipt_refuses_without_reset(self):
        h = self.harness
        for kind in ['missing', 'wrong_digest', 'wrong_mode', 'symlink']:
            with self.subTest(kind=kind):
                h.receipt.unlink(missing_ok=True)
                if kind != 'missing':
                    h.receipt.write_text('wrong digest\n')
                    h.receipt.chmod(0o644 if kind == 'wrong_mode' else 0o600)
                if kind == 'symlink':
                    target = h.folder/'other'
                    h.receipt.rename(target)
                    h.receipt.symlink_to(target)
                result = h.run(reuse=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(h.calls(), ['lock', 'check_build', 'check_arch'])

    def test_changed_effective_launch_settings_refuse_without_reset(self):
        self.first()
        for key, value in [('NOTPROTON_RETINA', '1'), ('NOTPROTON_RAW_CONTROLLERS', '1'),
                ('WINEMSYNC', '1'), ('STEAM_COMPAT_INSTALL_PATH', '/changed/game'),
                ('STEAM_COMPAT_LIBRARY_PATHS', '/changed/library'), ('CX_GRAPHICS_BACKEND', 'dxvk'),
                ('SDL_GAMECONTROLLER_IGNORE_DEVICES', 'changed'), ('np_build', 'other-build')]:
            with self.subTest(key=key):
                result = self.harness.run(reuse=True, **{key: value})
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.harness.calls(), ['lock', 'check_build', 'check_arch'])

    def test_changed_script_runner_ntdll_and_bridge_refuse_without_reset(self):
        h = self.harness
        self.first()
        for path in [h.tool/'run', h.runner/'wine', h.runner/'lib/wine/aarch64-windows/ntdll.dll',
                     h.prefix/'drive_c/windows/system32/ntdll.dll',
                     h.prefix/'drive_c/Program Files (x86)/Steam/lsteamclient.so']:
            with self.subTest(path=path.name):
                before = path.read_bytes()
                path.write_bytes(b'changed')
                result = h.run(reuse=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(h.calls(), ['lock', 'check_build', 'check_arch'])
                path.write_bytes(before)

    def test_ordinary_launch_without_receipt_keeps_normal_preparation(self):
        h = self.harness
        result = h.run(SKINOS_PREFIX_SESSION_RECEIPT='')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('seed', h.calls())
        self.assertFalse(h.receipt.exists())

    def test_invalid_marker_and_wrong_verb_fail_before_preparation(self):
        for changes in [dict(SKINOS_PREFIX_SESSION_READY='yes'), dict(verb='run')]:
            result = self.harness.run(**changes)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.harness.calls(), [])

    def test_publication_cannot_overwrite_an_existing_receipt(self):
        self.first()
        before = self.harness.receipt.read_bytes()
        result = self.harness.run()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.harness.receipt.read_bytes(), before)

    def test_upstream_preparation_and_settings_changes_require_review(self):
        for before, after in [
            ('if [ -n "$STEAM_COMPAT_DATA_PATH" ]; then\n  export WINEPREFIX=', 'if false; then\n  export WINEPREFIX='),
            ('  stage_step="prefix seed"', '  stage_step="changed seed"'),
            ('import_prefix_settings() {', 'import_prefix_settings() {\n  true'),
            ('export WINEDEBUG="${WINEDEBUG:-err+all,fixme-all}"', 'export WINEDEBUG=-all'),
        ]:
            with self.subTest(anchor=before):
                self.assertIn(before, UPSTREAM)
                with self.assertRaises(RuntimeError):
                    builder.prepared_prefix_adapter(UPSTREAM.replace(before, after, 1))


if __name__ == '__main__': unittest.main()
