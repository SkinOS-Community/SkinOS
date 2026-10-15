#!/usr/bin/env python3
"""Read-only Windows observer for an already-running, explicitly selected prefix.

Importing this module performs no process launch. Observer construction launches
only the hash-pinned observer executable, never a feeder/installer/Steam API.
Strict conformance consumers and their exact expected-state rules are untouched.
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import pathlib
import re
import struct
import time
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[1]
PAYLOAD = ROOT / "target/wine-input-observer"
_spec = importlib.util.spec_from_file_location("reconnect_observer_transport", ROOT / "scripts/check-wine-input.py")
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)
OWNED_PATH = re.compile(r"^\\\\\?\\hid#vid_045e&pid_028e&ig_(\d{2})#skinos-wine-output-(\d{2})-([0-9a-f]{8})#\{4d1e55b2-f16f-11cf-88cb-001111000030\}$", re.I)


def pe_machine(data: bytes) -> int:
    if len(data) < 64 or data[:2] != b"MZ":
        raise RuntimeError("Expected a PE image")
    offset = struct.unpack_from("<I", data, 0x3C)[0]
    if offset + 6 > len(data) or data[offset:offset + 4] != b"PE\0\0":
        raise RuntimeError("Invalid PE signature")
    return struct.unpack_from("<H", data, offset + 4)[0]


def selected_observer() -> tuple[pathlib.Path, str]:
    manifest = json.loads((PAYLOAD / "manifest.json").read_text())
    if manifest.get("schema_version") != 1 or manifest.get("experiment") != "reconnect_observer":
        raise RuntimeError("Unexpected observer manifest")
    program = PAYLOAD / "arm64/consumer-observer.exe"
    if program.is_symlink() or not program.is_file():
        raise RuntimeError("Observer must be a regular owned artifact")
    data = program.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != manifest.get("files", {}).get("arm64/consumer-observer.exe"):
        raise RuntimeError("Observer artifact checksum mismatch")
    if pe_machine(data) != 0xAA64:
        raise RuntimeError("Observer artifact must be ARM64")
    return program, digest


def stage_observer(program: pathlib.Path, prefix: pathlib.Path, digest: str) -> dict:
    """Copy only the pinned observer into a fresh, sandbox-readable C: path."""
    data = program.read_bytes()
    if program.is_symlink() or hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError("Observer origin changed before prefix staging")
    drive = prefix / "drive_c"
    if not drive.is_dir():
        raise RuntimeError("Observer staging requires an existing prefix drive_c")
    namespace = drive / "skinos-input-qa"
    if namespace.is_symlink():
        raise RuntimeError("Observer staging namespace must not be a symlink")
    namespace.mkdir(mode=0o700, exist_ok=True)
    name = uuid.uuid4().hex
    folder = namespace / name
    folder.mkdir(mode=0o700)
    staged = folder / "consumer-observer.exe"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    with os.fdopen(os.open(staged, flags, 0o600), "wb") as stream:
        stream.write(data)
    staged_digest = hashlib.sha256(staged.read_bytes()).hexdigest()
    if staged_digest != digest:
        raise RuntimeError("Staged observer checksum mismatch; copy preserved")
    return {"origin": str(program), "origin_sha256": digest, "staged": str(staged),
            "staged_sha256": staged_digest, "directory": str(folder),
            "windows_path": "C:\\skinos-input-qa\\" + name + "\\consumer-observer.exe"}


def cleanup_staged_observer(staging: dict) -> dict:
    """Call only after observer reaping; never recursively remove prefix data."""
    path, folder = pathlib.Path(staging["staged"]), pathlib.Path(staging["directory"])
    if path.parent != folder or path.name != "consumer-observer.exe" or folder.is_symlink():
        return {"removed": False, "preserved_reason": "staging identity changed"}
    if path.is_symlink() or not path.is_file():
        return {"removed": False, "preserved_reason": "staged file absent or replaced"}
    if hashlib.sha256(path.read_bytes()).hexdigest() != staging["staged_sha256"]:
        return {"removed": False, "preserved_reason": "staged bytes changed"}
    path.unlink()
    try:
        folder.rmdir()
    except OSError:
        return {"removed": True, "directory_removed": False, "preserved_reason": "directory not empty or in use"}
    return {"removed": True, "directory_removed": True}


def owned_identity(snapshot: dict) -> dict | None:
    owned = snapshot.get("owned")
    if not isinstance(owned, list) or snapshot.get("dinput_owned") != len(owned):
        raise RuntimeError("Inconsistent owned-device observation")
    if not owned:
        return None
    if len(owned) != 1:
        raise RuntimeError("Reconnect observer requires an unambiguous single owned pad")
    value = owned[0]
    match = OWNED_PATH.fullmatch(value.get("path", ""))
    if not match:
        raise RuntimeError("Observer did not return the complete owned IG path")
    slot, serial_slot, generation = int(match[1]), int(match[2]), int(match[3], 16)
    if slot != serial_slot or not 0 <= slot < 16 or not generation:
        raise RuntimeError("Invalid owned slot/generation path")
    if value.get("slot") != slot or value.get("generation") != generation:
        raise RuntimeError("Owned path and reported identity disagree")
    if not isinstance(value.get("guid"), str) or not value["guid"]:
        raise RuntimeError("DirectInput instance GUID is missing")
    return value


def validate_snapshot(snapshot: dict) -> list[dict]:
    rows = snapshot.get("xinput")
    if not isinstance(rows, list) or len(rows) != 4:
        raise RuntimeError("Observer must return all four XInput slots")
    slots = []
    for row in rows:
        if not isinstance(row, dict) or type(row.get("slot")) is not int or type(row.get("result")) is not int:
            raise RuntimeError("Invalid XInput slot/result schema")
        slots.append(row["slot"])
        if not 0 <= row["result"] <= 0xFFFFFFFF:
            raise RuntimeError("Invalid XInput result code")
        if row["result"] == 0:
            state = row.get("state")
            if not isinstance(state, dict):
                raise RuntimeError("Connected XInput slot has no state")
            for field in ("buttons", "left_trigger", "right_trigger", "left_x", "left_y", "right_x", "right_y"):
                if type(state.get(field)) is not int:
                    raise RuntimeError("Invalid raw XInput state field")
    if sorted(slots) != [0, 1, 2, 3]:
        raise RuntimeError("XInput slots must be unique and cover 0 through 3")
    for field in ("dinput_enum_result", "dinput_state_result", "dinput_total", "dinput_owned"):
        if type(snapshot.get(field)) is not int:
            raise RuntimeError("Invalid DirectInput result/count schema")
    return rows


def near_neutral(snapshot: dict) -> bool:
    """Observer-only tolerance; does not alter exact transport conformance."""
    connected = [row for row in validate_snapshot(snapshot) if row["result"] == 0]
    if len(connected) != 1 or not 0 <= snapshot.get("dinput_state_result", 0xFFFFFFFF) < 0x80000000:
        return False
    pad = connected[0].get("state", {})
    if pad.get("buttons") != 0 or pad.get("left_trigger") != 0 or pad.get("right_trigger") != 0:
        return False
    axes = ("left_x", "left_y", "right_x", "right_y")
    if any(type(pad.get(axis)) is not int or abs(pad[axis]) > 1 for axis in axes):
        return False
    state = snapshot.get("dinput_state")
    if not isinstance(state, dict) or state.get("pov") != 0xFFFFFFFF or state.get("buttons") != [0] * 10:
        return False
    expected = {"x": pad["left_x"] + 32768, "y": 32767 - pad["left_y"],
                "rx": pad["right_x"] + 32768, "ry": 32767 - pad["right_y"], "z": 32768}
    return all(type(state.get(axis)) is int and abs(state[axis] - value) <= 1 for axis, value in expected.items())


def qualifies(snapshot: dict, phase: str, before: dict | None = None) -> bool:
    rows = validate_snapshot(snapshot)
    identity = owned_identity(snapshot)
    if not 0 <= snapshot["dinput_enum_result"] < 0x80000000:
        return False
    if phase == "removed":
        return identity is None and snapshot["dinput_total"] == 0 and all(row["result"] == 1167 for row in rows)
    if phase not in ("before", "after"):
        raise ValueError("Unknown observation phase")
    connected = [row for row in rows if row["result"] == 0]
    if identity is None or snapshot["dinput_total"] != 1 or not near_neutral(snapshot):
        return False
    if len(connected) != 1 or connected[0]["slot"] != identity["slot"]:
        return False
    if any(row["result"] != 1167 for row in rows if row["slot"] != identity["slot"]):
        return False
    return phase == "before" or (before is not None and identity["slot"] == before["slot"] and
        identity["generation"] != before["generation"] and identity["path"].lower() != before["path"].lower())


class Observer:
    def __init__(self, runner: pathlib.Path, prefix: pathlib.Path, logs: pathlib.Path):
        # absolute() preserves the literal loader spelling and symlink path;
        # resolve() previously changed the chosen FEX launcher behavior.
        self.runner_root = pathlib.Path(runner).absolute()
        self.runner = self.runner_root / "skinos-launcher/aarch64-unix/Wine.app/Contents/MacOS/wine"
        self.prefix = pathlib.Path(prefix).absolute()
        self.logs = pathlib.Path(logs).absolute()
        self.child = None
        self.staging = None
        self.before_identity = None
        self.removal_verified = False
        self.closed = False
        if not self.runner_root.is_dir():
            raise RuntimeError("Observer runner must be the selected CrossOver root directory")
        if self.runner.is_symlink() or not self.runner.is_file() or not os.access(self.runner, os.X_OK):
            raise RuntimeError("Selected literal Wine loader is not executable")
        wineserver = self.runner_root / "bin/wineserver-arm64"
        if not wineserver.is_file() or not os.access(wineserver, os.X_OK):
            raise RuntimeError("Selected ARM64 wineserver is not executable")
        if not self.prefix.is_dir():
            raise RuntimeError("Observer requires an existing prefix; it never creates one")
        device_host = self.prefix / "drive_c/windows/system32/winedevice.exe"
        host_data = device_host.read_bytes()
        if pe_machine(host_data) != 0xAA64:
            raise RuntimeError("Selected prefix winedevice.exe is not ARM64")
        program, digest = selected_observer()
        if self.logs.is_symlink():
            raise RuntimeError("Observer log directory must not be a symlink")
        self.logs.mkdir(parents=True, exist_ok=True)
        for name in ("input-observer.jsonl", "input-observer-meta.json", "observer.stdout.jsonl", "observer.stderr.log"):
            if (self.logs / name).exists():
                raise RuntimeError("Refusing to overwrite observer evidence: " + name)
        # Deliberately do not inherit Steam/account/authentication environment.
        allowed = ("HOME", "USER", "LOGNAME", "TMPDIR", "LANG", "LC_ALL")
        env = {key: os.environ[key] for key in allowed if key in os.environ}
        marker = self.prefix.parent / "notproton-msync"
        msync = "0"
        if marker.exists():
            if marker.is_symlink() or not marker.is_file():
                raise RuntimeError("Invalid prefix msync marker")
            with marker.open("rb") as stream:
                value = stream.read(33)
            if len(value) > 32:
                raise RuntimeError("Oversized prefix msync marker")
            msync = value.decode("ascii").strip()
            if msync not in ("0", "1"):
                raise RuntimeError("Invalid prefix msync value")
        env.update(WINEPREFIX=str(self.prefix), WINEARCH="win64", WINELOADER=str(self.runner),
                   WINESERVER=str(wineserver), CX_ROOT=str(self.runner_root),
                   CX_HOME=str(pathlib.Path.home() / "Library/Application Support/CrossOver"),
                   WINEDLLPATH=str(self.runner_root / "lib/wine/x86_64-windows") + ":" + str(self.runner_root / "lib/wine/aarch64-unix"),
                   PATH=str(self.runner_root / "bin") + ":/usr/bin:/bin:/usr/sbin:/sbin",
                   WINEMSYNC=msync, WINEDEBUG="-all", WINEDLLOVERRIDES="xinput1_4,dinput8=b")
        for kind in ("CACHE", "CONFIG", "DATA"):
            env["FEX_APP_" + kind + "_LOCATION"] = str(self.prefix.parent / "fex-emu")
        self.metadata = {"schema_version": 1, "read_only": True, "physical_device_verified": False,
                         "runner_root": str(self.runner_root), "runner_literal": str(self.runner), "prefix": str(self.prefix),
                         "wineserver": str(wineserver), "winemsync": msync,
                         "winedevice_machine": "arm64", "winedevice_sha256": hashlib.sha256(host_data).hexdigest(),
                         "host_verification": "Existing prefix PE image; not a claim about a running PID",
                         "observer_artifact": str(program), "observer_sha256": digest}
        try:
            self.staging = stage_observer(program, self.prefix, digest)
            self.metadata["staging"] = self.staging
            (self.logs / "input-observer-meta.json").write_text(json.dumps(self.metadata, indent=2) + "\n")
            self.child = base.Child([str(self.runner), self.staging["windows_path"]], env, self.logs, "observer")
            ready = self.child.wait("input_observer_ready", timeout=4)
            if ready.get("schema_version") != 1 or ready.get("read_only") is not True:
                raise RuntimeError("Unexpected observer readiness schema")
            self.record("ready", readiness=ready, host_pid=self.child.process.pid)
        except BaseException:
            if self.child is not None:
                try:
                    self.child.close()
                finally:
                    if self.child.process.poll() is not None:
                        self.cleanup_staging()
            elif self.staging is not None:
                self.cleanup_staging()
            raise

    def cleanup_staging(self):
        if self.staging is None:
            return
        result = cleanup_staged_observer(self.staging)
        self.record("staging_cleanup", result=result, staged=self.staging["staged"])
        self.staging = None

    def record(self, event, **fields):
        value = {"event": event, "monotonic_ns": time.monotonic_ns(), "unix_ns": time.time_ns(), **fields}
        with (self.logs / "input-observer.jsonl").open("a") as stream:
            stream.write(json.dumps(value) + "\n")

    def compare(self, phase: str) -> dict:
        if self.closed or self.child is None:
            raise RuntimeError("Observer is closed")
        if phase not in ("before", "removed", "after"):
            raise ValueError("Unknown reconnect observation phase")
        if phase != "before" and self.before_identity is None:
            raise RuntimeError("A before observation is required")
        if phase == "after" and not self.removal_verified:
            raise RuntimeError("Device removal must be observed before reconnection")
        began = time.monotonic()
        deadline = began + 2.0
        stable = 0
        stable_since = None
        stable_key = None
        last = None
        while time.monotonic() < deadline:
            # Snapshot command only, not a frame/expectation/producer report.
            self.child.send(b"\x01")
            snapshot = self.child.wait("input_observer_snapshot", timeout=max(0.01, deadline - time.monotonic()))
            if snapshot.get("schema_version") != 1 or snapshot.get("read_only") is not True:
                raise RuntimeError("Unexpected observer snapshot schema")
            self.record("snapshot", phase=phase, snapshot=snapshot)
            last = snapshot
            identity = owned_identity(snapshot)
            good = qualifies(snapshot, phase, self.before_identity)
            if good:
                key = None if identity is None else (identity["slot"], identity["generation"], identity["path"].lower(), identity["guid"])
                if key != stable_key:
                    stable = 0
                    stable_since = None
                    stable_key = key
                stable += 1
                if stable_since is None:
                    stable_since = time.monotonic()
                if stable >= 3 and time.monotonic() - stable_since >= 0.12:
                    if phase == "before":
                        self.before_identity = dict(identity)
                    if phase == "removed":
                        self.removal_verified = True
                    evidence = {"phase": phase, "ok": True, "read_only": True, "physical_device_verified": False,
                                "near_neutral": phase != "removed", "generation_changed": phase == "after",
                                "previous_generation": self.before_identity["generation"],
                                "stable_samples": stable, "elapsed_ms": round((time.monotonic() - began) * 1000, 2),
                                "snapshot": snapshot, "log": str(self.logs / "input-observer.jsonl"),
                                "owned_identity": identity,
                                "observer_sha256": self.metadata["observer_sha256"]}
                    self.record("comparison", evidence=evidence)
                    return evidence
            else:
                stable = 0
                stable_since = None
                stable_key = None
            remaining = deadline - time.monotonic()
            if remaining > 0:
                time.sleep(min(0.04, remaining))
        self.record("comparison_failed", phase=phase, last_snapshot=last)
        raise RuntimeError("Read-only reconnect observation did not converge within two seconds: " + phase)

    def close(self) -> int:
        if self.closed:
            return self.exit_code
        self.exit_code = self.child.close() if self.child is not None else 0
        self.closed = True
        self.record("closed", exit_code=self.exit_code)
        if self.child is None or self.child.process.poll() is not None:
            self.cleanup_staging()
        else:
            self.record("staging_preserved", reason="observer process not reaped")
        return self.exit_code
