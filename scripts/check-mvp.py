#!/usr/bin/env python3
"""Exercise the real NDJSON backend using a session created in SkinOs.

Never reads or prints Steam tokens. Close SkinOs first to keep one CM session.
The chosen AppID must already belong to the signed-in user.
"""
import argparse
import json
import queue
import subprocess
import threading
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--appid", type=int, default=1942280)
parser.add_argument("--install", action="store_true")
parser.add_argument("--update", action="store_true")
parser.add_argument("--launch", action="store_true")
parser.add_argument("--stop-after-running", action="store_true")
parser.add_argument("--run-for", type=float, default=30, help="Minimum stable running time before requesting Stop")
parser.add_argument("--core", type=Path)
parser.add_argument("--timeout", type=float, default=900)
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]


def original_steam_processes():
    """Read executable paths only, never command-line arguments or environments."""
    result = subprocess.run(["/bin/ps", "-ax", "-o", "pid=,comm="],
                            text=True, capture_output=True, check=True)
    found = {}
    for line in result.stdout.splitlines():
        fields = line.strip().split(maxsplit=1)
        if len(fields) != 2:
            continue
        pid, executable = fields
        original_path = ("/Steam/Steam.AppBundle/" in executable
                         or "/Steam.app/Contents/" in executable)
        gui_name = Path(executable).name in ("steam_osx", "steamwebhelper", "Steam Helper")
        if original_path or gui_name:
            found[int(pid)] = executable
    return found


baseline = original_steam_processes()
if any(Path(path).name != "ipcserver" for path in baseline.values()):
    raise SystemExit("Close the official Steam client before this standalone-client check; no process was stopped.")
stop_observer = threading.Event()
unexpected_original = {}
observer_errors = []


def observe_processes():
    while not stop_observer.is_set():
        try:
            for pid, executable in original_steam_processes().items():
                # An already-existing background broker is not terminated or
                # treated as newly launched. Our private namespace must prove
                # separately that the test did not use this original broker.
                if pid not in baseline:
                    unexpected_original[pid] = executable
        except Exception:
            observer_errors.append("process observation failed")
            return
        stop_observer.wait(0.2)


observer = threading.Thread(target=observe_processes, daemon=True)
observer.start()
process = subprocess.Popen([str(args.core or root / "target/debug/skinos-core")], stdin=subprocess.PIPE,
                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
incoming = queue.Queue()


def reader():
    for line in process.stdout:
        try:
            incoming.put(json.loads(line))
        except json.JSONDecodeError:
            incoming.put({"event": "protocol_error", "payload": {}})
    incoming.put(None)


threading.Thread(target=reader, daemon=True).start()


def send(command, **payload):
    process.stdin.write(json.dumps({"id": command, "command": command, "payload": payload}) + "\n")
    process.stdin.flush()


evidence = {"appid": args.appid, "authenticated": False, "live_library": False,
            "installed": False, "running": False, "events": []}
started = time.monotonic()
last_bucket = -1
failure = None
stop_at = None
stop_sent = False
running_at = None
try:
    while time.monotonic() - started < args.timeout:
        if stop_at is not None and not stop_sent and time.monotonic() >= stop_at:
            send("stop_game", appid=args.appid)
            stop_sent=True
            evidence["stop_requested"]=True
        if unexpected_original:
            raise RuntimeError("Official Steam process was launched; standalone-client verification failed")
        if observer_errors:
            raise RuntimeError("Cannot verify absence of the official Steam client")
        try:
            event = incoming.get(timeout=min(1, args.timeout - (time.monotonic() - started)))
        except queue.Empty:
            if process.poll() is not None:
                raise RuntimeError("Backend terminated")
            continue
        if event is None:
            raise RuntimeError("Backend closed event stream")
        kind, payload = event.get("event"), event.get("payload", {})
        if kind == "ready":
            send("status")
        elif kind == "status" and event.get("id") == "status":
            accounts = payload.get("accounts", [])
            if not accounts:
                raise RuntimeError("Sign in to SkinOs first")
            send("restore_session", steamid=accounts[0]["steamid"])
        elif kind == "authenticated":
            evidence["authenticated"] = True
            print("Authenticated using SkinOs saved session", flush=True)
        elif kind == "library" and not payload.get("cached", True):
            games = payload.get("games", [])
            selected = next((g for g in games if g.get("appid") == args.appid), None)
            if selected is None:
                raise RuntimeError("Chosen game missing from live library")
            first = not evidence["live_library"]
            evidence.update(live_library=True, library_count=len(games),
                            target_macos_supported=selected.get("macos_supported"),
                            target_windows_supported=selected.get("windows_supported"),
                            installed_platform=selected.get("installed_platform"))
            if first:
                print(f"Live library: {len(games)} entries; chosen game present", flush=True)
                if args.install or args.update:
                    send("update_game" if args.update else "install_game", appid=args.appid)
                elif args.launch:
                    send("launch_game", appid=args.appid)
                else:
                    break
        elif kind == "install_progress":
            progress = payload.get("progress")
            evidence["last_progress"] = {k:payload.get(k) for k in ("phase","downloaded_bytes","total_bytes","progress","bytes_per_second","eta_seconds","reused_bytes")}
            bucket = int(progress * 10) if progress is not None else -1
            if bucket != last_bucket or payload.get("phase") != "downloading":
                last_bucket = bucket
                print(f"Install {payload.get('phase')}: {payload.get('downloaded_bytes', 0)}/{payload.get('total_bytes', 0)} bytes", flush=True)
        elif kind == "install_complete":
            evidence["installed"] = True
            evidence["updated"] = args.update
            evidence["install_dir"] = payload.get("install_dir")
            print("Installation completed", flush=True)
        elif kind == "operation_complete":
            if event.get("id") in ("install_game", "update_game"):
                if args.launch:
                    send("launch_game", appid=args.appid)
                else:
                    break
            elif event.get("id") == "launch_game":
                break
        elif kind == "launch_status":
            state = payload.get("state")
            evidence["events"].append({"event": kind, "state": state})
            print(f"Runtime: {state}", flush=True)
            if state == "running":
                evidence["running"] = True
                if running_at is None: running_at=time.monotonic()
                if args.stop_after_running and stop_at is None: stop_at=running_at+args.run_for
            if state == "exited":
                evidence["game_exited"] = True
                if running_at is not None: evidence["running_duration_seconds"]=round(time.monotonic()-running_at,2)
                if args.stop_after_running and not stop_sent:
                    raise RuntimeError("Game exited before the requested stability interval; startup was not validated")
            if state == "failed":
                # Wait for the core's terminal operation error. Stopping it on
                # the intermediate runtime failure would kill the supervisor
                # before its owned launchd broker has been cleaned up.
                evidence["runtime_failure"] = payload.get("message", "Game launch failed")
        elif kind in ("error", "protocol_error"):
            if kind == "error" and event.get("id") is None and payload.get("code") in (
                    "notifications_failed", "notifications_storage_failed", "notifications_busy"):
                # Optional inbox refresh has its own checker. Preserve its
                # warning without mistaking it for a failed game operation.
                evidence.setdefault("notification_warnings", []).append(payload["code"])
                print("Optional Steam notification refresh warning; continuing launch check", flush=True)
                continue
            raise RuntimeError(payload.get("message", "Invalid backend protocol"))
    else:
        raise RuntimeError("End-to-end check timed out")
    required = ["authenticated", "live_library"]
    if args.install or args.update:
        required.append("installed")
    if args.launch:
        required.append("running")
    if args.stop_after_running:
        required.extend(["stop_requested","game_exited"])
    missing = [name for name in required if not evidence[name]]
    if missing:
        raise RuntimeError("Unverified requirements: " + ", ".join(missing))
except Exception as error:
    failure = str(error)
    evidence["error"] = failure
    print(f"FAILED: {failure}", flush=True)
finally:
    if process.poll() is None:
        try:
            send("quit")
            process.wait(timeout=5)
        except (BrokenPipeError, subprocess.TimeoutExpired):
            process.terminate()
            process.wait(timeout=5)
    stop_observer.set()
    observer.join(timeout=2)
    evidence["official_gui_seen"] = bool(unexpected_original)
    evidence["process_observation_failed"] = bool(observer_errors)
    evidence["unexpected_original_processes"] = list(unexpected_original.values())
    if unexpected_original or observer_errors:
        failure = failure or "Standalone process verification failed"
        evidence["error"] = failure
    (root / "outputs").mkdir(exist_ok=True)
    output = "mvp-launch-check.json" if args.launch else "mvp-update-check.json" if args.update else "mvp-install-check.json" if args.install else "mvp-library-check.json"
    (root / "outputs" / output).write_text(json.dumps(evidence, indent=2), encoding="utf-8")
    (root / "outputs/mvp-check.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
if failure:
    raise SystemExit(1)
