#!/usr/bin/env python3
"""Live reconnect QA through the real SkinOS NDJSON interface.

Close other SkinOS sessions before running. This launches only the requested
installed game, closes this core's CM WebSocket once, and stops its own game.
It never reads credential files or prints tokens, account IDs or raw events.
Original-Steam process observation can run independently beside this checker.
"""
import argparse
import json
import math
import os
import queue
import re
import subprocess
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OPTIONAL_ERRORS = {"notifications_failed", "notifications_storage_failed", "notifications_busy"}


class CheckFailed(Exception):
    pass


class ReconnectCheck:
    def __init__(self, args):
        self.args = args
        self.incoming = queue.Queue()
        self.process = None
        self.started = time.monotonic()
        self.deadline = self.started + args.timeout
        self.account_id = None  # Memory only; never included in evidence or output.
        self.account_epoch = None
        self.auth_count = 0
        self.live_count = 0
        self.latest_library = None
        self.latest_library_time = None
        self.initial_game_ids = None
        self.disconnect_sent_at = None
        self.disconnected_at = None
        self.retry_at = None
        self.reauthenticated_at = None
        self.debug_ack = False
        self.running = False
        self.launch_requested = False
        self.game_exited = False
        self.game_drained = False
        self.stop_requested = False
        self.logout_requested = False
        self.logout_guard = False
        self.restore_needed = False
        self.restore_requested = False
        self.restore_authenticated = False
        self.evidence = {
            "appid": args.appid, "game_requested": not args.no_game,
            "authenticated": False, "live_library": False,
            "debug_disconnect_count": 0, "identity_preserved": None,
            "account_epoch_preserved": None, "library_cleared_during_recovery": False,
            "game_failed_or_exited_during_recovery": False,
            "reconnected": False, "logout_suppression_verified": False,
            "auto_login_restored": False, "events": [],
        }

    def send(self, command, request_id=None, **payload):
        request_id = request_id or command
        self.process.stdin.write(json.dumps({"id": request_id, "command": command, "payload": payload}) + "\n")
        self.process.stdin.flush()
        return request_id

    def start(self):
        environment = dict(os.environ, SKINOS_ENABLE_CONNECTION_DIAGNOSTICS="1")
        self.process = subprocess.Popen([str(self.args.core_path)], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=environment)
        def read():
            try:
                for line in self.process.stdout:
                    received_at = time.monotonic()
                    try:
                        self.incoming.put((received_at, json.loads(line)))
                    except json.JSONDecodeError:
                        self.incoming.put((received_at, {"event": "protocol_error", "payload": {}}))
            finally:
                self.incoming.put((time.monotonic(), None))
        threading.Thread(target=read, daemon=True).start()

    def observe(self, at, event, tolerate_errors=False):
        kind, payload = event.get("event"), event.get("payload", {})
        if not isinstance(payload, dict):
            raise CheckFailed("Backend returned a malformed payload")
        state = payload.get("state")
        if kind in {"authenticated", "status", "launch_status", "diagnostic"}:
            record = {"at_seconds": round(at - self.started, 6), "event": kind}
            if state in {"connecting", "reconnecting", "authenticated", "loading_library", "signed_out", "auth_required", "disconnected", "starting", "running", "stopping", "exited", "failed"}:
                record["state"] = state
            if kind == "status" and isinstance(payload.get("retry_in_seconds"), int):
                record["retry_in_seconds"] = payload["retry_in_seconds"]
            self.evidence["events"].append(record)
        if self.logout_guard and (kind == "authenticated" or kind == "status" and state in {"connecting", "reconnecting"}):
            raise CheckFailed("Explicit logout unexpectedly restarted authentication")
        if kind == "authenticated":
            identity = payload.get("steamid")
            if not isinstance(identity, str) or not identity:
                raise CheckFailed("Authentication did not provide an account identity")
            self.auth_count += 1
            if self.account_id is None:
                self.account_id = identity
                self.account_epoch = payload.get("generation")
                self.evidence["authenticated"] = True
            elif identity != self.account_id:
                self.evidence["identity_preserved"] = False
                raise CheckFailed("Reconnect authenticated a different account")
            elif self.disconnect_sent_at is not None and not self.logout_requested:
                self.reauthenticated_at = at
                self.evidence["identity_preserved"] = True
                self.evidence["account_epoch_preserved"] = payload.get("generation") == self.account_epoch
                if not self.evidence["account_epoch_preserved"]:
                    raise CheckFailed("Reconnect changed the account operation epoch")
            if self.restore_requested:
                if payload.get("session_saved") is not True:
                    raise CheckFailed("Cleanup login did not persist the restored automatic-login preference")
                self.restore_authenticated = True
        elif kind == "library":
            if self.account_id is not None and payload.get("steamid") != self.account_id:
                raise CheckFailed("Library event belongs to another account")
            games = payload.get("games")
            if not isinstance(games, list):
                raise CheckFailed("Library event is malformed")
            if self.disconnected_at is not None and not self.logout_requested and not games and self.initial_game_ids:
                self.evidence["library_cleared_during_recovery"] = True
                raise CheckFailed("Reconnect cleared the loaded game library")
            if not payload.get("cached", True):
                self.live_count += 1
                self.latest_library = games
                self.latest_library_time = at
        elif kind == "status":
            if self.disconnect_sent_at is not None and not self.logout_requested:
                if state == "reconnecting" and self.disconnected_at is None:
                    if payload.get("retry_in_seconds") != 10:
                        raise CheckFailed("Reconnect did not schedule the required ten-second delay")
                    self.disconnected_at = at
                    self.evidence["offline_account_preserved"] = (payload.get("account") or {}).get("steamid") == self.account_id
                    if not self.evidence["offline_account_preserved"]:
                        raise CheckFailed("Transport loss discarded the current account")
                    print("CM transport closed; ten-second retry scheduled", flush=True)
                if state == "connecting" and payload.get("reconnecting") is True and self.retry_at is None:
                    if self.disconnected_at is None:
                        raise CheckFailed("Retry started without a disconnect status")
                    self.retry_at = at
                    delay = at - self.disconnected_at
                    self.evidence["disconnect_to_retry_seconds"] = round(delay, 6)
                    if delay < 10:
                        raise CheckFailed("Retry started before the required ten-second delay")
                if state in {"signed_out", "auth_required"}:
                    raise CheckFailed("Recoverable transport loss unexpectedly required a new login")
        elif kind == "diagnostic" and payload.get("action") == "cm_transport_closed":
            self.debug_ack = True
        elif kind == "launch_status" and payload.get("appid") == self.args.appid:
            if state == "running":
                self.running = True
                self.evidence["game_running"] = True
            if state in {"failed", "exited"}:
                self.running = False
                self.game_exited = state == "exited"
                if self.disconnect_sent_at is not None and not self.stop_requested:
                    self.evidence["game_failed_or_exited_during_recovery"] = True
                    raise CheckFailed("Game stopped or failed during CM recovery")
                if state == "failed" and not tolerate_errors:
                    raise CheckFailed("Game launch failed")
        elif kind == "operation_complete" and event.get("id") == "launch-game":
            self.game_drained = True
        elif kind in {"error", "protocol_error"}:
            if event.get("id") == "launch-game":
                self.game_drained = True
            code = payload.get("code", "protocol_error")
            if code in OPTIONAL_ERRORS and event.get("id") is None:
                self.evidence.setdefault("optional_warnings", []).append(code)
            elif not tolerate_errors:
                safe_code = code if isinstance(code, str) and re.fullmatch(r"[a-z_]{1,64}", code) else "unknown"
                raise CheckFailed("Backend error: " + safe_code)
        return kind, payload

    def next_event(self, deadline=None, tolerate_errors=False):
        deadline = self.deadline if deadline is None else deadline
        while time.monotonic() < deadline:
            try:
                at, event = self.incoming.get(timeout=min(0.25, max(0.001, deadline - time.monotonic())))
            except queue.Empty:
                if self.process.poll() is not None:
                    raise CheckFailed("Backend exited before completing verification")
                continue
            if event is None:
                raise CheckFailed("Backend closed its event stream")
            self.observe(at, event, tolerate_errors=tolerate_errors)
            return at, event
        raise CheckFailed("Verification timed out")

    def until(self, predicate, deadline=None, tolerate_errors=False):
        while True:
            at, event = self.next_event(deadline, tolerate_errors=tolerate_errors)
            if predicate(event):
                return at, event

    def stop_game(self, deadline=None, tolerate_errors=False):
        if not self.launch_requested or self.game_drained:
            return
        self.stop_requested = True
        self.send("stop_game", appid=self.args.appid)
        self.until(lambda _: self.game_drained, deadline, tolerate_errors=tolerate_errors)
        if not self.game_exited:
            raise CheckFailed("Game operation drained without a clean exited status")
        self.evidence["game_exited_cleanly"] = True

    def restore_automatic_login(self, deadline=None):
        if not self.restore_needed:
            return
        self.logout_guard = False
        if not self.account_id:
            raise CheckFailed("Cannot restore login preference without the original account identity")
        if not self.restore_requested:
            self.restore_requested = True
            self.send("restore_session", request_id="restore-autologin", steamid=self.account_id)
        if not self.restore_authenticated:
            self.until(lambda _: self.restore_authenticated, deadline)
        self.send("status", request_id="verify-autologin")
        _, event = self.until(lambda event: event.get("event") == "status" and event.get("id") == "verify-autologin", deadline)
        payload = event["payload"]
        if payload.get("auto_login_disabled") is not False or payload.get("state") != "authenticated":
            raise CheckFailed("Automatic-login preference was not restored")
        self.restore_needed = False
        self.evidence["auto_login_restored"] = True
        print("Automatic login restored through an explicit saved-session login", flush=True)

    def run(self):
        self.start()
        self.until(lambda event: event.get("event") == "ready")
        self.send("status", request_id="startup-status")
        _, status = self.until(lambda event: event.get("event") == "status" and event.get("id") == "startup-status")
        if status["payload"].get("auto_login_disabled") is True:
            raise CheckFailed("Automatic login is disabled or no saved account exists; choose an account in SkinOS first")
        self.send("auto_login", request_id="automatic-login")
        self.until(lambda _: self.evidence["authenticated"])
        self.until(lambda _: self.live_count >= 1)
        self.initial_game_ids = {row.get("appid") for row in self.latest_library}
        self.evidence.update(live_library=True, initial_library_count=len(self.latest_library))
        print(f"Automatic login and fresh library verified ({len(self.latest_library)} games)", flush=True)
        if not self.args.no_game:
            selected = next((row for row in self.latest_library if row.get("appid") == self.args.appid), None)
            if selected is None or not selected.get("installed_dir"):
                raise CheckFailed("Requested game must already be owned and installed")
            self.launch_requested = True
            self.send("launch_game", request_id="launch-game", appid=self.args.appid)
            self.until(lambda _: self.running)
            print("Requested game is running through SkinOS", flush=True)
        baseline_libraries = self.live_count
        self.disconnect_sent_at = time.monotonic()
        self.evidence["debug_disconnect_count"] += 1
        self.send("debug_disconnect", request_id="close-one-cm")
        self.until(lambda _: self.debug_ack and self.reauthenticated_at is not None and self.live_count > baseline_libraries)
        if self.retry_at is None or self.disconnected_at is None:
            raise CheckFailed("No timed reconnect attempt was observed")
        self.evidence["disconnect_to_auth_seconds"] = round(self.reauthenticated_at - self.disconnected_at, 6)
        self.evidence["reconnected_library_count"] = len(self.latest_library)
        self.evidence["fresh_library_after_reconnect"] = self.latest_library_time >= self.reauthenticated_at
        if not self.evidence["fresh_library_after_reconnect"]:
            raise CheckFailed("Fresh library did not follow the second authentication")
        if self.args.appid not in {row.get("appid") for row in self.latest_library} and not self.args.no_game:
            raise CheckFailed("Requested game disappeared after reconnection")
        self.evidence["reconnected"] = True
        self.evidence["game_preserved"] = self.running if not self.args.no_game else None
        print(f"Same account reauthenticated after {self.evidence['disconnect_to_auth_seconds']:.3f}s; fresh library verified", flush=True)
        self.stop_game()
        # Logout changes a persisted preference. From this point finally must
        # restore it through the core, not by editing files or reading tokens.
        self.logout_requested = True
        self.restore_needed = True
        self.send("logout", request_id="explicit-logout")
        self.until(lambda event: event.get("event") == "status" and event.get("id") == "explicit-logout" and event.get("payload", {}).get("state") == "signed_out")
        self.logout_guard = True
        guard_until = time.monotonic() + 12
        if guard_until > self.deadline:
            raise CheckFailed("Not enough time remains for the twelve-second logout verification")
        while time.monotonic() < guard_until:
            try:
                self.next_event(guard_until)
            except CheckFailed as error:
                if str(error) == "Verification timed out" and time.monotonic() >= guard_until:
                    break
                raise
        self.evidence["logout_suppression_verified"] = True
        print("Explicit logout stayed signed out for twelve seconds", flush=True)
        self.restore_automatic_login()

    def cleanup(self):
        errors = []
        if self.process is None:
            return errors
        if self.process.poll() is None:
            self.logout_guard = False
            try:
                self.stop_game(time.monotonic() + 45, tolerate_errors=True)
            except Exception:
                errors.append("Could not verify clean shutdown of the test game")
            if self.restore_needed:
                try:
                    self.restore_automatic_login(time.monotonic() + 90)
                except Exception:
                    errors.append("Automatic login still requires manual restoration in SkinOS")
            try:
                self.send("quit")
                self.process.wait(timeout=10)
            except (BrokenPipeError, OSError, subprocess.TimeoutExpired):
                self.process.terminate()
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)
                errors.append("Test core needed forced shutdown")
        elif self.restore_needed:
            errors.append("Core exited before automatic-login preference could be restored")
        self.evidence["core_exit_code"] = self.process.returncode
        return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core-path", "--corepath", "--core", dest="core_path", type=Path,
                        default=ROOT / "target/debug/skinos-core")
    parser.add_argument("--appid", type=int, default=629520)
    parser.add_argument("--timeout", type=float, default=180)
    parser.add_argument("--no-game", action="store_true", help="Check real connection recovery and logout without launching a game")
    parser.add_argument("--output", type=Path, default=ROOT / "outputs/reconnect-check.json")
    args = parser.parse_args()
    if args.appid <= 0 or not math.isfinite(args.timeout) or args.timeout <= 30:
        parser.error("Use a positive AppID and a timeout greater than 30 seconds")
    args.core_path = args.core_path.resolve()
    check = ReconnectCheck(args)
    failure = None
    try:
        check.run()
    except KeyboardInterrupt:
        failure = "Verification interrupted; cleanup requested"
    except CheckFailed as error:
        failure = str(error)
    except Exception as error:
        # Exception strings from arbitrary child or filesystem data are not logged.
        failure = "Verification failed (" + type(error).__name__ + ")"
    finally:
        cleanup_errors = check.cleanup()
        if cleanup_errors:
            check.evidence["cleanup_errors"] = cleanup_errors
            failure = failure or cleanup_errors[0]
        check.evidence["passed"] = failure is None
        check.evidence["error"] = failure
        check.evidence["elapsed_seconds"] = round(time.monotonic() - check.started, 3)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(check.evidence, indent=2) + "\n", encoding="utf-8")
    if failure:
        print("FAILED: " + failure, flush=True)
        raise SystemExit(1)
    scope = "reconnect, logout cancellation and automatic login restoration" if args.no_game else "reconnect, game continuity, logout cancellation and automatic login restoration"
    print("PASS: " + scope + " verified", flush=True)


if __name__ == "__main__":
    main()
