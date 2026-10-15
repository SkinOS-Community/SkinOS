#!/usr/bin/env python3
"""Controlled genuine Steam Input diagnostic with a temporary owned Wine bus.

Requires an existing prepared game prefix. Registers only SkinOsInputBus, runs
the normal authenticated core diagnostic, then removes that same owned bus.
No game is launched, no physical controller is driven, no account file is read.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('owned_wine_fixture', ROOT/'scripts/check-wine-input.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def require_exclusive_core():
    """A second authenticated core can displace the UI's live Steam session."""
    result = subprocess.run(['ps', '-axo', 'comm='], capture_output=True, text=True, timeout=3, check=True)
    if any(line.strip() == 'skinos-core' or line.strip().endswith('/skinos-core') for line in result.stdout.splitlines()):
        raise RuntimeError('Close the existing SkinOS application/backend before standalone authenticated QA')


class OriginalSteamMonitor:
    """Read comm only; never capture arguments, environments or account data."""
    def __init__(self):
        self.seen = False
        self.samples = 0
        self.errors = 0
        self.stop = threading.Event()
        self.sample()
        self.thread = threading.Thread(target=self.observe, daemon=True)
        self.thread.start()

    def sample(self):
        try:
            result = subprocess.run(['ps', '-axo', 'comm='], capture_output=True, text=True, timeout=2, check=True)
            self.samples += 1
            self.seen |= any(line.strip() == 'steam_osx' or line.strip().endswith((
                                '/Steam.app/Contents/MacOS/steam_osx',
                                '/Steam.AppBundle/Steam/Contents/MacOS/steam_osx'))
                             for line in result.stdout.splitlines())
        except (OSError, subprocess.SubprocessError): self.errors += 1

    def observe(self):
        while not self.stop.wait(0.5): self.sample()

    def close(self):
        self.stop.set()
        self.thread.join(timeout=3)
        self.sample()
        return dict(original_gui_seen=self.seen, original_gui_samples=self.samples,
                    original_gui_observation_complete=self.errors == 0 and not self.thread.is_alive())


def stop_checker(process):
    """Give its SIGTERM handler time to drain the owned core and producer."""
    if process.poll() is not None: return False
    process.terminate()
    try:
        process.wait(timeout=65)
        return False
    except subprocess.TimeoutExpired:
        # Only the new session we created. Never touch another Wine session.
        try: os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError: pass
        process.wait(timeout=5)
        return True


def shutdown_proof(path, checker_pid, appid):
    try:
        if path.stat().st_size > 8*1024*1024: return False
        proof = json.loads(path.read_text())
        return (proof.get('checker_pid') == checker_pid and proof.get('appid') == appid
                and proof.get('core_shutdown_confirmed') is True
                and proof.get('forced_core_shutdown') is False)
    except (OSError, ValueError): return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', required=True, type=Path)
    parser.add_argument('--runner', required=True, type=Path)
    parser.add_argument('--appid', type=int, default=648800)
    parser.add_argument('--activity', action='store_true')
    parser.add_argument('--log-dir', required=True, type=Path)
    args = parser.parse_args()
    require_exclusive_core()
    app, runner, logs = args.app.absolute(), args.runner.absolute(), args.log_dir.absolute()
    logs.mkdir(parents=True, exist_ok=False)
    prefix = Path.home()/f'Library/Application Support/Steam/steamapps/compatdata/{args.appid}/pfx'
    if not (prefix/'drive_c/windows/system32/winedevice.exe').is_file():
        raise RuntimeError('Diagnostic requires the selected already-prepared game prefix')
    # The present controlled fixture uses the verified FEX ARM64 service host.
    wine_device = (prefix/'drive_c/windows/system32/winedevice.exe').read_bytes()
    offset = int.from_bytes(wine_device[60:64], 'little')
    if wine_device[offset:offset+6] != b'PE\0\0\x64\xaa':
        raise RuntimeError('This diagnostic requires ARM64 winedevice')
    loader = runner/'skinos-launcher/aarch64-unix/Wine.app/Contents/MacOS/wine'
    payload = app/'Contents/Resources/notproton/wine-input'
    manifest = json.loads((payload/'manifest.json').read_text())
    for name in ['arm64/feeder.exe', 'arm64/consumer.exe', 'arm64/skinosinput.sys', 'arm64/skinosinput.inf']:
        path = payload/name
        if path.is_symlink() or hashlib.sha256(path.read_bytes()).hexdigest() != manifest['files'][name]:
            raise RuntimeError('Wine input payload hash mismatch')
    (logs/'payload-manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    env = os.environ.copy()
    for key in list(env):
        if key.startswith(('Steam', 'STEAM_', 'SKINOS_')) or key in ['DYLD_INSERT_LIBRARIES', 'WINEDLLOVERRIDES']:
            env.pop(key)
    env.update(WINEPREFIX=str(prefix), WINEARCH='win64', WINELOADER=str(loader),
        WINESERVER=str(runner/'bin/wineserver-arm64'), CX_ROOT=str(runner),
        CX_HOME=str(Path.home()/'Library/Application Support/CrossOver'), WINEDEBUG='-all',
        WINEDLLPATH=str(runner/'lib/wine/x86_64-windows')+':'+str(runner/'lib/wine/aarch64-unix'),
        PATH=str(runner/'bin')+':'+env['PATH'])
    for kind in ['CACHE','CONFIG','DATA']:
        env['FEX_APP_'+kind+'_LOCATION'] = str(prefix.parent/'fex-emu')
    def windows(path): return 'Z:'+str(path).replace('/','\\')
    feeder_path = windows(payload/'arm64/feeder.exe')
    def run(name, arguments, checked=True):
        with open(logs/(name+'.stdout.log'),'wb') as out, open(logs/(name+'.stderr.log'),'wb') as err:
            process = subprocess.run([str(loader),feeder_path]+arguments, env=env, stdout=out, stderr=err, timeout=30)
        rows=[]
        for line in (logs/(name+'.stdout.log')).read_text(errors='replace').splitlines():
            try: rows.append(json.loads(line))
            except json.JSONDecodeError: pass
        if checked and process.returncode: raise RuntimeError(name+' failed; see owned diagnostic log')
        return process.returncode, rows
    def status(name):
        _, rows = run(name, ['--status'])
        return next(row for row in rows if row.get('event')=='wine_input_driver_status')
    initial = status('initial-status')
    if not (initial.get('service_found') is False and initial.get('service_query_error') == 1060 and initial.get('cm_locate_result') == 13):
        raise RuntimeError('Input bus absence was not verified; refusing to take over another session')
    consumer = None
    checker = None
    forced_checker = False
    proof_path = logs/'steam-input-proof.json'
    attempted = False
    cleaning = False
    summary = dict(appid=args.appid, software_fixture=True, physical_device_verified=False,
        activity_requested=args.activity, passed=False)
    monitor = OriginalSteamMonitor()
    def interrupted(signum, _frame):
        if not cleaning: raise RuntimeError('Diagnostic interrupted by signal '+str(signum))
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        consumer = fixture.Child([str(loader), windows(payload/'arm64/consumer.exe')], env, logs, 'keepalive')
        consumer.wait('transport_consumer_ready')
        attempted = True
        run('register',['--register', windows(payload/'arm64/skinosinput.inf')])
        run('activate',['--activate'])
        command = ['python3',str(ROOT/'scripts/check-input.py'),'--core',str(app/'Contents/MacOS/skinos-core'),
            '--appid',str(args.appid),'--neutral','--generic-support','--focus','--stock-config',
            '--wine-output','--trace-hid','--require-controller','--require-xinput', '--output', str(proof_path)]
        if args.activity: command += ['--activity','--require-activity']
        # The core selects/authenticates its own runtime normally. No Wine or
        # manual SDK environment is imposed on the core's structured launch.
        checker = subprocess.Popen(command, start_new_session=True)
        checker.wait(timeout=180)
        summary['diagnostic_exit_code'] = checker.returncode
        summary['passed'] = checker.returncode == 0
    except (Exception, KeyboardInterrupt) as error:
        summary['error'] = str(error) or type(error).__name__
        summary['passed'] = False
    finally:
        cleaning = True
        cleanup = {}
        try:
            if checker is not None:
                forced_checker = stop_checker(checker)
                summary['diagnostic_exit_code'] = checker.returncode
            confirmed = checker is None or (not forced_checker and shutdown_proof(proof_path, checker.pid, args.appid))
            cleanup['core_shutdown_confirmed'] = confirmed
            cleanup['forced_checker_shutdown'] = forced_checker
            if attempted and confirmed:
                code, _ = run('uninstall',['--uninstall'],checked=False)
                cleanup['uninstall_exit'] = code
                for attempt in range(10):
                    state = status('cleanup-'+str(attempt))
                    removed = state.get('service_found') is False and state.get('service_query_error') == 1060 and state.get('cm_locate_result') == 13
                    cleanup['removed'] = removed
                    if removed: break
                    time.sleep(0.25)
            elif attempted:
                cleanup['skipped_reason'] = 'Producer shutdown was not confirmed; bus retained for explicit cleanup'
                summary['passed'] = False
        except Exception as error:
            cleanup['error'] = str(error)
            summary['passed'] = False
        finally:
            if consumer:
                try: cleanup['keepalive_exit'] = consumer.close()
                except Exception as error:
                    cleanup['keepalive_error'] = str(error)
                    summary['passed'] = False
        summary['cleanup'] = cleanup
        summary.update(monitor.close())
        if summary['original_gui_seen'] or not summary['original_gui_observation_complete']:
            summary['passed'] = False
        if attempted and (not cleanup.get('removed') or cleanup.get('uninstall_exit') or cleanup.get('keepalive_exit')):
            summary['passed'] = False
        (logs/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary))
    if not summary['passed']: raise SystemExit(1)


if __name__ == '__main__': main()
