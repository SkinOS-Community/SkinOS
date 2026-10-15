#!/usr/bin/env python3
"""Isolated late DirectInput acquisition test; fixture transport, not Steam proof.

Refuses an existing prefix. No input report is resent to make acquisition pass.
Uses the owned ensure-and-feed process and existing manifest-pinned DI consumer.
Importing this module never starts Wine or installs anything.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import struct
import subprocess
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("wine_transport_helpers", ROOT / "scripts/check-wine-input.py")
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)


class LateAcquireScenario:
    """Ordering only; IO is explicit so offline tests can inspect every action."""

    def __init__(self, io, descriptor):
        self.io = io
        self.descriptor = descriptor

    def run(self):
        io = self.io
        io.start_feeder("feeder")
        baseline = io.start_consumer("consumer-baseline")
        io.compare(baseline, "empty_baseline", None)
        io.send_feeder(1, self.descriptor)
        held = base.report(buttons=0x1000, lt=255, axes=(12345, -23456, -1234, 2345))
        io.send_feeder(2, held)
        io.check_report_count(1, "before_first_late_consumer")
        first = io.start_consumer("consumer-late-1")
        io.compare(first, "first_late_acquire_held", held)
        io.check_report_count(1, "before_second_late_consumer")
        second = io.start_consumer("consumer-late-2")
        io.compare(second, "second_late_acquire_held", held)
        io.compare(first, "first_consumer_still_held", held)
        io.check_report_count(1, "both_late_consumers_verified")

        neutral = base.report()
        io.send_feeder(2, neutral)
        for iteration in range(3):
            for name, child in (("first", first), ("second", second)):
                io.compare(child, f"release_{name}_{iteration}", neutral)
            io.pause(0.05)
        io.check_report_count(2, "release_stays_released")

        io.send_feeder(2, held)
        io.compare(first, "held_before_eof_first", held)
        io.compare(second, "held_before_eof_second", held)
        io.close_feeder()
        io.compare(first, "eof_removed_first", None)
        io.compare(second, "eof_removed_second", None)
        io.compare(baseline, "eof_removed_baseline", None)
        io.check_report_count(3, "eof_removed")

        io.start_feeder("feeder-reopen")
        io.compare(first, "reopen_empty_first", None)
        io.compare(second, "reopen_empty_second", None)
        io.send_feeder(1, self.descriptor)
        # No neutral producer report: initial neutral must not inherit old held
        # state; both existing DI objects were released by the kind-3 checks.
        io.compare(first, "reopen_create_only_neutral_first", neutral)
        io.compare(second, "reopen_create_only_neutral_second", neutral)
        io.check_report_count(3, "reopen_without_any_new_report")
        io.close_feeder()
        io.compare(first, "final_eof_removed_first", None)
        io.compare(second, "final_eof_removed_second", None)


def read_descriptor():
    path = ROOT / "third_party/notproton/integration/wine-input/descriptor.h"
    value = bytes(int(x, 16) for x in re.findall(r"0x([0-9a-f]{2})", path.read_text()))
    if len(value) != 188 or hashlib.sha256(value).hexdigest() != base.DESCRIPTOR_SHA:
        raise RuntimeError("Pinned producer descriptor mismatch")
    return value


def verify_payload(payload, arch):
    """No mutable or unpinned transport files enter the install command."""
    if payload.is_symlink():
        raise RuntimeError("Payload root must not be a symlink")
    payload = payload.resolve()
    allowed = {ROOT / "target/wine-input-replay-experiment", ROOT / "target/wine-input-ensure-experiment", ROOT / "target/wine-input"}
    if payload not in {path.resolve() for path in allowed}:
        raise RuntimeError("Payload must be an owned Wine input build root")
    manifest = json.loads((payload / "manifest.json").read_text())
    if manifest.get("schema_version") != 1:
        raise RuntimeError("Unsupported transport manifest schema")
    for name in ("feeder.exe", "skinosinput.sys", "skinosinput.inf"):
        path = payload / arch / name
        if path.is_symlink() or not path.is_file():
            raise RuntimeError("Transport file must be a regular non-symlink: " + name)
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != manifest["files"].get(arch + "/" + name):
            raise RuntimeError("Transport checksum mismatch: " + name)
        if name.endswith((".exe", ".sys")):
            if len(data) < 64 or data[:2] != b"MZ":
                raise RuntimeError("Invalid transport PE image: " + name)
            offset = struct.unpack_from("<I", data, 0x3c)[0]
            if offset + 6 > len(data) or data[offset:offset + 4] != b"PE\0\0":
                raise RuntimeError("Invalid transport PE header: " + name)
            if struct.unpack_from("<H", data, offset + 4)[0] != {"x64": 0x8664, "arm64": 0xaa64}[arch]:
                raise RuntimeError("Transport architecture mismatch: " + name)
    return payload, manifest


class Harness:
    def __init__(self, args, payload, manifest, consumer, consumer_arch, consumer_sha):
        self.args = args
        self.logs = args.log_dir.resolve()
        self.prefix = args.prefix.resolve()
        self.folder = payload / args.arch
        self.manifest = manifest
        self.consumer_path = consumer
        self.consumer_arch = consumer_arch
        self.consumer_sha = consumer_sha
        self.env = os.environ.copy()
        self.env.update(WINEPREFIX=str(self.prefix), WINEARCH="win64", WINELOADER=str(args.wine.absolute()))
        for key in list(self.env):
            if key.startswith(("Steam", "STEAM_", "SKINOS_")) or key in ("DYLD_INSERT_LIBRARIES", "WINEDLLOVERRIDES"):
                self.env.pop(key)
        self.feeder = None
        self.consumers = []
        self.results = []
        self.events = []
        self.reports_written = self.reports_acked = self.frames_written = 0
        self.install_attempted = False

    def record(self, event, **fields):
        item = {"event": event, "monotonic_ns": time.monotonic_ns(), "unix_ns": time.time_ns(), **fields}
        self.events.append(item)
        with (self.logs / "timeline.jsonl").open("a") as output:
            output.write(json.dumps(item) + "\n")
        return item

    def windows(self, path):
        return "Z:" + str(path.resolve()).replace("/", "\\")

    def command(self, path, *args):
        # Keep the literal selected loader path; canonicalization changed FEX
        # runtime behavior in earlier controlled prefix experiments.
        return [str(self.args.wine.absolute()), self.windows(path), *args]

    def run_command(self, name, args, timeout=20, checked=False):
        with (self.logs / (name + ".stdout.log")).open("wb") as stdout, (self.logs / (name + ".stderr.log")).open("wb") as stderr:
            result = subprocess.run([str(self.args.wine.absolute()), *args], env=self.env,
                                    stdout=stdout, stderr=stderr, timeout=timeout)
        self.record("command_exit", name=name, exit_code=result.returncode)
        if checked and result.returncode:
            raise RuntimeError(name + " failed: " + str(result.returncode))
        return result.returncode

    def start_feeder(self, name):
        if self.feeder is not None:
            raise RuntimeError("Feeder already running")
        self.install_attempted = True
        self.feeder = base.Child(self.command(self.folder / "feeder.exe", "--ensure-and-feed",
                                             self.windows(self.folder / "skinosinput.inf")), self.env, self.logs, name)
        self.record("feeder_started", name=name, host_pid=self.feeder.process.pid)
        ready = self.feeder.wait("wine_input_ready", timeout=45)
        self.record("feeder_ready", name=name, readiness=ready)
        if ready.get("ok") is not True or ready.get("win32_error") != 0:
            raise RuntimeError("Feeder readiness failed")

    def start_consumer(self, name):
        started_ns = time.monotonic_ns()
        consumer = base.Child(self.command(self.consumer_path), self.env, self.logs, name)
        self.consumers.append(consumer)
        self.record("consumer_started", name=name, host_pid=consumer.process.pid,
                    start_monotonic_ns=started_ns, source_reports_written=self.reports_written,
                    source_reports_acked=self.reports_acked)
        ready = consumer.wait("transport_consumer_ready")
        self.record("consumer_ready", name=name, readiness=ready)
        if ready.get("dinput_state_probe") is not True:
            raise RuntimeError("Consumer is not the strict DirectInput state probe")
        return consumer

    def send_feeder(self, kind, payload=b""):
        if self.feeder is None:
            raise RuntimeError("No active feeder")
        data = base.frame(kind, payload)
        self.frames_written += 1
        if kind == 2:
            self.reports_written += 1
        self.record("producer_write", kind=kind, slot=0, sequence=self.frames_written,
                    source_reports_written=self.reports_written, payload_hex=payload.hex(),
                    frame_sha256=hashlib.sha256(data).hexdigest())
        self.feeder.send(data)
        ack = self.feeder.wait("wine_input_ack")
        if ack.get("ok") is not True or ack.get("win32_error") != 0 or ack.get("kind") != kind or ack.get("slot") != 0:
            self.record("producer_ack_failed", acknowledgment=ack)
            raise RuntimeError("Unexpected producer acknowledgment: " + str(ack))
        if kind == 2:
            self.reports_acked += 1
        self.record("producer_ack", acknowledgment=ack, source_reports_acked=self.reports_acked)

    def check_report_count(self, expected, label):
        passed = self.reports_written == expected == self.reports_acked
        self.record("source_count_checkpoint", label=label, expected=expected,
                    written=self.reports_written, acknowledged=self.reports_acked, ok=passed)
        if not passed:
            raise RuntimeError("Unexpected source report count at " + label)

    def compare(self, consumer, label, value):
        # These frames go only to the consumer's expectation pipe. They never
        # reach the feeder or the driver's producer IOCTL.
        kind = 3 if value is None else 2
        began = time.monotonic_ns()
        self.record("consumer_expectation", consumer=consumer.name, case=label, kind=kind,
                    expected_payload_hex=None if value is None else value.hex(),
                    source_reports_written=self.reports_written, source_reports_acked=self.reports_acked)
        consumer.send(base.frame(kind, value or b""))
        result = consumer.wait("transport_comparison", timeout=8)
        result = {**result, "case": label, "consumer": consumer.name, "host_pid": consumer.process.pid,
                  "duration_ms": (time.monotonic_ns() - began) / 1_000_000,
                  "expected_payload_hex": None if value is None else value.hex(),
                  "source_reports_written": self.reports_written, "source_reports_acked": self.reports_acked}
        self.results.append(result)
        self.record("comparison", result=result)
        print(json.dumps({"case": label, "ok": result.get("ok"), "transport_only": True}), flush=True)
        if result.get("ok") is not True or result.get("kind") != kind or result.get("slot") != 0:
            raise RuntimeError("Late-acquire transport mismatch: " + label)
        if value is not None and (result.get("dinput_match") is not True or result.get("observed_di") is None):
            raise RuntimeError("Missing actual DirectInput state: " + label)

    def close_feeder(self):
        if self.feeder is None:
            return
        child = self.feeder
        self.record("feeder_stdin_eof", name=child.name, source_reports_written=self.reports_written)
        code = child.close()
        self.feeder = None
        self.record("feeder_exit", name=child.name, exit_code=code)
        if code:
            raise RuntimeError("Feeder EOF cleanup failed: " + str(code))

    def pause(self, seconds):
        time.sleep(seconds)

    def cleanup(self):
        cleanup = {"install_attempted": self.install_attempted, "diagnostics": {}, "errors": []}
        try:
            self.close_feeder()
        except Exception as exc:
            cleanup["errors"].append("feeder: " + str(exc))

        def diagnostic(name, command):
            try:
                code = self.run_command(name, [self.windows(self.folder / "feeder.exe"), command], timeout=10)
                cleanup[name + "_exit"] = code
                lines = []
                for line in (self.logs / (name + ".stdout.log")).read_text(errors="replace").splitlines():
                    try:
                        lines.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass
                cleanup["diagnostics"][name] = lines
                return next((x for x in reversed(lines) if x.get("event") == "wine_input_driver_status"), None)
            except Exception as exc:
                cleanup["errors"].append(name + ": " + str(exc))
                return None

        if self.install_attempted:
            diagnostic("cleanup-before", "--status")
            diagnostic("uninstall", "--uninstall")
            deadline = time.monotonic() + 5
            removed = False
            for attempt in range(1, 11):
                state = diagnostic("cleanup-after-" + str(attempt), "--status")
                service_absent = state is not None and state.get("service_found") is False and state.get("service_query_error") == 1060
                root_absent = state is not None and state.get("cm_locate_result") == 13
                cleanup.update(service_absent_verified=service_absent, root_absent_verified=root_absent)
                removed = service_absent and root_absent
                if removed or time.monotonic() >= deadline:
                    break
                time.sleep(0.25)
            cleanup["removal_state"] = "removed" if removed else "pending_or_failed"
            if not removed:
                cleanup["errors"].append("Root/service removal was not confirmed within bounded readback")
            if cleanup.get("uninstall_exit", 1) != 0:
                cleanup["errors"].append("Uninstall command failed")
        # Keep consumers alive across service removal; they are never used as
        # the producer. A failed comparison also persists as their exit code.
        for consumer in reversed(self.consumers):
            try:
                code = consumer.close()
                cleanup[consumer.name + "_exit"] = code
                if code:
                    cleanup["errors"].append(consumer.name + " exited " + str(code))
            except Exception as exc:
                cleanup["errors"].append(consumer.name + ": " + str(exc))
        (self.logs / "cleanup.json").write_text(json.dumps(cleanup, indent=2) + "\n")
        return cleanup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wine", required=True, type=pathlib.Path)
    parser.add_argument("--prefix", required=True, type=pathlib.Path)
    parser.add_argument("--arch", required=True, choices=("x64", "arm64"))
    parser.add_argument("--payload", type=pathlib.Path, default=ROOT / "target/wine-input-replay-experiment")
    parser.add_argument("--consumer-arch", choices=("x64", "arm64", "x86"))
    parser.add_argument("--consumer-path", type=pathlib.Path)
    parser.add_argument("--log-dir", required=True, type=pathlib.Path)
    args = parser.parse_args()
    prefix = args.prefix.resolve()
    logs = args.log_dir.resolve()
    if prefix.exists():
        raise RuntimeError("Refusing existing prefix: " + str(prefix))
    if logs.exists() and any(logs.iterdir()):
        raise RuntimeError("Refusing nonempty log directory: " + str(logs))
    if not args.wine.is_file():
        raise RuntimeError("Wine loader not found")
    payload, manifest = verify_payload(args.payload, args.arch)
    consumer_arch = args.consumer_arch or args.arch
    requested = args.consumer_path or ROOT / "target/wine-input-di-experiment" / consumer_arch / "consumer-dinput.exe"
    consumer, consumer_arch, consumer_sha = base.selected_consumer(payload, args.arch, consumer_arch, requested)
    if consumer.name != "consumer-dinput.exe":
        raise RuntimeError("Strict late-acquire test requires the existing DirectInput state consumer")
    descriptor = read_descriptor()
    logs.mkdir(parents=True, exist_ok=True)
    prefix.mkdir(parents=True, mode=0o700)
    (logs / "payload-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (logs / "consumer-manifest.json").write_bytes((consumer.parent.parent / "manifest.json").read_bytes())
    harness = Harness(args, payload, manifest, consumer, consumer_arch, consumer_sha)
    summary = {"transport_only": True, "steam_input_verified": False, "physical_device_verified": False,
               "late_acquire_verified": False, "passed": False, "ensure_feeder": True,
               "driver_arch": args.arch, "consumer_arch": consumer_arch,
               "consumer_path": str(consumer), "consumer_sha256": consumer_sha,
               "prefix": str(prefix), "payload": str(payload),
               "identity_check": "Consumer requires owned DIPROP_GUIDANDPATH slot/generation pattern internally",
               "pid_scope": "Recorded PIDs are host process IDs, not Windows process IDs"}
    error = None
    try:
        harness.run_command("prefix-init", ["cmd.exe", "/c", "exit", "0"], timeout=60, checked=True)
        LateAcquireScenario(harness, descriptor).run()
        summary["late_acquire_verified"] = True
    except BaseException as exc:
        error = exc
        summary.update(error_type=type(exc).__name__, error=str(exc))
        (logs / "failure.json").write_text(json.dumps(summary, indent=2) + "\n")
    finally:
        cleanup = harness.cleanup()
        summary.update(cases=harness.results, cleanup=cleanup, producer_frames_written=harness.frames_written,
                       producer_reports_written=harness.reports_written, producer_reports_acked=harness.reports_acked)
        summary["passed"] = error is None and not cleanup["errors"]
        (logs / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if error is not None:
        raise error
    if not summary["passed"]:
        raise RuntimeError("Comparisons passed but cleanup failed; see cleanup.json")
    print(json.dumps({"passed": True, "transport_only": True, "summary": str(logs / "summary.json")}), flush=True)


if __name__ == "__main__":
    main()
