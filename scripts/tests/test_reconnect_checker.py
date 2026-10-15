"""Pure event-replay tests; importing the checker never starts a backend."""
import argparse
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('reconnect_checker', ROOT / 'scripts/check-reconnect.py')
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)
ACCOUNT = '76561198000000000'


class ReconnectCheckerTests(unittest.TestCase):
    def check(self):
        check = checker.ReconnectCheck(argparse.Namespace(appid=629520, no_game=False, timeout=180))
        check.observe(check.started, {'event': 'authenticated', 'payload': {'steamid': ACCOUNT, 'generation': 1}})
        check.disconnect_sent_at = check.started + 1
        with contextlib.redirect_stdout(io.StringIO()):
            check.observe(check.started + 2, {'event': 'status', 'payload': {'state': 'reconnecting', 'retry_in_seconds': 10, 'account': {'steamid': ACCOUNT}}})
        return check

    def test_early_retry_fails_exact_delay_check(self):
        check = self.check()
        with self.assertRaisesRegex(checker.CheckFailed, 'before the required'):
            check.observe(check.started + 11.9, {'event': 'status', 'payload': {'state': 'connecting', 'reconnecting': True}})

    def test_real_delay_and_identity_preservation_pass_without_serializing_id(self):
        check = self.check()
        check.observe(check.started + 12.05, {'event': 'status', 'payload': {'state': 'connecting', 'reconnecting': True}})
        check.observe(check.started + 13, {'event': 'authenticated', 'payload': {'steamid': ACCOUNT, 'generation': 1, 'refresh_token': 'must-not-be-recorded'}})
        self.assertTrue(check.evidence['identity_preserved'])
        self.assertTrue(check.evidence['account_epoch_preserved'])
        output = json.dumps(check.evidence)
        self.assertNotIn(ACCOUNT, output)
        self.assertNotIn('must-not-be-recorded', output)
        self.assertIsNone(check.process)

    def test_game_exit_before_requested_stop_is_failure(self):
        check = self.check()
        with self.assertRaisesRegex(checker.CheckFailed, 'Game stopped'):
            check.observe(check.started + 5, {'event': 'launch_status', 'payload': {'appid': 629520, 'state': 'exited'}})

    def test_logout_guard_rejects_unexpected_login(self):
        check = self.check()
        check.logout_guard = True
        with self.assertRaisesRegex(checker.CheckFailed, 'Explicit logout'):
            check.observe(check.started + 20, {'event': 'status', 'payload': {'state': 'connecting'}})

    def test_correlated_launch_error_marks_operation_drained_for_cleanup(self):
        check = self.check()
        check.observe(check.started + 5, {'event': 'error', 'id': 'launch-game', 'payload': {'code': 'operation_failed', 'message': 'never written'}}, tolerate_errors=True)
        self.assertTrue(check.game_drained)
        self.assertNotIn('never written', json.dumps(check.evidence))


if __name__ == '__main__':
    unittest.main()
