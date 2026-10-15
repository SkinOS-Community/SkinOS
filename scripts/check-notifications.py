#!/usr/bin/env python3
"""Read-only Steam-notification check; close the GUI before running.

Never sends chats, marks notifications read, or saves message bodies, URLs,
account/friend identities or credentials. Authentication remains inside core.
"""
import argparse
import collections
import json
from pathlib import Path
import queue
import subprocess
import threading
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--core', type=Path, required=True)
parser.add_argument('--timeout', type=float, default=120)
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
process = subprocess.Popen([str(args.core)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL, text=True)
inbox = queue.Queue()


def reader():
    for line in process.stdout:
        try:
            inbox.put(json.loads(line))
        except json.JSONDecodeError:
            inbox.put({'event': 'invalid_json'})
    inbox.put(None)


threading.Thread(target=reader, daemon=True).start()


def send(rid, command, **payload):
    process.stdin.write(json.dumps({'id': rid, 'command': command, 'payload': payload}) + '\n')
    process.stdin.flush()


proof = {'authenticated': False, 'live_library': False, 'snapshot_received': False,
         'messages_sent': 0, 'notifications_marked_read': 0}
expected = None
requested = False
complete = False
failure = None
started = time.monotonic()
try:
    while time.monotonic() - started < args.timeout:
        try:
            event = inbox.get(timeout=1)
        except queue.Empty:
            continue
        if event is None:
            raise RuntimeError('core_closed_early')
        kind, rid, data = event.get('event'), event.get('id'), event.get('payload', {})
        if kind == 'ready':
            send('status', 'status')
        elif kind == 'status' and rid == 'status':
            accounts = data.get('accounts', [])
            if not accounts:
                raise RuntimeError('no_saved_account')
            expected = accounts[0]['steamid']
            send('restore', 'restore_session', steamid=expected)
        elif kind == 'authenticated':
            if data.get('steamid') != expected:
                raise RuntimeError('wrong_authenticated_account')
            proof['authenticated'] = True
            if not requested:
                requested = True
                proof['requested_before_library'] = not proof['live_library']
                send('notifications', 'get_notifications')
        elif kind == 'library' and not data.get('cached', True):
            proof['live_library'] = True
            if not requested:
                requested = True
                send('notifications', 'get_notifications')
        elif kind == 'steam_notifications' and rid == 'notifications':
            if not proof['live_library']:
                raise RuntimeError('notification_fetch_preempted_initial_library')
            if data.get('steamid') != expected:
                raise RuntimeError('wrong_notification_account')
            rows = data.get('notifications')
            if not isinstance(rows, list) or len(rows) > 200:
                raise RuntimeError('invalid_or_unbounded_snapshot')
            ids = [row.get('id') for row in rows]
            if any(not isinstance(i, str) or not i for i in ids) or len(set(ids)) != len(ids):
                raise RuntimeError('invalid_or_duplicate_ids')
            if any(not isinstance(row.get('title'), str) or not isinstance(row.get('read'), bool) for row in rows):
                raise RuntimeError('invalid_notification_row')
            proof.update(snapshot_received=True, account_matches=True, count=len(rows),
                         types=dict(collections.Counter(str(row.get('type')) for row in rows)),
                         unread_count=data.get('unread_count'),
                         pending_gift_count=data.get('pending_gift_count'),
                         pending_friend_count=data.get('pending_friend_count'),
                         pending_family_invite_count=data.get('pending_family_invite_count'))
            print('Live Steam notification snapshot validated; count:', len(rows), flush=True)
            send('logout', 'logout')
        elif kind == 'status' and rid == 'logout' and data.get('state') == 'signed_out':
            send('after_logout', 'get_notifications')
        elif kind == 'error':
            code = data.get('code', 'unknown')
            if rid == 'after_logout' and code == 'not_authenticated':
                proof['denied_after_logout'] = True
                complete = True
                break
            if rid in ('restore', 'notifications'):
                # Only the local error classification reaches this artifact.
                raise RuntimeError('core_error_' + str(code)[:80])
        elif kind == 'invalid_json':
            raise RuntimeError('invalid_core_json')
    if not complete:
        raise RuntimeError('check_timed_out')
except Exception as error:
    failure = str(error) if isinstance(error, RuntimeError) else type(error).__name__
finally:
    if process.poll() is None:
        try:
            send('quit', 'quit')
            process.wait(timeout=15)
        except (BrokenPipeError, subprocess.TimeoutExpired):
            process.terminate()
            process.wait(timeout=5)
    proof.update(passed=complete and failure is None, error=failure,
                 core_exit_code=process.returncode, elapsed_seconds=round(time.monotonic() - started, 2))
    (root / 'outputs/notifications-live-check.json').write_text(json.dumps(proof, indent=2))
if failure:
    raise SystemExit(failure)
