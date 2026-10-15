#!/usr/bin/env python3
"""Offline regressions: SDK version negotiation, proof boundaries and PE ABI."""
import importlib.util
import json
import os
import pathlib
import struct
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('probe_builder', ROOT / 'scripts/build-notproton.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class SdkProbeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.directory = pathlib.Path(cls.temp.name)
        cls.harness = cls.directory / 'probe-fixture'
        subprocess.run(['clang', '-O2', '-Wno-missing-noreturn', '-o', str(cls.harness),
                        str(ROOT / 'scripts/tests/fixtures/sdk-probe-harness.c')], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def probe(self, mode, expected='76561198000000001', sdk='fixture.dll', client=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith('SKINOS_')}
        env.update(TEST_CASE=mode, SKINOS_EXPECTED_STEAMID=expected)
        if sdk is not None: env['SKINOS_WINDOWS_SDK'] = sdk
        if client is not None: env['SKINOS_WINDOWS_CLIENT'] = client
        result = subprocess.run([str(self.harness)], env=env, capture_output=True, text=True)
        return result.returncode, json.loads(result.stdout)

    def test_classic_init_and_v021_match_actual_goldsrc_exports(self):
        code, proof = self.probe('classic')
        self.assertEqual(code, 0)
        self.assertTrue(proof['ok'])
        self.assertEqual(proof['proof_kind'], 'game_sdk')
        self.assertEqual(proof['init_method'], 'SteamAPI_Init')
        self.assertTrue(proof['sdk_initialized'])
        self.assertEqual(proof['steamid'], '76561198000000001')

    def test_modern_sdk_keeps_initflat(self):
        code, proof = self.probe('modern')
        self.assertEqual(code, 0)
        self.assertEqual(proof['init_method'], 'SteamAPI_InitFlat')

    def test_callbacks_settle_real_sdk_readiness(self):
        for mode in ['online_delayed', 'identity_delayed', 'client_delayed']:
            with self.subTest(mode=mode):
                code, proof = self.probe(mode)
                self.assertEqual(code, 0)
                self.assertTrue(proof['ok'])
                self.assertTrue(proof['logged_on'])
                self.assertTrue(proof['expected_matches'])
                self.assertEqual(proof['readiness_reason'], 'ready')
                self.assertEqual(proof['callback_pumps'], 3)
                self.assertEqual(proof['readiness_elapsed_ms'], 75)

    def test_persistent_offline_exhausts_bounded_readiness_wait(self):
        code, proof = self.probe('offline')
        self.assertEqual(code, 3)
        self.assertFalse(proof['logged_on'])
        self.assertTrue(proof['expected_matches'])
        self.assertEqual(proof['readiness_reason'], 'sdk_readiness_timeout')
        self.assertEqual(proof['readiness_elapsed_ms'], 10000)
        self.assertEqual(proof['callback_pumps'], 400)

    def test_online_without_identity_is_not_proof(self):
        code, proof = self.probe('identity_missing')
        self.assertEqual(code, 3)
        self.assertTrue(proof['logged_on'])
        self.assertFalse(proof['expected_matches'])
        self.assertFalse(proof['ok'])
        self.assertEqual(proof['readiness_reason'], 'sdk_readiness_timeout')
        self.assertEqual(proof['readiness_elapsed_ms'], 10000)

    def test_wrong_nonzero_identity_never_waits_for_another_account(self):
        code, proof = self.probe('online_delayed', '76561198000000002')
        self.assertEqual(code, 3)
        self.assertFalse(proof['expected_matches'])
        self.assertEqual(proof['readiness_reason'], 'identity_mismatch')
        self.assertEqual(proof['readiness_elapsed_ms'], 0)
        self.assertEqual(proof['callback_pumps'], 0)
        code, proof = self.probe('wrong_after_callback')
        self.assertEqual(code, 3)
        self.assertEqual(proof['readiness_reason'], 'identity_mismatch')
        self.assertEqual(proof['callback_pumps'], 1)

    def test_offline_sdk_without_callback_export_remains_failed(self):
        code, proof = self.probe('callbacks_missing')
        self.assertEqual(code, 3)
        self.assertFalse(proof['ok'])
        self.assertEqual(proof['readiness_reason'], 'sdk_callbacks_export_missing')
        self.assertEqual(proof['readiness_elapsed_ms'], 0)
        self.assertEqual(proof['callback_pumps'], 0)

    def test_identity_mismatch_and_offline_stay_failed(self):
        for mode, expected in [('classic', '76561198000000002'), ('offline', '76561198000000001')]:
            with self.subTest(mode=mode):
                code, proof = self.probe(mode, expected)
                self.assertEqual(code, 3)
                self.assertFalse(proof['ok'])

    def test_invalid_expected_identity_is_not_truncated_to_valid_prefix(self):
        for expected in ['76561198000000001extra', '18446744073709551616', '0', '']:
            code, proof = self.probe('classic', expected)
            self.assertEqual(code, 1)
            self.assertEqual(proof['reason'], 'missing_expected_identity')

    def test_broken_game_sdk_never_falls_back_to_working_bridge(self):
        code, proof = self.probe('broken', sdk='broken.dll', client='fixture-client.dll')
        self.assertEqual(code, 1)
        self.assertEqual(proof['reason'], 'load_sdk_failed')

    def test_failed_classic_init_cannot_claim_success(self):
        code, proof = self.probe('init_failure')
        self.assertEqual(code, 1)
        self.assertEqual(proof['reason'], 'sdk_classic_init_failed')

    def test_sdkless_bridge_proof_is_labelled_separately(self):
        code, proof = self.probe('bridge', sdk=None, client='fixture-client.dll')
        self.assertEqual(code, 0)
        self.assertEqual(proof['proof_kind'], 'steamclient_bridge')
        self.assertTrue(proof['bridge_initialized'])
        self.assertNotIn('sdk_initialized', proof)

    def test_compiled_probes_have_both_pe_widths_and_undecorated_win32_imports(self):
        sysroot = pathlib.Path(subprocess.check_output(['rustc', '--print', 'sysroot'], text=True).strip())
        host = next(line.split(': ', 1)[1] for line in subprocess.check_output(['rustc', '-vV'], text=True).splitlines() if line.startswith('host: '))
        candidates = [sysroot / 'lib/rustlib' / host / 'bin/rust-lld'] + list((pathlib.Path.home() / '.rustup/toolchains').glob('*/lib/rustlib/' + host + '/bin/rust-lld'))
        lld = next((p for p in candidates if p.is_file()), None)
        self.assertIsNotNone(lld, 'Windows probe build requires rust-lld')
        builder.build_sdk_probes(builder.BASE, self.directory, lld)
        for name, machine, magic, width, directories in [('sdk-probe32.exe', 0x14c, 0x10b, 4, 96), ('sdk-probe.exe', 0x8664, 0x20b, 8, 112)]:
            data = (self.directory / name).read_bytes()
            pe = struct.unpack_from('<I', data, 0x3c)[0]
            self.assertEqual(data[pe:pe + 4], b'PE\0\0')
            self.assertEqual(struct.unpack_from('<H', data, pe + 4)[0], machine)
            optional = pe + 24
            self.assertEqual(struct.unpack_from('<H', data, optional)[0], magic)
            count = struct.unpack_from('<H', data, pe + 6)[0]
            sections = optional + struct.unpack_from('<H', data, pe + 20)[0]
            def offset(rva):
                for i in range(count):
                    size, address, raw_size, raw = struct.unpack_from('<IIII', data, sections + 40 * i + 8)
                    if address <= rva < address + max(size, raw_size): return raw + rva - address
                raise AssertionError('unmapped PE RVA')
            def cstr(at): return data[at:data.index(b'\0', at)].decode('ascii')
            imports_rva = struct.unpack_from('<I', data, optional + directories + 8)[0]
            imports = offset(imports_rva)
            lookup, _, _, dll, _ = struct.unpack_from('<IIIII', data, imports)
            self.assertEqual(cstr(offset(dll)), 'KERNEL32.dll')
            self.assertEqual(data[imports + 20:imports + 40], bytes(20))
            symbols, table = [], offset(lookup)
            while True:
                rva = int.from_bytes(data[table:table + width], 'little')
                if not rva: break
                symbols.append(cstr(offset(rva) + 2)); table += width
            self.assertEqual(set(symbols), {'LoadLibraryA', 'GetProcAddress', 'GetEnvironmentVariableA', 'GetStdHandle', 'WriteFile', 'GetTickCount64', 'Sleep', 'ExitProcess'})


if __name__ == '__main__':
    unittest.main()
