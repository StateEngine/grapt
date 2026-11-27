"""Offline product-stderr retention, with no native product or provider calls."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import run
from capture import retain_server_log

TAIL = ('GRAPT_PROVIDER_DISPATCH tail_stage=SqlCommit class=Content '
        'session=ses_fixture attempt=pa_fixture start_ms=100 end_ms=120 '
        'clock_order=ordered\n')


class ServerLogTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.profile = self.root/'private/service'
        self.logs = self.profile/'logs'
        self.logs.mkdir(parents=True)
        self.source = self.logs/'server.log'
        self.target = self.root/'server.log'

    def test_absent_and_empty_are_different_evidence(self):
        self.assertEqual(retain_server_log(self.profile, self.target, 100), {'state': 'absent'})
        self.assertFalse(self.target.exists())
        self.source.write_bytes(b'')
        result = retain_server_log(self.profile, self.target, 100)
        self.assertEqual(result, {'state': 'retained', 'path': 'server.log', 'bytes': 0})
        self.assertEqual(self.target.read_bytes(), b'')

    def test_copy_preserves_tail_and_raw_bytes_while_masking_secrets(self):
        secret = b'offline-selected-secret'
        escaped = b'{"value":"sk\\u002dant-usr-offline-secret"}\n'
        self.source.write_bytes(TAIL.encode() + secret + b'\xff\n' + escaped)
        result = retain_server_log(self.profile, self.target, 1024, (secret,))
        retained = self.target.read_bytes()
        self.assertIn(TAIL.encode(), retained)
        self.assertIn(b'\xff', retained)
        self.assertNotIn(secret, retained)
        self.assertNotIn(b'offline-secret', retained)
        self.assertEqual(result['bytes'], len(retained))
        self.assertEqual(self.target.stat().st_mode & 0o777, 0o600)

    def test_oversized_or_expanded_log_refuses_before_retaining(self):
        self.source.write_bytes(b'x'*101)
        with self.assertRaisesRegex(RuntimeError, 'bounded regular file'):
            retain_server_log(self.profile, self.target, 100)
        self.assertFalse(self.target.exists())
        self.source.write_bytes(b'x'*11)
        with self.assertRaisesRegex(RuntimeError, 'Redacted server log exceeds'):
            retain_server_log(self.profile, self.target, 100, (b'x',))
        self.assertFalse(self.target.exists())

    def test_links_and_fifo_are_refused_without_reading_foreign_data(self):
        other = self.root/'foreign'
        other.write_bytes(b'foreign-data')
        self.source.symlink_to(other)
        with self.assertRaises(OSError):
            retain_server_log(self.profile, self.target, 100)
        self.source.unlink()
        os.mkfifo(self.source)
        with self.assertRaisesRegex(RuntimeError, 'bounded regular file'):
            retain_server_log(self.profile, self.target, 100)
        self.source.unlink()
        self.logs.rmdir()
        self.logs.symlink_to(other.parent, target_is_directory=True)
        with self.assertRaisesRegex(RuntimeError, 'real directory'):
            retain_server_log(self.profile, self.target, 100)
        self.assertFalse(self.target.exists())
        self.assertEqual(other.read_bytes(), b'foreign-data')

    def fixture_row(self, code, *, paid=False):
        task = {'id': 'probe', 'category': 'smoke', 'held_out': False,
                'files': {'README.md': 'fixture\n'}, 'prompt': 'Offline fixture only.',
                'hidden_tests': 'import unittest\nfrom pathlib import Path\n'
                                'class T(unittest.TestCase):\n'
                                ' def test_unchanged(self): '
                                'self.assertEqual(Path("README.md").read_text(),"fixture\\n")\n'}
        adapter = {'model': 'none', 'auth': 'anthropic-api' if paid else 'none',
                   'harness': 'grapt', 'argv': [sys.executable, '-c', code, '{service_root}']}
        replacements = {}
        if paid:
            key = self.root/'offline.key'
            key.write_text('sk-ant-usr-offline-sentinel')
            key.chmod(0o600)
            replacements['anthropic_key'] = key
        # The Python fixture stands in for the native process at the runner's
        # boundary. It emits only a synthetic log, never a product/model request.
        with patch.object(run, 'agent_sandbox', side_effect=lambda argv, *args, **kwargs: argv), \
                patch.object(run.grapt_usage, 'collect', return_value={}), \
                patch.object(run.provider_timing, 'collect', return_value={}), \
                patch.object(run, 'retain_server_log', wraps=retain_server_log) as retain:
            result = run.attempt(task, 'secode-fixture', adapter, self.root/'evidence',
                                 10, replacements)
        self.assertEqual(retain.call_count, 1,
                         f'Server-log fixture failed before capture: {result}')
        return result, retain.call_args.args[0], self.root/'evidence/secode-fixture/probe'

    def test_fixture_setup_refusal_reports_the_original_result(self):
        refusal = {'status': 'setup_failed', 'success': False,
                   'error': 'Synthetic private allocation refusal'}
        with patch.object(run, 'attempt', return_value=refusal) as attempt:
            with self.assertRaisesRegex(AssertionError, 'setup_failed.*Synthetic private allocation refusal'):
                self.fixture_row('Never executed')
        attempt.assert_called_once()

    def test_offline_row_retains_tail_before_private_profile_disposal(self):
        code = ('from pathlib import Path; import sys; '
                'logs=Path(sys.argv[1])/"logs"; logs.mkdir(parents=True); '
                f'(logs/"server.log").write_text({TAIL!r})')
        result, profile, folder = self.fixture_row(code)
        self.assertTrue(result['success'], result)
        self.assertFalse(profile.parent.exists())
        self.assertEqual((folder/'server.log').read_text(), TAIL)
        self.assertEqual(result['product_server_log']['state'], 'retained')
        saved = json.loads((folder/'result.json').read_text())
        self.assertEqual(saved['product_server_log'], result['product_server_log'])

    def test_failed_paid_row_retains_masked_log_and_still_disposes_profile(self):
        code = ('from pathlib import Path; import os,sys; '
                'logs=Path(sys.argv[1])/"logs"; logs.mkdir(parents=True); '
                f'(logs/"server.log").write_text({TAIL!r}+os.environ["ANTHROPIC_API_KEY"]); '
                'raise SystemExit(1)')
        result, profile, folder = self.fixture_row(code, paid=True)
        self.assertEqual(result['status'], 'failed')
        self.assertFalse(result['success'])
        self.assertTrue(result['private_profile_removed'])
        self.assertFalse(profile.parent.exists())
        retained = (folder/'server.log').read_text()
        self.assertIn(TAIL, retained)
        self.assertNotIn('sk-ant-', retained)
        self.assertIn('[REDACTED]', retained)

    def test_capture_failure_is_evidence_failed_and_private_cleanup_still_runs(self):
        code = ('from pathlib import Path; import sys; '
                'logs=Path(sys.argv[1])/"logs"; logs.mkdir(parents=True); '
                '(logs/"server.log").mkdir()')
        result, profile, folder = self.fixture_row(code, paid=True)
        self.assertEqual(result['status'], 'evidence_failed')
        self.assertFalse(result['success'])
        self.assertEqual(result['product_server_log']['state'], 'failed')
        self.assertTrue(result['private_profile_removed'])
        self.assertFalse(profile.parent.exists())
        self.assertFalse((folder/'server.log').exists())


if __name__ == '__main__':
    unittest.main()
