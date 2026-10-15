#!/usr/bin/env python3
"""Offline protocol/staging tests only; no Wine, Steam, or physical assertions."""
import hashlib
import importlib.util
import os
import pathlib
import struct
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock

sys.dont_write_bytecode = True
path = pathlib.Path(__file__).with_name("game_rumble_driver.py")
with mock.patch.object(subprocess, "Popen", side_effect=AssertionError("Offline import only")):
    spec = importlib.util.spec_from_file_location("rumble_driver_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)


def result(kind=6, generation=1, left=0, right=0, code=0):
    def device(kind):
        return r"\\?\hid#vid_045e&pid_028e&%s_00#skinos-wine-output-00-%08x#{%s}" % (kind, generation, module.GUIDS[kind])
    return {"event": "wine_rumble_result", "ok": code == 0, "kind": kind, "slot": 0, "owned_slot": 0,
            "generation": generation, "left": left, "right": right, "xinput_result": code,
            "xinput_set_called": kind == 5, "input_bytes": 15, "output_bytes": 5, "feature_bytes": 17,
            "feature_verified": True, "input_id_zero_verified": True, "haptics_caps_verified": True,
            "xi_interfaces": 1, "owned_xi_interfaces": 1, "dinput_devices": 1, "owned_dinput_devices": 1,
            "xinput_connected_count": 1, "inspection_error": 0, "device_path": device("xi"), "ig_path": device("ig"),
            "transport_only": True, "delivery_verified": False, "physical_device_verified": False}


class FakeChild:
    def __init__(self, rows):
        self.rows = list(rows)
        self.sent = []
        self.events = []
        self.reaped = False
        self.process = types.SimpleNamespace(poll=lambda: 0 if self.reaped else None)

    def send(self, data):
        self.sent.append(data); self.events.append(("send", struct.unpack_from("<I", data, 8)[0]))

    def wait(self, event, timeout):
        assert event == "wine_rumble_result" and timeout == 8
        value = self.rows.pop(0)
        if isinstance(value, Exception): raise value
        return value

    def close(self):
        self.events.append(("reaped",)); self.reaped = True; return 0


def driver(rows, bound=None):
    value = module.Driver.__new__(module.Driver)
    value.child = FakeChild(rows); value.bound = bound; value.revoked = value.closed = False
    value.staging = None; value.record = mock.Mock()
    return value


class ProtocolTests(unittest.TestCase):
    def test_actual_pinned_artifact_readonly(self):
        artifact, digest = module.selected_consumer()
        self.assertEqual(module.observer.pe_machine(artifact.read_bytes()), 0xAA64)
        self.assertEqual(hashlib.sha256(artifact.read_bytes()).hexdigest(), digest)

    def test_fresh_inspection_then_exact_motor_frame_and_result(self):
        value = driver([result(), result(5, left=0x1234, right=0xABCD)])
        observed = value.set_motors(0x1234, 0xABCD)
        self.assertEqual([struct.unpack_from("<I", x, 8)[0] for x in value.child.sent], [6, 5])
        self.assertEqual(value.child.sent[1][32:36], b"\x34\x12\xcd\xab")
        self.assertEqual(observed["xinput_result"], 0)
        self.assertFalse(observed["delivery_verified"])

    def test_binding_or_caps_failure_refuses_motor_frame(self):
        bound = module.validate_result(result(), 6)
        for replacement in (result(generation=2), {**result(), "feature_bytes": 33},
                            {**result(), "xi_interfaces": 2}, {**result(), "owned_slot": 1},
                            {**result(), "device_path": "foreign"}, {**result(), "feature_verified": False}):
            with self.subTest(replacement=replacement):
                value = driver([replacement], bound)
                with self.assertRaises(RuntimeError): value.set_motors(100, 200)
                self.assertTrue(value.revoked)
                self.assertEqual([struct.unpack_from("<I", x, 8)[0] for x in value.child.sent], [6])
                value.close()
                self.assertEqual(len(value.child.sent), 1)
                self.assertTrue(value.child.reaped)

    def test_actual_xinput_failure_is_returned_not_delivery(self):
        value = driver([result(), result(5, left=200, code=1167)])
        observed = value.set_motors(200, 0)
        self.assertFalse(observed["ok"])
        self.assertEqual(observed["xinput_result"], 1167)
        self.assertFalse(observed["delivery_verified"])

    def test_close_zero_only_after_same_generation_inspection_then_reap(self):
        value = driver([result(), result(5)], module.validate_result(result(), 6))
        self.assertEqual(value.close(), 0)
        self.assertEqual(value.child.events, [("send", 6), ("send", 5), ("reaped",)])
        self.assertEqual(value.child.sent[1][32:36], bytes(4))
        self.assertEqual(value.close(), 0)
        self.assertEqual(len(value.child.sent), 2)

    def test_invalid_values_and_transport_failure_cannot_send_motors(self):
        for left in (-1, 65536, True, 1.5):
            value = driver([])
            with self.assertRaises(ValueError): value.set_motors(left, 0)
            self.assertFalse(value.child.sent)
        value = driver([TimeoutError("offline transport failure")])
        with self.assertRaises(TimeoutError): value.set_motors(1, 2)
        self.assertTrue(value.revoked)
        self.assertEqual(len(value.child.sent), 1)


class FilesystemTests(unittest.TestCase):
    def test_unique_c_staging_and_unchanged_only_cleanup(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp); prefix = root / "pfx"; (prefix / "drive_c").mkdir(parents=True)
            source = root / "rumble-consumer.exe"; source.write_bytes(b"offline staged bytes; never executed")
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            stage = module.stage_consumer(source, prefix, digest)
            self.assertTrue(stage["windows_path"].startswith("C:\\skinos-input-qa\\rumble-"))
            self.assertEqual(module.cleanup_stage(stage), {"removed": True, "directory_removed": True})
            stage = module.stage_consumer(source, prefix, digest)
            pathlib.Path(stage["staged"]).write_bytes(b"changed")
            self.assertFalse(module.cleanup_stage(stage)["removed"])
            self.assertTrue(source.is_file())

    def test_explicit_fex_environment_and_saved_msync(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp); runner = root / "CrossOver"; prefix = root / "compat/pfx"
            loader = runner / "skinos-launcher/aarch64-unix/Wine.app/Contents/MacOS/wine"
            server = runner / "bin/wineserver-arm64"
            for item in (loader, server): item.parent.mkdir(parents=True, exist_ok=True); item.write_bytes(b"not executed"); item.chmod(0o700)
            device = prefix / "drive_c/windows/system32/winedevice.exe"; device.parent.mkdir(parents=True)
            pe = bytearray(80); pe[:2] = b"MZ"; struct.pack_into("<I", pe, 0x3C, 64); pe[64:68] = b"PE\0\0"; struct.pack_into("<H", pe, 68, 0xAA64); device.write_bytes(pe)
            (prefix.parent / "notproton-msync").write_text("1\n")
            with mock.patch.dict(os.environ, {"SteamAppId": "offline", "SKINOS_TEST": "offline"}):
                selected, env, metadata = module.prepare_environment(runner, prefix)
            self.assertEqual(selected, loader)
            self.assertEqual(env["WINESERVER"], str(server))
            self.assertEqual(env["WINEMSYNC"], "1")
            self.assertEqual(env["FEX_APP_DATA_LOCATION"], str(prefix.parent / "fex-emu"))
            self.assertNotIn("SteamAppId", env); self.assertNotIn("SKINOS_TEST", env)
            self.assertEqual(metadata["winedevice_machine"], "arm64")


if __name__ == "__main__": unittest.main(verbosity=2)
