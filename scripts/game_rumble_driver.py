#!/usr/bin/env python3
"""External, ownership-bound XInput QA client for an existing normal-game prefix.

Imports are inert. Driver construction starts only the pinned Windows rumble
consumer; no feeder, installer, Steam API, or account configuration is touched.
An XInput result is never promoted to downstream delivery or physical proof.
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
PAYLOAD = ROOT / "target/wine-input-rumble-experiment"
_spec = importlib.util.spec_from_file_location("game_rumble_observer_rules", ROOT / "scripts/input_reconnect_observer.py")
observer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(observer)
base = observer.base
PATH_PATTERN = re.compile(r"^\\\\\?\\hid#vid_045e&pid_028e&(xi|ig)_(\d{2})#skinos-wine-output-(\d{2})-([0-9a-f]{8})#\{([0-9a-f-]+)\}$", re.I)
GUIDS = {"xi": "6c53d5fd-6480-440f-b618-476750c5e1a6", "ig": "4d1e55b2-f16f-11cf-88cb-001111000030"}


def selected_consumer() -> tuple[pathlib.Path, str]:
    manifest = json.loads((PAYLOAD / "manifest.json").read_text())
    if manifest.get("schema_version") != 1 or manifest.get("experiment") != "owned_wine_haptics_feedback":
        raise RuntimeError("Unexpected rumble payload manifest")
    path = PAYLOAD / "arm64/rumble-consumer.exe"
    if path.is_symlink() or not path.is_file():
        raise RuntimeError("Rumble consumer must be a regular owned artifact")
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != manifest.get("files", {}).get("arm64/rumble-consumer.exe") or observer.pe_machine(data) != 0xAA64:
        raise RuntimeError("Rumble consumer hash/ARM64 architecture mismatch")
    return path, digest


def prepare_environment(runner: pathlib.Path, prefix: pathlib.Path) -> tuple[pathlib.Path, dict, dict]:
    runner, prefix = pathlib.Path(runner).absolute(), pathlib.Path(prefix).absolute()
    loader = runner / "skinos-launcher/aarch64-unix/Wine.app/Contents/MacOS/wine"
    server = runner / "bin/wineserver-arm64"
    if not runner.is_dir() or loader.is_symlink() or not loader.is_file() or not os.access(loader, os.X_OK):
        raise RuntimeError("Selected CrossOver root has no regular staged ARM64 loader")
    if not server.is_file() or not os.access(server, os.X_OK):
        raise RuntimeError("Selected ARM64 wineserver is unavailable")
    if not prefix.is_dir():
        raise RuntimeError("Rumble QA requires an existing prefix; it never creates one")
    host_data = (prefix / "drive_c/windows/system32/winedevice.exe").read_bytes()
    if observer.pe_machine(host_data) != 0xAA64:
        raise RuntimeError("Selected prefix winedevice.exe must be ARM64")
    marker = prefix.parent / "notproton-msync"
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
    # Allowlisted public process context, never inherited Steam/auth variables.
    env = {key: os.environ[key] for key in ("HOME", "USER", "LOGNAME", "TMPDIR", "LANG", "LC_ALL") if key in os.environ}
    env.update(WINEPREFIX=str(prefix), WINEARCH="win64", WINELOADER=str(loader), WINESERVER=str(server),
               CX_ROOT=str(runner), CX_HOME=str(pathlib.Path.home() / "Library/Application Support/CrossOver"),
               WINEDLLPATH=str(runner / "lib/wine/x86_64-windows") + ":" + str(runner / "lib/wine/aarch64-unix"),
               PATH=str(runner / "bin") + ":/usr/bin:/bin:/usr/sbin:/sbin", WINEMSYNC=msync,
               WINEDEBUG="-all", WINEDLLOVERRIDES="xinput1_4,dinput8,hid=b")
    for kind in ("CACHE", "CONFIG", "DATA"):
        env["FEX_APP_" + kind + "_LOCATION"] = str(prefix.parent / "fex-emu")
    metadata = {"runner_root": str(runner), "runner_literal": str(loader), "prefix": str(prefix),
                "wineserver": str(server), "winemsync": msync, "winedevice_machine": "arm64",
                "winedevice_sha256": hashlib.sha256(host_data).hexdigest(),
                "host_verification": "Existing prefix PE image, not a running-PID assertion"}
    return loader, env, metadata


def stage_consumer(origin: pathlib.Path, prefix: pathlib.Path, digest: str) -> dict:
    data = origin.read_bytes()
    if origin.is_symlink() or hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError("Pinned rumble consumer changed before staging")
    namespace = prefix / "drive_c/skinos-input-qa"
    if not namespace.parent.is_dir() or namespace.is_symlink():
        raise RuntimeError("Invalid existing-prefix staging namespace")
    namespace.mkdir(mode=0o700, exist_ok=True)
    token = "rumble-" + uuid.uuid4().hex
    folder = namespace / token
    folder.mkdir(mode=0o700)
    path = folder / "rumble-consumer.exe"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    with os.fdopen(os.open(path, flags, 0o600), "wb") as stream:
        stream.write(data)
    if hashlib.sha256(path.read_bytes()).hexdigest() != digest:
        raise RuntimeError("Staged rumble consumer hash mismatch; copy preserved")
    return {"origin": str(origin), "origin_sha256": digest, "staged": str(path), "staged_sha256": digest,
            "directory": str(folder), "windows_path": "C:\\skinos-input-qa\\" + token + "\\rumble-consumer.exe"}


def cleanup_stage(stage: dict) -> dict:
    path, folder = pathlib.Path(stage["staged"]), pathlib.Path(stage["directory"])
    if path.parent != folder or path.name != "rumble-consumer.exe" or folder.is_symlink() or path.is_symlink() or not path.is_file():
        return {"removed": False, "preserved_reason": "staging identity changed"}
    if hashlib.sha256(path.read_bytes()).hexdigest() != stage["staged_sha256"]:
        return {"removed": False, "preserved_reason": "staged bytes changed"}
    path.unlink()
    try:
        folder.rmdir()
    except OSError:
        return {"removed": True, "directory_removed": False}
    return {"removed": True, "directory_removed": True}


def path_identity(path, kind):
    if not isinstance(path, str) or not (match := PATH_PATTERN.fullmatch(path)):
        raise RuntimeError("Missing complete owned " + kind.upper() + " device path")
    interface, slot, serial_slot, generation, guid = match.groups()
    if interface.lower() != kind or guid.lower() != GUIDS[kind] or int(slot) != 0 or int(serial_slot) != 0 or int(generation, 16) == 0:
        raise RuntimeError("Invalid owned slot-zero " + kind.upper() + " identity")
    return int(generation, 16)


def validate_result(row: dict, kind: int, bound: dict | None = None, motors: tuple[int, int] | None = None) -> dict:
    if not isinstance(row, dict) or row.get("event") != "wine_rumble_result":
        raise RuntimeError("Unexpected rumble consumer event")
    expected = {"kind": kind, "slot": 0, "owned_slot": 0, "input_bytes": 15, "output_bytes": 5, "feature_bytes": 17,
                "xi_interfaces": 1, "owned_xi_interfaces": 1, "dinput_devices": 1, "owned_dinput_devices": 1,
                "xinput_connected_count": 1, "inspection_error": 0}
    if any(type(row.get(key)) is not int or row[key] != value for key, value in expected.items()):
        raise RuntimeError("Rumble inspection failed caps/count/slot validation")
    for key in ("feature_verified", "input_id_zero_verified", "haptics_caps_verified", "transport_only"):
        if row.get(key) is not True:
            raise RuntimeError("Unverified actual HID property: " + key)
    if row.get("delivery_verified") is not False or row.get("physical_device_verified") is not False:
        raise RuntimeError("Consumer made an unsupported delivery claim")
    generation = path_identity(row.get("device_path"), "xi")
    if type(row.get("generation")) is not int or row["generation"] != generation or path_identity(row.get("ig_path"), "ig") != generation:
        raise RuntimeError("XI/IG generation mismatch")
    identity = {"slot": 0, "generation": generation, "device_path": row["device_path"], "ig_path": row["ig_path"]}
    if bound is not None and any(identity[key].lower() != bound[key].lower() if isinstance(identity[key], str) else identity[key] != bound[key] for key in identity):
        raise RuntimeError("Owned rumble binding changed; refusing retarget")
    if type(row.get("xinput_result")) is not int or not 0 <= row["xinput_result"] <= 0xFFFFFFFF:
        raise RuntimeError("Missing actual XInput result")
    if kind == 6:
        if row.get("ok") is not True or row["xinput_result"] != 0 or row.get("xinput_set_called") is not False:
            raise RuntimeError("Inspection failed or unexpectedly called XInputSetState")
    elif kind == 5:
        if motors is None or any(type(row.get(key)) is not int or row[key] != value for key, value in zip(("left", "right"), motors)):
            raise RuntimeError("Motor echo does not match the actual command")
        if row.get("xinput_set_called") is not True or type(row.get("ok")) is not bool or row["ok"] != (row["xinput_result"] == 0):
            raise RuntimeError("Motor call was refused or its result is inconsistent")
    else:
        raise ValueError("Unsupported QA frame")
    return identity


class Driver:
    def __init__(self, runner: pathlib.Path, prefix: pathlib.Path, logs: pathlib.Path):
        self.child = None
        self.staging = None
        self.bound = None
        self.revoked = False
        self.closed = False
        self.logs = pathlib.Path(logs).absolute()
        self.prefix = pathlib.Path(prefix).absolute()
        loader, env, runtime = prepare_environment(runner, self.prefix)
        program, digest = selected_consumer()
        if self.logs.is_symlink():
            raise RuntimeError("QA logs must not be a symlink")
        self.logs.mkdir(parents=True, exist_ok=True)
        for name in ("game-rumble.jsonl", "game-rumble-meta.json", "rumble.stdout.jsonl", "rumble.stderr.log"):
            if (self.logs / name).exists():
                raise RuntimeError("Refusing to overwrite rumble evidence")
        self.metadata = {"schema_version": 1, "delivery_verified": False, "physical_device_verified": False,
                         "consumer_origin": str(program), "consumer_sha256": digest, **runtime}
        try:
            self.staging = stage_consumer(program, self.prefix, digest)
            self.metadata["staging"] = self.staging
            (self.logs / "game-rumble-meta.json").write_text(json.dumps(self.metadata, indent=2) + "\n")
            self.child = base.Child([str(loader), self.staging["windows_path"]], env, self.logs, "rumble")
            ready = self.child.wait("wine_rumble_consumer_ready", timeout=4)
            if ready.get("transport_only") is not True or ready.get("delivery_verified") is not False:
                raise RuntimeError("Unexpected rumble consumer readiness")
            self.record("ready", result=ready, host_pid=self.child.process.pid)
        except BaseException:
            if self.child is not None:
                try:
                    self.child.close()
                finally:
                    if self.child.process.poll() is not None:
                        self._cleanup_stage()
            elif self.staging is not None:
                self._cleanup_stage()
            raise

    def record(self, event, **fields):
        with (self.logs / "game-rumble.jsonl").open("a") as stream:
            stream.write(json.dumps({"event": event, "monotonic_ns": time.monotonic_ns(), "unix_ns": time.time_ns(), **fields}) + "\n")

    def _exchange(self, kind, payload=b"", motors=None):
        if self.closed or self.child is None or self.revoked:
            raise RuntimeError("Rumble client closed or binding revoked")
        try:
            self.child.send(base.frame(kind, payload))
            row = self.child.wait("wine_rumble_result", timeout=8)
            self.record("inspection" if kind == 6 else "xinput_result", result=row)
            identity = validate_result(row, kind, self.bound, motors)
        except Exception:
            self.revoked = True
            raise
        if self.bound is None:
            self.bound = identity
        return row

    def inspect(self) -> dict:
        return self._exchange(6)

    def set_motors(self, left: int, right: int) -> dict:
        if any(type(value) is not int or not 0 <= value <= 65535 for value in (left, right)):
            raise ValueError("Motor values must be unsigned 16-bit integers")
        self.inspect()  # Fresh binding proof before every command, also checked in C.
        return self._exchange(5, struct.pack("<HH", left, right), (left, right))

    def _cleanup_stage(self):
        if self.staging is not None:
            self.record("staging_cleanup", result=cleanup_stage(self.staging), staged=self.staging["staged"])
            self.staging = None

    def close(self) -> int:
        if self.closed:
            return self.exit_code
        try:
            if self.bound is not None and not self.revoked and self.child is not None and self.child.process.poll() is None:
                try:
                    zero = self.set_motors(0, 0)
                    self.record("cleanup_zero", result=zero, delivery_verified=False)
                except Exception as exc:
                    self.record("cleanup_zero_refused", error_type=type(exc).__name__, reason=str(exc), delivery_verified=False)
            elif self.bound is not None:
                self.record("cleanup_zero_refused", reason="binding revoked or process already exited", delivery_verified=False)
        finally:
            # Consumer EOF independently attempts zero only on its still-owned
            # bound generation. Its exit failure is returned, never concealed.
            self.exit_code = self.child.close() if self.child is not None else 0
            self.closed = True
            self.record("closed", exit_code=self.exit_code)
            if self.child is None or self.child.process.poll() is not None:
                self._cleanup_stage()
        return self.exit_code
