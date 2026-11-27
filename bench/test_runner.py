"""No-model checks of evaluation, evidence capture and report honesty."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import report
import run
from budget import BudgetStop


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def task(self):
        return {"id": "probe", "category": "bug", "held_out": False,
                "files": {"answer.py": "value = 0\n"}, "prompt": "Set value to one.",
                "hidden_tests": "import unittest\nimport answer\nclass Check(unittest.TestCase):\n def test_value(self): self.assertEqual(answer.value,1)\n"}

    def test_deferred_cohort_keeps_denominator_and_never_probes_or_launches(self):
        output = self.root/'weekly'
        argv = ['run', '--output', str(output), '--execute', '--authorization-ref', 'fixture',
                '--task', 'B01', '--harness', 'codex', '--harness', 'claude',
                '--codex-binary', sys.executable, '--defer-harness', 'claude=Subscription window held']
        row = {'harness': 'codex', 'task': 'B01', 'status': 'completed', 'success': True}
        with patch.object(sys, 'argv', argv), patch.object(run, 'aligned'), \
                patch.object(run, 'bounded_process', return_value=('completed', 0, 0)) as process, \
                patch.object(run, 'attempt', return_value=row) as attempt:
            self.assertEqual(run.main(), 1)
        self.assertEqual(attempt.call_count, 1)
        self.assertEqual(attempt.call_args.args[1], 'codex')
        self.assertTrue(all(Path(call.args[0][0]).resolve() == Path(sys.executable).resolve()
                            for call in process.call_args_list))
        saved = json.loads((output/'results.json').read_text())
        self.assertEqual(saved['planned_harnesses'], ['codex', 'claude'])
        self.assertEqual(saved['deferred_harnesses'], {'claude': 'Subscription window held'})
        self.assertFalse(saved['measurement_complete'])
        self.assertEqual(saved['summary']['claude']['status_counts'], {'missing': 1})
        self.assertNotIn('claude', saved['harness_versions'])
        self.assertFalse((output/'claude').exists())

    def test_all_deferred_rows_make_no_account_read_version_probe_or_attempt(self):
        output = self.root/'all-held'
        argv = ['run', '--output', str(output), '--execute', '--authorization-ref', 'fixture',
                '--harness', 'claude', '--defer-harness', 'claude=Not eligible']
        with patch.object(sys, 'argv', argv), patch.object(run, 'aligned') as aligned, \
                patch.object(run, 'bounded_process') as process, patch.object(run, 'attempt') as attempt:
            self.assertEqual(run.main(), 1)
        aligned.assert_not_called()
        process.assert_not_called()
        attempt.assert_not_called()
        saved = json.loads((output/'results.json').read_text())
        self.assertEqual(saved['attempts'], [])
        self.assertEqual(saved['summary']['claude']['planned'], 12)

    def test_deferral_requires_one_selected_harness_and_bounded_nonempty_reason(self):
        for selections in [['missing=hold'], ['codex'], ['codex= '], ['codex='+'x'*513],
                           ['codex=first', 'codex=second']]:
            with self.assertRaises(ValueError):
                run.deferred_harnesses(selections, {'codex': {}})

    def test_fake_agent_pass_captures_committed_and_new_code(self):
        code = ("from pathlib import Path; import subprocess; "
                "Path('answer.py').write_text('value = 1\\n'); "
                "subprocess.run(['git','-c','user.name=Probe','-c','user.email=probe@localhost','commit','-qam','agent commit'],check=True); "
                "Path('new.py').write_text('helper = True\\n')")
        adapter = {"model": "none", "auth": "none", "argv": [sys.executable, "-c", code]}
        result = run.attempt(self.task(), "fake", adapter, self.root / "results", 10, {})
        self.assertTrue(result["success"], result)
        diff = (self.root / "results/fake/probe/changes.diff").read_text()
        self.assertIn("+value = 1", diff)
        self.assertIn("+helper = True", diff)

    def test_ignored_helper_is_captured_even_when_evaluation_passes(self):
        code = ("from pathlib import Path; Path('.gitignore').write_text('helper.py\\n'); "
                "Path('helper.py').write_text('value = 1\\n'); "
                "Path('answer.py').write_text('from helper import value\\n')")
        adapter = {'model': 'none', 'auth': 'none', 'argv': [sys.executable, '-c', code]}
        result = run.attempt(self.task(), 'fake', adapter, self.root/'results', 10, {})
        self.assertTrue(result['success'], result)
        self.assertIn('helper.py', result['untracked_files'])
        self.assertIn('+value = 1', (self.root/'results/fake/probe/changes.diff').read_text())

    def test_added_diff_headers_cannot_exceed_the_evidence_bound(self):
        code = ("from pathlib import Path; Path('answer.py').write_text('value = 1\\n'); "
                "Path('new.py').write_text('x=1\\n'*100)")
        adapter = {'model': 'none', 'auth': 'none', 'argv': [sys.executable, '-c', code]}
        with patch.object(run, 'MAX_LOG', 600):
            result = run.attempt(self.task(), 'fake', adapter, self.root/'results', 10, {})
        self.assertEqual(result['status'], 'evidence_failed')
        self.assertFalse(result['success'])
        self.assertLessEqual((self.root/'results/fake/probe/changes.diff').stat().st_size, 600)

    def test_unrelated_host_credentials_and_config_are_absent_from_agent_environment(self):
        sentinels = ['AZURE_OPENAI_API_KEY', 'AWS_SECRET_ACCESS_KEY', 'SSH_AUTH_SOCK',
                     'OPENCODE_CONFIG_CONTENT', 'OPENAI_BASE_URL', 'HTTP_PROXY']
        code = (f"import os; from pathlib import Path; assert not any(k in os.environ for k in {sentinels!r}); "
                "Path('answer.py').write_text('value = 1\\n')")
        adapter = {'model': 'none', 'auth': 'none', 'argv': [sys.executable, '-c', code]}
        with patch.dict(os.environ, {key: 'synthetic-private-sentinel' for key in sentinels}):
            result = run.attempt(self.task(), 'fake', adapter, self.root/'results', 10, {})
        self.assertTrue(result['success'], result)

    def test_grapt_profile_credentials_are_read_only_in_the_real_namespace(self):
        snapshot, service, output = self.root/'repo', self.root/'service', self.root/'output'
        for directory in [snapshot, service, output]: directory.mkdir()
        for name in ['provider.credential', 'openrouter.credential']:
            (service/name).write_text('synthetic-access-only')
        code = f'''from pathlib import Path
import errno
root = Path({str(service)!r})
for name in ['provider.credential', 'openrouter.credential']:
    try:
        (root/name).write_text('rotated')
    except OSError as error:
        assert error.errno == errno.EROFS
    else:
        raise RuntimeError('Credential write succeeded')
'''
        environment = run.runtime_environment()
        command = run.agent_sandbox([sys.executable, '-c', code], 'grapt', snapshot, output,
                                    environment, service_root=str(service))
        status, exit_code, _ = run.bounded_process(command, snapshot, '', output/'probe.log', 10, environment)
        self.assertEqual((status, exit_code), ('completed', 0), (output/'probe.log').read_text())
        self.assertTrue(all((service/name).read_text() == 'synthetic-access-only'
                            for name in ['provider.credential', 'openrouter.credential']))

    def test_execution_binding_refusal_precedes_native_profile_initialization(self):
        adapter = {'model': 'none', 'auth': 'chatgpt-oauth', 'harness': 'grapt',
                   'profile_required': True, 'cohort': 'luna', 'argv': ['/missing/native']}
        with patch.object(run, 'execution_inputs', side_effect=ValueError('Synthetic account mismatch')), \
                patch.object(run, 'bounded_process', wraps=run.bounded_process) as process:
            result = run.attempt(self.task(), 'grapt', adapter, self.root/'results', 10,
                                 {'account_source': self.root/'inputs', 'product_binary': '/missing/product'})
        self.assertEqual(result['status'], 'setup_failed')
        self.assertFalse(any('init-profile' in call.args[0] for call in process.call_args_list))

    def test_evaluator_zero_exit_without_evidence_fails(self):
        task = self.task()
        task["hidden_tests"] = "raise SystemExit(0)\n"
        repo = self.root / "repo"
        run.create_snapshot(task, repo)
        status, _ = run.hidden_checks(task, repo, self.root / "tests.log")
        self.assertEqual(status, "failed")

    def test_agent_cannot_read_unrelated_scratch_state(self):
        for parent in [Path(tempfile.gettempdir()), Path.cwd()]:
            with self.subTest(parent=parent), tempfile.TemporaryDirectory(dir=parent) as temporary:
                root = Path(temporary)
                state = root / "host-state.txt"
                state.write_text("private sentinel")
                code = (f"from pathlib import Path; assert not Path({str(state)!r}).exists(); "
                        "Path('answer.py').write_text('value = 1\\n')")
                adapter = {"model": "none", "auth": "none", "argv": [sys.executable, "-c", code]}
                result = run.attempt(self.task(), "fake", adapter, root / "results", 10, {})
                self.assertTrue(result["success"], result)
                self.assertEqual(state.read_text(), "private sentinel")

    def test_opencode_api_credentials_cannot_replace_chatgpt(self):
        account = self.root / "data/opencode/auth.json"
        account.parent.mkdir(parents=True)
        account.write_text(json.dumps({"openai": {"type": "api", "key": "fake-test-key"}}))
        env = dict(os.environ, XDG_DATA_HOME=str(self.root / "data"))
        with self.assertRaisesRegex(RuntimeError, "ChatGPT sign-in"):
            run.agent_sandbox([sys.executable, "--version"], "opencode", self.root,
                              self.root / "results", env)

    def test_evaluator_cannot_read_an_unrelated_host_file(self):
        secret = self.root / "host-only.txt"
        secret.write_text("sentinel")
        task = self.task()
        task["hidden_tests"] = ("import unittest\nfrom pathlib import Path\nclass Check(unittest.TestCase):\n"
                                f" def test_isolated(self): self.assertFalse(Path({str(secret)!r}).exists())\n")
        repo = self.root / "repo"
        run.create_snapshot(task, repo)
        status, code = run.hidden_checks(task, repo, self.root / "tests.log")
        self.assertEqual((status, code), ("completed", 0))

    def test_setup_failure_remains_a_record(self):
        adapter = {"model": "none", "auth": "none", "argv": ["/missing/benchmark-cli"]}
        result = run.attempt(self.task(), "missing", adapter, self.root / "results", 10, {})
        self.assertEqual(result["status"], "setup_failed")
        self.assertFalse(result["success"])
        self.assertTrue((self.root / "results/missing/probe/result.json").exists())

    def test_native_grapt_failure_retains_private_diagnostics(self):
        import shutil
        adapter = {'model': 'none', 'auth': 'none', 'harness': 'grapt',
                   'argv': [sys.executable, '-c', 'raise SystemExit(1)']}
        result = run.attempt(self.task(), 'grapt', adapter, self.root/'results', 10, {})
        self.assertEqual(result['status'], 'failed')
        retained = Path(result['private_diagnostic'])
        self.addCleanup(shutil.rmtree, retained)
        self.assertTrue(retained.is_dir())
        self.assertTrue(retained.name.endswith('-diagnostic'))

    def test_active_budget_refusal_stops_the_owned_process(self):
        pid_file = self.root/'owned-pid'
        code = ("import os,time; from pathlib import Path; "
                f"Path({str(pid_file)!r}).write_text(str(os.getpid())); time.sleep(60)")
        def refuse():
            raise BudgetStop('Unverifiable shared-key usage')
        with self.assertRaisesRegex(BudgetStop, 'Unverifiable'):
            run.bounded_process([sys.executable, '-c', code], self.root, '',
                                self.root/'process.log', 15, {'PATH': os.defpath}, monitor=refuse)
        pid = int(pid_file.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_receive_times_preserve_native_events_and_stderr_bytes(self):
        from datetime import datetime, timezone
        import base64
        log = self.root/'agent.jsonl'
        event = {'type': 'turn.completed', 'usage': {
            'input_tokens': 10, 'cached_input_tokens': 2, 'output_tokens': 4}}
        code = f"import os; os.write(1, {json.dumps(event).encode()!r}+b'\\n'); os.write(2,b'raw\\xff')"
        before = datetime.now(timezone.utc)
        status, exit_code, _ = run.bounded_process([sys.executable, '-c', code],
            self.root, '', log, 10, {'PATH': os.defpath}, receive_timestamps=True)
        after = datetime.now(timezone.utc)
        self.assertEqual((status, exit_code), ('completed', 0))
        rows = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(rows[0]['event'], event)
        self.assertEqual(base64.b64decode(rows[1]['raw_base64']), b'raw\xff')
        self.assertTrue(all(before <= datetime.fromisoformat(row['received_at']) <= after for row in rows))
        self.assertEqual(run.telemetry(log, 'codex')['usage'], event['usage'])

    def test_receive_capture_refuses_overflow_and_reaps_the_child(self):
        pid_file = self.root/'capture-pid'
        code = ("import os,time; from pathlib import Path; "
                f"Path({str(pid_file)!r}).write_text(str(os.getpid())); "
                "os.write(1,b'x'*4096); time.sleep(60)")
        with patch.object(run, 'MAX_LOG', 512), self.assertRaisesRegex(RuntimeError, 'capture failed'):
            run.bounded_process([sys.executable, '-c', code], self.root, '',
                self.root/'agent.jsonl', 0.5, {'PATH': os.defpath}, receive_timestamps=True)
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pid_file.read_text()), 0)
        self.assertLessEqual((self.root/'agent.jsonl').stat().st_size, 512)

    def test_opencode_input_includes_cache_without_inventing_model_or_spend(self):
        log = self.root / 'opencode.jsonl'
        log.write_text(json.dumps({'type': 'step_finish', 'part': {'tokens': {
            'input': 10, 'output': 5, 'cache': {'read': 20, 'write': 3}}, 'cost': 0}})+'\n')
        result = run.telemetry(log, 'opencode')
        self.assertEqual(result['usage'], {'input_tokens': 33, 'cached_input_tokens': 20, 'output_tokens': 5})
        self.assertEqual(result['cache_write_tokens'], 3)
        self.assertIsNone(result['actual_model'])
        self.assertIsNone(result['api_equivalent_cost_usd'])

    def test_missing_usage_is_not_zero(self):
        log = self.root / "usage.jsonl"
        log.write_text(json.dumps({"type": "turn.completed", "usage": {"input_tokens": 10}}) + "\n")
        result = run.telemetry(log, "codex")
        self.assertIsNone(result["usage"])
        self.assertIsNone(result["api_equivalent_cost_usd"])

    def test_native_grapt_and_claude_usage_keep_cache_classes(self):
        log = self.root/'usage.jsonl'
        cases = [
            ('grapt', {'type': 'benchmark.grapt.completed', 'usage': {
                'inputTokens': 10, 'outputTokens': 4, 'cachedReadTokens': 20, 'cachedWriteTokens': 3}}),
            ('claude', {'type': 'result', 'usage': {
                'input_tokens': 10, 'output_tokens': 4, 'cache_read_input_tokens': 20, 'cache_creation_input_tokens': 3}})]
        for harness, row in cases:
            with self.subTest(harness=harness):
                log.write_text(json.dumps(row)+'\n')
                self.assertEqual(run.telemetry(log, harness)['usage'], {
                    'input_tokens': 33, 'cached_input_tokens': 20, 'output_tokens': 4})
                row['usage'][next(iter(row['usage']))] = None
                log.write_text(json.dumps(row)+'\n')
                self.assertIsNone(run.telemetry(log, harness)['usage'])

    def test_report_join_preserves_failure_and_refuses_retry(self):
        failed = {'harness': 'fake', 'task': 'a', 'status': 'failed', 'success': False}
        passed = {'harness': 'fake', 'task': 'b', 'status': 'completed', 'success': True}
        first = {'corpus': 'same', 'planned_tasks': ['a', 'b'], 'planned_harnesses': ['fake'], 'attempts': [failed]}
        next_segment = dict(first, planned_tasks=['b'], attempts=[passed])
        self.assertEqual(report.combined([first, next_segment])[-1]['fake']['success_rate'], 0.5)
        with self.assertRaisesRegex(ValueError, 'Duplicate attempt'):
            report.combined([first, first])
        self.assertIn('Development harness benchmark', report.markdown([first, next_segment]))

    def test_missing_attempts_and_unknown_cost_remain_visible(self):
        result = report.summary([{"harness": "fake", "task": "a", "status": "completed", "success": True}],
                                ["fake"], ["a", "b", "c"])["fake"]
        self.assertEqual(result["success_rate"], 1/3)
        self.assertEqual(result["status_counts"]["missing"], 2)
        self.assertIsNone(result["api_equivalent_cost_usd_total"])
        self.assertIsNone(result["interventions"])
        self.assertIsNone(result["tokens"]["input_tokens"])


if __name__ == "__main__":
    unittest.main()
