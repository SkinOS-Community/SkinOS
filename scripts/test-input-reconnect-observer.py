#!/usr/bin/env python3
"""Pure parser/classifier tests; no Observer construction or Windows API mocks."""
import copy
import importlib.util
import pathlib
import sys
import unittest
import tempfile
import hashlib
sys.dont_write_bytecode = True
path = pathlib.Path(__file__).with_name("input_reconnect_observer.py")
spec = importlib.util.spec_from_file_location("observer_parser", path)
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)


def snapshot(generation=1):
    path = r"\\?\hid#vid_045e&pid_028e&ig_00#skinos-wine-output-00-%08x#{4d1e55b2-f16f-11cf-88cb-001111000030}" % generation
    return {"dinput_enum_result": 0, "dinput_state_result": 0, "dinput_total": 1, "dinput_owned": 1,
            "owned": [{"slot": 0, "generation": generation, "path": path, "guid": "recorded-guid-field"}],
            "dinput_state": {"x": 32768, "y": 32766, "rx": 32768, "ry": 32766, "z": 32768,
                             "pov": 0xffffffff, "buttons": [0] * 10},
            "xinput": [{"slot": 0, "result": 0, "state": {"buttons": 0, "left_trigger": 0, "right_trigger": 0,
                         "left_x": 0, "left_y": 1, "right_x": 0, "right_y": 1}}] +
                       [{"slot": slot, "result": 1167} for slot in (1, 2, 3)]}


class ParserTests(unittest.TestCase):
    def test_neutral_one_lsb_does_not_weaken_pressed_or_axis_checks(self):
        state = snapshot()
        self.assertTrue(observer.qualifies(state, "before"))
        for key, value in (("buttons", 4096), ("left_y", 2), ("left_trigger", 1)):
            bad = copy.deepcopy(state); bad["xinput"][0]["state"][key] = value
            self.assertFalse(observer.qualifies(bad, "before"))
        bad = copy.deepcopy(state); bad["dinput_state"]["y"] = 0
        self.assertFalse(observer.qualifies(bad, "before"))

    def test_new_generation_required(self):
        before = observer.owned_identity(snapshot())
        self.assertFalse(observer.qualifies(snapshot(), "after", before))
        self.assertTrue(observer.qualifies(snapshot(2), "after", before))

    def test_missing_duplicate_and_noninteger_slots_rejected(self):
        for rows in ([], [{"slot": 0, "result": 1167}] * 4,
                     [{"slot": slot, "result": "1167"} for slot in range(4)]):
            value = snapshot(); value["xinput"] = rows
            with self.assertRaises(RuntimeError): observer.qualifies(value, "removed")

    def test_removed_requires_explicit_disconnected_codes(self):
        value = snapshot(); value.update(owned=[], dinput_owned=0, dinput_total=0)
        value["xinput"] = [{"slot": slot, "result": 1167} for slot in range(4)]
        self.assertTrue(observer.qualifies(value, "removed"))
        value["xinput"][2]["result"] = 5
        self.assertFalse(observer.qualifies(value, "removed"))

    def test_foreign_count_and_wrong_slot_rejected(self):
        value = snapshot(); value["dinput_total"] = 2
        self.assertFalse(observer.qualifies(value, "before"))
        value = snapshot(); value["xinput"][0]["slot"], value["xinput"][1]["slot"] = 1, 0
        self.assertFalse(observer.qualifies(value, "before"))


class StagingTests(unittest.TestCase):
    def fixture(self, root):
        prefix = root / "pfx"; (prefix / "drive_c").mkdir(parents=True)
        origin = root / "origin.exe"; origin.write_bytes(b"offline staging bytes, never executed")
        digest = hashlib.sha256(origin.read_bytes()).hexdigest()
        return prefix, origin, digest

    def test_copy_hash_c_path_and_owned_cleanup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary); prefix, origin, digest = self.fixture(root)
            staged = observer.stage_observer(origin, prefix, digest)
            path = pathlib.Path(staged["staged"])
            self.assertEqual(staged["origin_sha256"], staged["staged_sha256"])
            self.assertEqual(path.read_bytes(), origin.read_bytes())
            self.assertTrue(staged["windows_path"].startswith("C:\\skinos-input-qa\\"))
            self.assertEqual(observer.cleanup_staged_observer(staged), {"removed": True, "directory_removed": True})
            self.assertTrue(origin.is_file())
            self.assertTrue((prefix / "drive_c/skinos-input-qa").is_dir())
            self.assertFalse(path.exists())

    def test_changed_copy_is_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary); prefix, origin, digest = self.fixture(root)
            staged = observer.stage_observer(origin, prefix, digest)
            path = pathlib.Path(staged["staged"]); path.write_bytes(b"changed")
            self.assertFalse(observer.cleanup_staged_observer(staged)["removed"])
            self.assertEqual(path.read_bytes(), b"changed")

    def test_symlink_namespace_and_changed_origin_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary); prefix, origin, digest = self.fixture(root)
            (prefix / "drive_c/skinos-input-qa").symlink_to(root, target_is_directory=True)
            with self.assertRaises(RuntimeError): observer.stage_observer(origin, prefix, digest)
            (prefix / "drive_c/skinos-input-qa").unlink(); origin.write_bytes(b"changed")
            with self.assertRaises(RuntimeError): observer.stage_observer(origin, prefix, digest)


if __name__ == "__main__": unittest.main(verbosity=2)
