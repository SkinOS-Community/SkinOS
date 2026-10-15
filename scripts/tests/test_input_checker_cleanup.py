"""Offline process-lifecycle tests: the only child is a tiny JSON mock core."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('wine_checker', ROOT/'scripts/check-steam-input-wine.py')
wrapper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wrapper)


class InputCleanupTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.folder = Path(self.temporary.name)

    def start(self, mode):
        core = self.folder/'core'
        core.write_text('#!'+sys.executable+'\n'+'''
import json, pathlib, sys
root = pathlib.Path(__file__).parent
def emit(event, payload=None, id=None):
    print(json.dumps(dict(event=event, payload=payload or {}, id=id)), flush=True)
emit('ready')
for line in sys.stdin:
    command=json.loads(line)['command']
    with (root/'commands').open('a') as out: out.write(command+'\\n')
    if command=='auto_login':
        emit('authenticated', {'steamid':'mock-account'})
        emit('library', {'cached':False,'games':[{'appid':648800}]})
    elif command=='diagnose_input':
        (root/'started').touch()
        mode=(root/'mode').read_text()
        if mode=='eof': break
        if mode=='complete':
            emit('steam_input_diagnostics', {'steamid':'mock-account','appid':648800,
                'summary':{'appid_verified':True,'appid':648800,'scan_completed':True}})
            emit('operation_complete', id='diagnose_input')
    elif command=='stop_game': emit('error', {'message':'mock stopped'}, id='diagnose_input')
    elif command=='quit': break
''')
        core.chmod(0o700)
        (self.folder/'mode').write_text(mode)
        output = self.folder/'proof.json'
        proc = subprocess.Popen([sys.executable, str(ROOT/'scripts/check-input.py'), '--core', str(core),
            '--timeout', '5', '--output', str(output)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: proc.poll() is None and (proc.kill(), proc.wait(timeout=3)))
        return proc, output

    def proof(self, process, path):
        stdout, stderr = process.communicate(timeout=8)
        self.assertTrue(path.is_file(), stdout+stderr)
        proof = json.loads(path.read_text())
        self.assertNotIn('mock-account', path.read_text())
        self.assertEqual(proof['checker_pid'], process.pid)
        return proof

    def test_success_requires_terminal_operation_and_clean_core_exit(self):
        process, path = self.start('complete')
        proof = self.proof(process, path)
        self.assertEqual(process.returncode, 0)
        self.assertTrue(proof['requirements_satisfied'])
        self.assertTrue(proof['operation_finished'])
        self.assertTrue(proof['core_shutdown_confirmed'])
        self.assertTrue(wrapper.shutdown_proof(path, process.pid, 648800))
        self.assertFalse(wrapper.shutdown_proof(path, process.pid+1, 648800))

    def test_sigterm_drains_owned_diagnostic_before_quitting(self):
        process, path = self.start('wait')
        deadline = time.monotonic()+3
        while not (self.folder/'started').exists():
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.01)
        process.send_signal(signal.SIGTERM)
        proof = self.proof(process, path)
        self.assertEqual(process.returncode, 1)
        self.assertTrue(proof['core_shutdown_confirmed'])
        self.assertFalse(proof['forced_core_shutdown'])
        self.assertFalse(proof['requirements_satisfied'])
        self.assertEqual((self.folder/'commands').read_text().splitlines()[-2:], ['stop_game', 'quit'])

    def test_exit_zero_without_diagnostic_terminal_does_not_confirm_cleanup(self):
        process, path = self.start('eof')
        proof = self.proof(process, path)
        self.assertEqual(process.returncode, 1)
        self.assertFalse(proof['core_shutdown_confirmed'])
        self.assertFalse(wrapper.shutdown_proof(path, process.pid, 648800))

    def test_wrapper_gives_checker_grace_and_marks_forced_group_shutdown(self):
        child = mock.Mock(pid=123456)
        child.poll.return_value = None
        child.wait.side_effect = [subprocess.TimeoutExpired('checker', 65), 0]
        with mock.patch.object(wrapper.os, 'killpg') as kill:
            self.assertTrue(wrapper.stop_checker(child))
        child.terminate.assert_called_once()
        self.assertEqual(child.wait.call_args_list, [mock.call(timeout=65), mock.call(timeout=5)])
        kill.assert_called_once_with(child.pid, signal.SIGKILL)

    def test_gui_observer_reads_only_comm_and_matches_original_steam(self):
        monitor = wrapper.OriginalSteamMonitor.__new__(wrapper.OriginalSteamMonitor)
        for comm in ['/Applications/Steam.app/Contents/MacOS/steam_osx',
                     '/Users/fixture/Library/Application Support/Steam/Steam.AppBundle/Steam/Contents/MacOS/steam_osx']:
            monitor.seen, monitor.samples, monitor.errors = False, 0, 0
            with mock.patch.object(wrapper.subprocess, 'run', return_value=mock.Mock(stdout='/owned/runtime-host\n'+comm+'\n')) as run:
                monitor.sample()
            self.assertTrue(monitor.seen)
            self.assertEqual(monitor.samples, 1)
            self.assertEqual(run.call_args.args, (['ps', '-axo', 'comm='],))


if __name__ == '__main__': unittest.main()
