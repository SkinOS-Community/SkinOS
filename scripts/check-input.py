#!/usr/bin/env python3
"""Observe genuine Steam Input through the owned host and NotProton.

No physical device is required for a truthful zero-device diagnostic. --neutral
attaches only an explicitly labelled SDL test device; it sends no input events.
This checker never reads credentials or emits an account ID.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import queue
import signal
import subprocess
import threading
import time

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--core', type=Path, required=True)
p.add_argument('--appid', type=int, default=648800)
p.add_argument('--neutral', action='store_true')
p.add_argument('--generic-support', action='store_true', help='Temporarily enable genuine generic-gamepad support for this diagnostic only')
p.add_argument('--require-controller', action='store_true', help='Fail unless the genuine game-facing API reports a controller')
p.add_argument('--require-xinput', action='store_true', help='Fail unless a genuine Steam Input handle maps to a connected XInput slot')
p.add_argument('--require-activity', action='store_true', help='Fail unless genuine correlated input changes were observed')
p.add_argument('--native-sdk', type=Path, help='Compare an original native SDK with the Windows consumer under the same host and AppID')
p.add_argument('--focus', action='store_true', help='Temporarily set and verify the owned runtime input focus for this AppID')
p.add_argument('--stock-config', action='store_true', help='Load the original gamepad layout for the verified neutral fixture')
p.add_argument('--trace-hid', action='store_true', help='Observe the original virtual-HID creation result without altering it')
p.add_argument('--wine-output', action='store_true', help='Use the owned Wine output backend with an explicitly prepared input driver')
p.add_argument('--activity', action='store_true', help='Verify A/left-stick press and release using only the owned SDL software fixture')
p.add_argument('--rumble', action='store_true', help='Observe genuine Steam Input vibration callbacks on the owned SDL software fixture only')
p.add_argument('--timeout', type=float, default=150)
p.add_argument('--output', type=Path, help='Write proof to this run-specific file')
args = p.parse_args()
spec = importlib.util.spec_from_file_location('input_qa', Path(__file__).with_name('check-steam-input-wine.py'))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
if args.activity and not (args.wine_output and args.neutral and args.generic_support and args.focus and args.stock_config):
    p.error('--activity requires --wine-output --neutral --generic-support --focus --stock-config')
if args.rumble and (not args.neutral or args.activity):
    p.error('--rumble requires --neutral and cannot be combined with --activity')
environment = os.environ.copy()
if args.neutral:
    environment['SKINOS_STEAM_INPUT_FIXTURE'] = 'neutral'
else:
    environment.pop('SKINOS_STEAM_INPUT_FIXTURE', None)
if args.generic_support:
    environment['SKINOS_INPUT_GENERIC_SUPPORT'] = '1'
else:
    environment.pop('SKINOS_INPUT_GENERIC_SUPPORT', None)
if args.native_sdk:
    environment['SKINOS_INPUT_NATIVE_SDK'] = str(args.native_sdk.resolve(strict=True))
else:
    environment.pop('SKINOS_INPUT_NATIVE_SDK', None)
if args.focus:
    environment['SKINOS_INPUT_FOCUS'] = '1'
else:
    environment.pop('SKINOS_INPUT_FOCUS', None)
if args.stock_config:
    environment['SKINOS_INPUT_CONFIG'] = '1'
else:
    environment.pop('SKINOS_INPUT_CONFIG', None)
if args.trace_hid:
    environment['SKINOS_INPUT_TRACE'] = '1'
else:
    environment.pop('SKINOS_INPUT_TRACE', None)
if args.wine_output:
    environment['SKINOS_INPUT_OUTPUT_BACKEND'] = 'wine'
else:
    environment.pop('SKINOS_INPUT_OUTPUT_BACKEND', None)
if args.activity:
    environment['SKINOS_INPUT_ACTIVITY'] = '1'
else:
    environment.pop('SKINOS_INPUT_ACTIVITY', None)
environment.pop('SKINOS_INPUT_RUMBLE_FIXTURE_ID', None)
environment.pop('SKINOS_INPUT_RECONNECT_TEST', None)
if args.rumble: environment['SKINOS_INPUT_RUMBLE_TEST'] = '1'
else: environment.pop('SKINOS_INPUT_RUMBLE_TEST', None)
qa.require_exclusive_core()
proc = subprocess.Popen([str(args.core)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.DEVNULL, text=True, env=environment)
monitor = qa.OriginalSteamMonitor()
inbox = queue.Queue()

def reader():
    for line in proc.stdout:
        try: inbox.put(json.loads(line))
        except json.JSONDecodeError: pass
    inbox.put(None)

threading.Thread(target=reader, daemon=True).start()
def send(command, **payload):
    proc.stdin.write(json.dumps({'id': command, 'command': command, 'payload': payload})+'\n')
    proc.stdin.flush()

proof = dict(appid=args.appid, neutral_fixture_requested=args.neutral, generic_support_requested=args.generic_support, physical_device_verified=False)
started = time.monotonic()
owner = None
requested = False
complete = False
operation_finished = False
failure = None
cleanup_started = False
forced_core_shutdown = False

def interrupted(signum, _frame):
    # A second signal must not interrupt the bounded producer drain below.
    if not cleanup_started:
        raise RuntimeError('Diagnostic interrupted by signal '+str(signum))

signal.signal(signal.SIGTERM, interrupted)
signal.signal(signal.SIGINT, interrupted)
try:
    while time.monotonic()-started < args.timeout:
        try: event = inbox.get(timeout=1)
        except queue.Empty: continue
        if event is None: raise RuntimeError('Core event stream closed')
        kind, data = event.get('event'), event.get('payload', {})
        if kind == 'ready': send('auto_login')
        elif kind == 'authenticated':
            owner = data.get('steamid'); proof['authenticated'] = True
        elif kind == 'library' and not data.get('cached', True) and not requested:
            if not any(g.get('appid') == args.appid for g in data.get('games', [])):
                raise RuntimeError('Diagnostic game is absent from the owned library')
            requested = True; send('diagnose_input', appid=args.appid)
        elif kind == 'steam_input_status':
            print('Input runtime:', data.get('state'), flush=True)
        elif kind == 'steam_input_diagnostics':
            if data.get('steamid') != owner or data.get('appid') != args.appid:
                raise RuntimeError('Diagnostic identity mismatch')
            if data.get('summary', {}).get('appid_verified') is not True or data['summary'].get('appid') != args.appid:
                raise RuntimeError('Steam Input probe did not verify the target game context')
            proof.update(identity_matches=True, product_diagnostic=data.get('product_diagnostic'), fixture=data.get('fixture'), summary=data.get('summary'), samples=data.get('samples'), native_input=data.get('native_input'), focus_snapshot=data.get('focus_snapshot'), config_proof=data.get('config_proof'), fixture_activity=data.get('fixture_activity'), rumble_fixture=data.get('rumble_fixture'))
            if data.get('native_input') is not None:
                print('Native Steam Input comparison:', json.dumps(data['native_input']), flush=True)
            print('Genuine Steam Input report:', json.dumps(data.get('summary')), flush=True)
        elif kind == 'operation_complete' and event.get('id') == 'diagnose_input':
            operation_finished = True
            complete = proof.get('summary', {}).get('scan_completed') is True
            if not complete: raise RuntimeError('No completed genuine Steam Input scan')
            break
        elif kind == 'error' and event.get('id') in ('auto_login', 'diagnose_input'):
            if event.get('id') == 'diagnose_input': operation_finished = True
            raise RuntimeError(data.get('message', 'Input diagnostic failed'))
    if not complete: raise RuntimeError('Steam Input diagnostic timed out')
    summary = proof.get('summary', {})
    if args.require_controller and not summary.get('controller_seen'):
        raise RuntimeError('Steam Input did not deliver a controller handle to the game')
    if args.require_xinput and not summary.get('xinput_route_observed'):
        raise RuntimeError('Steam Input did not establish a correlated XInput route')
    if args.require_activity and not summary.get('steam_input_activity_verified'):
        raise RuntimeError('Steam Input did not deliver correlated input changes')
    if args.activity and not proof.get('fixture_activity', {}).get('pressed_and_released_verified'):
        raise RuntimeError('Software fixture did not complete the genuine Steam Input press/release roundtrip')
    if args.rumble:
        rumble = proof.get('rumble_fixture') or {}
        if not all(rumble.get(key) is True for key in ['software_sink_verified','ordered_callback_verified','cleanup_seen','parent_route_verified']):
            raise RuntimeError('Genuine Steam Input rumble callback/lifecycle proof was not verified')
        if rumble.get('fixture') is not True or rumble.get('physical_device_verified') is not False or len(rumble.get('sdk_dispatches',[])) != 4:
            raise RuntimeError('Rumble fixture scope or dispatch transcript is incomplete')
except Exception as error:
    failure = str(error); print('FAILED:', failure, flush=True)
finally:
    cleanup_started = True
    # A core exit alone is insufficient: wait for the diagnostic operation to
    # finish tearing down its producer before quitting the supervising core.
    if requested and not operation_finished and proc.poll() is None:
        try:
            send('stop_game', appid=args.appid)
            until = time.monotonic()+35
            while time.monotonic() < until:
                try: event = inbox.get(timeout=min(1, max(0.01, until-time.monotonic())))
                except queue.Empty: continue
                if event is None: break
                if event.get('id') == 'diagnose_input' and event.get('event') in ('operation_complete', 'error'):
                    operation_finished = True
                    break
        except (BrokenPipeError, OSError): pass
    if proc.poll() is None:
        try:
            send('quit')
            proc.wait(timeout=15)
        except (BrokenPipeError, OSError, subprocess.TimeoutExpired):
            forced_core_shutdown = True
            proc.terminate()
            try: proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
    shutdown_confirmed = proc.returncode == 0 and not forced_core_shutdown and (not requested or operation_finished)
    if not shutdown_confirmed and failure is None:
        failure = 'Core producer shutdown was not confirmed'
    proof.update(monitor.close())
    if (proof['original_gui_seen'] or not proof['original_gui_observation_complete']) and failure is None:
        failure = 'Could not confirm that original Steam GUI remained absent'
    proof.update(scan_completed=complete, requirements_satisfied=complete and failure is None,
                 required={'controller': args.require_controller, 'xinput': args.require_xinput, 'activity': args.require_activity},
                 error=failure, core_exit_code=proc.returncode, checker_pid=os.getpid(), core_pid=proc.pid,
                 operation_finished=operation_finished, forced_core_shutdown=forced_core_shutdown,
                 core_shutdown_confirmed=shutdown_confirmed,
                 elapsed_seconds=round(time.monotonic()-started, 2))
    name = 'steam-input-generic-check.json' if args.generic_support else ('steam-input-neutral-check.json' if args.neutral else 'steam-input-no-device-check.json')
    if args.native_sdk: name = 'steam-input-native-differential.json'
    if args.focus: name = 'steam-input-focus-check.json'
    if args.stock_config: name = 'steam-input-stock-config-check.json'
    if args.wine_output: name = 'steam-input-wine-output-check.json'
    if args.activity: name = 'steam-input-wine-activity-check.json'
    if args.rumble: name = 'steam-input-rumble-fixture-check.json'
    output = args.output or Path(__file__).resolve().parents[1]/'outputs'/name
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(proof, indent=2))
if failure: raise SystemExit(1)
