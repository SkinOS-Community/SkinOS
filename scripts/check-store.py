#!/usr/bin/env python3
"""Read-only live store/social check using SkinOS's own saved session.

Close the GUI before running this so only one core session is active. Does not
launch/stop games, send chats, change presence, or make purchases. Never prints
or saves raw IPC, account IDs, friend names, chat contents, cookies or tokens.
"""
import argparse
import json
import queue
import subprocess
import threading
import time
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--timeout", type=float, default=120)
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
process = subprocess.Popen(
    [str(root / "target/debug/skinos-core")], stdin=subprocess.PIPE,
    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
)
events = queue.Queue()


def reader():
    for line in process.stdout:
        try:
            events.put(json.loads(line))
        except json.JSONDecodeError:
            events.put({"event": "invalid_json"})
    events.put(None)


threading.Thread(target=reader, daemon=True).start()


def send(request_id, command, **payload):
    process.stdin.write(json.dumps({"id": request_id, "command": command, "payload": payload}) + "\n")
    process.stdin.flush()


evidence = {"authenticated": False, "live_library": False, "web": {}, "details": {},
            "friends_loaded": False, "profile_loaded": False, "chat_history_checked": False,
            "chat_messages_sent": 0, "purchases_made": 0}
expected_id = None
pending = set()
started_checks = False
started_logout = False
complete = False
failure = None
started = time.monotonic()
try:
    while time.monotonic() - started < args.timeout:
        try:
            event = events.get(timeout=1)
        except queue.Empty:
            continue
        if event is None:
            raise RuntimeError("core_closed_before_completion")
        kind = event.get("event")
        payload = event.get("payload", {})
        request_id = event.get("id")
        if kind == "ready":
            send("status", "status")
        elif kind == "status" and request_id == "status":
            accounts = payload.get("accounts", [])
            if not accounts:
                raise RuntimeError("no_saved_skinos_session")
            expected_id = accounts[0]["steamid"]
            send("restore", "restore_session", steamid=expected_id)
        elif kind == "authenticated":
            if payload.get("steamid") != expected_id:
                raise RuntimeError("authenticated_account_mismatch")
            evidence["authenticated"] = True
            print("Saved SkinOS session authenticated", flush=True)
        elif kind == "library" and not payload.get("cached", True):
            evidence["live_library"] = True
            evidence["library_count"] = len(payload.get("games", []))
            if not started_checks:
                started_checks = True
                for target in ("store", "community"):
                    key = "web_" + target
                    pending.add(key)
                    send(key, "get_web_session", target=target)
                for appid in (1942280, 629520):
                    key = "details_" + str(appid)
                    pending.add(key)
                    send(key, "get_game_details", appid=appid)
                pending.update(("friends", "profile"))
                send("friends", "get_friends")
                send("profile", "get_profile")
        elif kind == "web_session" and request_id in pending:
            target = payload.get("target")
            cookies = payload.get("cookies", [])
            hosts = {"store.steampowered.com", "steamcommunity.com", "checkout.steampowered.com"}
            expected_pairs = {(host, name) for host in hosts for name in ("steamLoginSecure", "sessionid")}
            actual_pairs = {(cookie.get("domain"), cookie.get("name")) for cookie in cookies}
            valid = (payload.get("steamid") == expected_id and len(cookies) == 6
                     and actual_pairs == expected_pairs
                     and all(cookie.get("secure") and cookie.get("path") == "/"
                             and cookie.get("value") and cookie.get("expires_at", 0) > time.time()
                             and (cookie.get("name") != "steamLoginSecure" or cookie.get("http_only"))
                             for cookie in cookies))
            if not valid:
                raise RuntimeError("invalid_cookie_scope_or_account")
            evidence["web"][target] = {"account_matches": True, "server_identity_verified": True,
                                       "cookie_scope_valid": True, "cookie_count": len(cookies)}
            pending.remove(request_id)
            print(f"{target}: Steam accepted account-scoped HTTPS cookies", flush=True)
        elif kind == "game_details" and request_id in pending:
            appid = payload.get("appid")
            if request_id != "details_" + str(appid):
                raise RuntimeError("metadata_appid_mismatch")
            platforms = payload.get("platforms", {})
            requirements = payload.get("requirements", {})
            supported = [key for key in ("windows", "mac", "linux") if platforms.get(key)]
            if not supported or any(not isinstance(requirements.get(key, {}).get("minimum"), str)
                                    for key in ("windows", "mac", "linux")):
                raise RuntimeError("invalid_requirements_metadata")
            evidence["details"][str(appid)] = {"name": payload.get("name"), "supported_platforms": supported,
                                               "requirements_present": [key for key in supported if requirements[key]["minimum"]],
                                               "store_url_valid": payload.get("store_url") == f"https://store.steampowered.com/app/{appid}/"}
            pending.remove(request_id)
            print(f"Game {appid}: requirements for {', '.join(supported)}", flush=True)
        elif kind == "friends" and request_id == "friends":
            friends = payload.get("friends", [])
            evidence["friends_loaded"] = True
            evidence["friend_count"] = len(friends)
            evidence["named_friend_count"] = sum(bool(row.get("persona_name")) and row.get("persona_name") != row.get("steamid") for row in friends)
            pending.discard(request_id)
            if friends:
                pending.add("history")
                send("history", "get_chat_history", steamid=friends[0]["steamid"])
            else:
                evidence["chat_history_skipped"] = "no_friends"
            print(f"Friends received: {len(friends)}", flush=True)
        elif kind == "profile" and request_id == "profile":
            if payload.get("steamid") != expected_id or payload.get("state") != "invisible":
                raise RuntimeError("profile_account_or_default_presence_mismatch")
            evidence["profile_loaded"] = True
            evidence["default_presence"] = payload.get("state")
            pending.discard(request_id)
            print("Own profile received; default presence is invisible", flush=True)
        elif kind == "chat_history" and request_id == "history":
            evidence["chat_history_checked"] = True
            evidence["chat_history_count"] = len(payload.get("messages", []))
            pending.discard(request_id)
            print("Chat history read successfully; no messages sent", flush=True)
        elif kind == "status" and request_id == "logout" and payload.get("state") == "signed_out":
            send("after_logout", "get_web_session", target="store")
        elif kind == "error":
            code = payload.get("code", "unknown")
            if request_id == "after_logout" and code == "not_authenticated":
                evidence["web_denied_after_logout"] = True
                complete = True
                break
            # Classify known local errors only; never include server payloads.
            if code == "web_session_failed":
                reason = payload.get("message", "")
                for marker in ("Steam website did not confirm", "Cannot verify the Steam website login",
                               "Steam web request failed (HTTP", "Steam website authentication failed (Steam result",
                               "Steam web response exceeds", "Steam website redirected", "Steam website returned too many redirects",
                               "Steam website authentication timed out"):
                    if reason.startswith(marker):
                        evidence["web_failure_reason"] = marker
                        print("Website check: " + marker, flush=True)
                        break
            # No server-supplied text or request payload can reach diagnostic files.
            raise RuntimeError("core_error_" + str(code)[:80])
        elif kind == "invalid_json":
            raise RuntimeError("invalid_core_json")
        if started_checks and not pending and not started_logout:
            started_logout = True
            send("logout", "logout")
    if not complete:
        raise RuntimeError("check_timed_out")
except Exception as error:
    failure = str(error) if isinstance(error, RuntimeError) else type(error).__name__
finally:
    try:
        if process.poll() is None:
            send("quit", "quit")
            process.wait(timeout=8)
    except (BrokenPipeError, subprocess.TimeoutExpired):
        process.kill()
        process.wait()
    evidence["completed"] = complete
    evidence["error"] = failure
    evidence["elapsed_seconds"] = round(time.monotonic() - started, 1)
    evidence["core_exit_code"] = process.returncode
    output = root / "outputs/store-social-check.json"
    output.parent.mkdir(exist_ok=True)
    output.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n")
    print("Sanitized proof saved: outputs/store-social-check.json", flush=True)
if failure:
    raise SystemExit(failure)
