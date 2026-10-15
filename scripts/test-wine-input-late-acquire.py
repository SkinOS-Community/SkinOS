#!/usr/bin/env python3
"""Offline orchestration tests only. Never starts Wine, Steam, or subprocesses."""
import contextlib
import importlib.util
import io
import pathlib
import struct
import subprocess
import sys
import types
import unittest
from unittest import mock

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]


def no_process(*args, **kwargs):
    raise AssertionError("Offline tests must never launch a process")


with mock.patch.object(subprocess, "Popen", side_effect=no_process), mock.patch.object(subprocess, "run", side_effect=no_process):
    spec = importlib.util.spec_from_file_location("late_acquire_under_test", ROOT / "scripts/check-wine-input-late-acquire.py")
    subject = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(subject)

HELD = struct.pack("<2xHBB4h6x", 0x1000, 255, 0, 12345, -23456, -1234, 2345)
NEUTRAL = bytes(20)


class FakeIO:
    """Ordering model, not evidence that a real consumer observed input."""

    def __init__(self, descriptor):
        self.descriptor = descriptor
        self.events = []
        self.feeder = None
        self.present = False
        self.snapshot = None
        self.frames = []
        self.reports = self.acked = 0
        self.consumers = {}
        self.expectations = []

    def start_feeder(self, name):
        assert self.feeder is None
        self.feeder = name
        self.events.append(("start_feeder", name))

    def start_consumer(self, name):
        assert name not in self.consumers, "Late consumers must be fresh objects"
        if name.startswith("consumer-late-"):
            assert self.reports == self.acked == 1
            assert self.snapshot == HELD
            assert ("ack", 2, 1) in self.events
        result = types.SimpleNamespace(name=name)
        self.consumers[name] = result
        self.events.append(("start_consumer", name, self.reports, self.acked))
        return result

    def send_feeder(self, kind, payload=b""):
        assert self.feeder is not None
        self.frames.append((kind, payload))
        self.events.append(("producer", kind, payload))
        if kind == 1:
            assert not self.present and payload == self.descriptor
            self.present = True
            self.snapshot = None
        elif kind == 2:
            assert self.present and len(payload) == 20
            self.reports += 1
            self.snapshot = payload
            self.acked += 1
        else:
            raise AssertionError("Scenario must exercise EOF, not explicit Destroy")
        self.events.append(("ack", kind, self.acked))

    def compare(self, consumer, label, value):
        assert consumer is self.consumers[consumer.name]
        before = (len(self.frames), self.reports, self.acked)
        if value is None:
            assert not self.present
        else:
            assert self.present
            assert value == (self.snapshot if self.snapshot is not None else NEUTRAL)
        if "late_acquire_held" in label or label == "first_consumer_still_held":
            assert self.reports == self.acked == 1 and value == HELD
        if label.startswith("release_"):
            assert self.reports == self.acked == 2 and value == NEUTRAL
        if label.startswith("reopen_create_only_"):
            assert self.reports == self.acked == 3 and self.snapshot is None
            assert self.frames[-1] == (1, self.descriptor)
        self.expectations.append((consumer.name, label, value, self.reports))
        self.events.append(("compare", consumer.name, label, value, self.reports))
        assert before == (len(self.frames), self.reports, self.acked)

    def check_report_count(self, expected, label):
        assert self.reports == self.acked == expected
        self.events.append(("checkpoint", label, expected))

    def pause(self, seconds):
        assert seconds == 0.05
        self.events.append(("pause", seconds, self.reports))

    def close_feeder(self):
        assert self.feeder is not None
        self.events.append(("eof", self.feeder, self.reports))
        self.feeder = None
        self.present = False
        self.snapshot = None


class OfflineTests(unittest.TestCase):
    def setUp(self):
        self.guards = contextlib.ExitStack()
        self.guards.enter_context(mock.patch.object(subprocess, "Popen", side_effect=no_process))
        self.guards.enter_context(mock.patch.object(subprocess, "run", side_effect=no_process))
        self.guards.enter_context(mock.patch.object(subject.base, "Child", side_effect=no_process))
        self.guards.enter_context(mock.patch.object(subject.time, "sleep", side_effect=AssertionError("Use FakeIO.pause")))
        self.addCleanup(self.guards.close)

    def test_scenario_orders_late_acquire_without_resending_input(self):
        descriptor = subject.read_descriptor()  # Read-only pinned local bytes.
        fake = FakeIO(descriptor)
        subject.LateAcquireScenario(fake, descriptor).run()
        self.assertEqual((fake.reports, fake.acked, len(fake.frames)), (3, 3, 5))
        self.assertEqual([kind for kind, _ in fake.frames], [1, 2, 2, 2, 1])
        self.assertEqual([payload for kind, payload in fake.frames if kind == 2], [HELD, NEUTRAL, HELD])
        self.assertEqual(len(fake.expectations), 21)
        self.assertEqual(set(fake.consumers), {"consumer-baseline", "consumer-late-1", "consumer-late-2"})
        acknowledged = fake.events.index(("ack", 2, 1))
        for name in ("consumer-late-1", "consumer-late-2"):
            self.assertGreater(fake.events.index(("start_consumer", name, 1, 1)), acknowledged)
        release_checks = [item for item in fake.expectations if item[1].startswith("release_")]
        self.assertEqual(len(release_checks), 6)
        self.assertEqual({item[0] for item in release_checks}, {"consumer-late-1", "consumer-late-2"})
        self.assertTrue(all(item[2] == NEUTRAL and item[3] == 2 for item in release_checks))
        self.assertEqual([e for e in fake.events if e[0] == "pause"], [("pause", 0.05, 2)] * 3)
        self.assertEqual([e for e in fake.events if e[0] == "eof"], [("eof", "feeder", 3), ("eof", "feeder-reopen", 3)])
        self.assertIsNone(fake.feeder)
        self.assertFalse(fake.present)
        self.assertEqual([e[2] for e in fake.events if e[0] == "checkpoint"], [1, 1, 1, 2, 3, 3])

    def test_failed_first_report_ack_cannot_start_late_consumers(self):
        class FailedAck(FakeIO):
            def send_feeder(self, kind, payload=b""):
                if kind == 2:
                    self.events.append(("failed_report_ack",))
                    raise RuntimeError("Fixture acknowledgment failed")
                super().send_feeder(kind, payload)

        fake = FailedAck(bytes(188))
        with self.assertRaisesRegex(RuntimeError, "acknowledgment"):
            subject.LateAcquireScenario(fake, bytes(188)).run()
        self.assertEqual(list(fake.consumers), ["consumer-baseline"])

    def test_duplicate_report_injected_during_compare_is_rejected(self):
        class DuplicateReport(FakeIO):
            def compare(self, consumer, label, value):
                if label == "first_late_acquire_held":
                    self.send_feeder(2, value)  # Deliberate offline negative control.
                super().compare(consumer, label, value)

        fake = DuplicateReport(bytes(188))
        with self.assertRaises(AssertionError):
            subject.LateAcquireScenario(fake, bytes(188)).run()
        self.assertEqual(fake.reports, 2)
        self.assertNotIn("consumer-late-2", fake.consumers)

    def harness(self, result):
        harness = subject.Harness.__new__(subject.Harness)
        harness.reports_written = harness.reports_acked = 3
        harness.frames_written = 5
        harness.results = []
        harness.record = mock.Mock()
        harness.feeder = types.SimpleNamespace(send=mock.Mock(side_effect=no_process))
        consumer = types.SimpleNamespace(name="offline-consumer", process=types.SimpleNamespace(pid=1234),
                                         send=mock.Mock(), wait=mock.Mock(return_value=result))
        return harness, consumer

    def test_actual_compare_writes_expectation_only(self):
        for value, kind in ((HELD, 2), (None, 3)):
            with self.subTest(kind=kind):
                result = {"event": "transport_comparison", "ok": True, "kind": kind, "slot": 0,
                          "dinput_match": True, "observed_di": {"offline_fixture": True}}
                harness, consumer = self.harness(result)
                with contextlib.redirect_stdout(io.StringIO()):
                    harness.compare(consumer, "offline_expectation", value)
                consumer.send.assert_called_once_with(subject.base.frame(kind, value or b""))
                consumer.wait.assert_called_once_with("transport_comparison", timeout=8)
                harness.feeder.send.assert_not_called()
                self.assertEqual((harness.reports_written, harness.reports_acked, harness.frames_written), (3, 3, 5))

    def test_actual_compare_rejects_bad_schema_or_missing_observation(self):
        valid = {"event": "transport_comparison", "ok": True, "kind": 2, "slot": 0,
                 "dinput_match": True, "observed_di": {"offline_fixture": True}}
        for changed in ({"ok": False}, {"kind": 3}, {"slot": 1}, {"dinput_match": False}, {"observed_di": None}):
            with self.subTest(changed=changed):
                harness, consumer = self.harness({**valid, **changed})
                with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(RuntimeError):
                    harness.compare(consumer, "offline_bad_schema", HELD)
                harness.feeder.send.assert_not_called()

    def test_actual_send_counts_only_acknowledged_producer_reports(self):
        for kind, payload in ((1, bytes(188)), (2, HELD)):
            with self.subTest(kind=kind):
                harness = subject.Harness.__new__(subject.Harness)
                harness.reports_written = harness.reports_acked = harness.frames_written = 0
                harness.record = mock.Mock()
                harness.feeder = types.SimpleNamespace(send=mock.Mock(), wait=mock.Mock(return_value={"ok": True, "win32_error": 0, "kind": kind, "slot": 0}))
                harness.send_feeder(kind, payload)
                self.assertEqual((harness.reports_written, harness.reports_acked, harness.frames_written), (int(kind == 2), int(kind == 2), 1))
                harness.feeder.send.assert_called_once_with(subject.base.frame(kind, payload))

    def test_actual_bad_ack_stays_unacknowledged(self):
        for changed in ({"ok": False}, {"win32_error": 5}, {"kind": 1}, {"slot": 1}):
            with self.subTest(changed=changed):
                harness = subject.Harness.__new__(subject.Harness)
                harness.reports_written = harness.reports_acked = harness.frames_written = 0
                harness.record = mock.Mock()
                ack = {"ok": True, "win32_error": 0, "kind": 2, "slot": 0, **changed}
                harness.feeder = types.SimpleNamespace(send=mock.Mock(), wait=mock.Mock(return_value=ack))
                with self.assertRaises(RuntimeError):
                    harness.send_feeder(2, HELD)
                self.assertEqual((harness.reports_written, harness.reports_acked, harness.frames_written), (1, 0, 1))
                with self.assertRaises(RuntimeError):
                    harness.check_report_count(1, "offline_rejected_ack")


if __name__ == "__main__":
    unittest.main(verbosity=2)
