"""Setup-refusal records using fake boundaries only; no native or sandbox launch."""
import errno
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import run


class AttemptSetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.task = {'id': 'setup', 'category': 'smoke', 'held_out': False,
                     'files': {}, 'prompt': 'Synthetic setup refusal.', 'hidden_tests': ''}

    def snapshot(self, task, directory):
        directory.mkdir(parents=True)
        return 'synthetic-baseline'

    def evidence_process(self, argv, directory, prompt, log, timeout, env, **kwargs):
        self.assertEqual(argv[0], 'git')
        log.write_text('')
        return 'completed', 0, 0

    def attempt(self, task, adapter, *, snapshot=None, environment=None, allocation=None):
        with patch.object(Path, 'home', return_value=self.root), \
                patch.object(run, 'create_snapshot', side_effect=snapshot or self.snapshot), \
                patch.object(run, 'runtime_environment', side_effect=environment or (lambda: {})), \
                patch.object(run, 'bounded_process', side_effect=self.evidence_process) as process, \
                patch.object(run, 'agent_sandbox') as sandbox, \
                patch.object(run, 'hidden_checks') as checks, \
                patch.object(run, 'retain_server_log', wraps=run.retain_server_log) as retain:
            if allocation:
                with patch.object(run.tempfile, 'TemporaryDirectory', side_effect=allocation):
                    result = run.attempt(task, 'fixture', adapter, self.root/'rows', 1, {})
            else:
                result = run.attempt(task, 'fixture', adapter, self.root/'rows', 1, {})
        sandbox.assert_not_called()
        checks.assert_not_called()
        self.assertEqual(result['status'], 'setup_failed')
        self.assertFalse(result['success'])
        self.assertEqual(result['native_harness'], adapter['harness'])
        folder = self.root/'rows/fixture/setup'
        self.assertEqual(json.loads((folder/'result.json').read_text()), result)
        self.assertEqual((folder/'changes.diff').read_text(), '')
        return result, process, retain

    def test_snapshot_refusal_stays_a_record_without_private_allocation(self):
        adapter = {'model': 'none', 'auth': 'none', 'harness': 'grapt', 'argv': ['/never-launch']}
        result, process, retain = self.attempt(
            self.task, adapter, snapshot=OSError('Synthetic snapshot refusal'))
        self.assertEqual(result['error'], 'Synthetic snapshot refusal')
        process.assert_not_called()
        retain.assert_not_called()
        self.assertFalse((self.root/'.cache').exists())

    def test_private_parent_refusal_preserves_automatic_codex_failure(self):
        parent = self.root/'.cache/grapt'
        parent.mkdir(parents=True)
        (parent/'bench-private').write_text('Synthetic non-directory')
        task = dict(self.task, automatic_compaction={})
        adapter = {'model': 'none', 'auth': 'none', 'harness': 'codex', 'argv': ['/never-launch']}
        result, process, retain = self.attempt(task, adapter)
        self.assertIn('File exists', result['error'])
        self.assertEqual(process.call_count, 2)
        retain.assert_not_called()
        self.assertNotIn('product_server_log', result)

    def test_private_allocation_refusal_preserves_automatic_grapt_failure(self):
        task = dict(self.task, automatic_compaction={})
        adapter = {'model': 'none', 'auth': 'none', 'harness': 'grapt', 'argv': ['/never-launch']}
        result, process, retain = self.attempt(
            task, adapter, allocation=OSError(errno.EROFS, 'Synthetic private allocation refusal'))
        self.assertIn('Synthetic private allocation refusal', result['error'])
        self.assertEqual(process.call_count, 2)
        retain.assert_not_called()
        self.assertNotIn('product_server_log', result)
        self.assertEqual(list((self.root/'.cache/grapt/bench-private').iterdir()), [])

    def test_environment_refusal_disposes_profile_and_preserves_automatic_failure(self):
        task = dict(self.task, automatic_compaction={})
        adapter = {'model': 'none', 'auth': 'none', 'harness': 'grapt', 'argv': ['/never-launch']}
        result, process, retain = self.attempt(
            task, adapter, environment=OSError('Synthetic environment refusal'))
        self.assertEqual(result['error'], 'Synthetic environment refusal')
        self.assertEqual(process.call_count, 2)
        retain.assert_called_once()
        self.assertEqual(result['product_server_log'], {'state': 'absent'})
        self.assertFalse(retain.call_args.args[0].parent.exists())
        self.assertEqual(list((self.root/'.cache/grapt/bench-private').iterdir()), [])


if __name__ == '__main__':
    unittest.main()
