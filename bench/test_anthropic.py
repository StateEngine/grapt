"""Direct-route privacy and accounting witnesses; no provider calls."""
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import accounts
import anthropic
import automatic
import automatic_settings
from budget import BudgetStop
import run


class DirectAnthropicTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.key = self.root/'key'
        self.key.write_text('sk-ant-usr-offline-sentinel')
        self.key.chmod(0o600)

    def direct_inputs(self):
        seed = self.root/'seed'; seed.mkdir()
        config = {'store': {'authority': 'initialize'}, 'product': {'build': 'benchmark-setup'},
                  'catalog': {'source': 'models.dev-chatgpt'},
                  'provider': {'id': 'openai', 'model': accounts.LUNA, 'base_model': accounts.LUNA,
                               'route': 'chatgpt-codex', 'account': 'benchmark-inert-primary'}}
        catalog = {'openai': {'models': {accounts.LUNA: {'limit': {'context': 272000}}}},
                   'openrouter': {'models': {accounts.HAIKU: {
                       'id': accounts.HAIKU, 'limit': {'context': 1000000, 'input': 1000000, 'output': 128000}}}}}
        for name, value in [('opencode.jsonc', config), ('model-catalog.json', catalog),
                            ('account-models.json', {'models': []})]:
            (seed/name).write_text(json.dumps(value))
        destination = self.root/'accounts/haiku-direct'
        destination.parent.mkdir()
        with patch.object(accounts, 'fetch', side_effect=AssertionError('No provider calls')):
            receipt = accounts.prepare_direct(seed, destination, 'claude-haiku-5-5')
        self.assertEqual(receipt['provider_calls'], 0)
        self.assertEqual(receipt['route'], 'anthropic-direct')
        self.assertNotIn('sk-ant-', json.dumps(receipt))
        return destination

    def test_default_route_and_explicit_modes_are_recorded_without_execution(self):
        for server_side, mode in [('off', 'client-summary'), ('on', 'provider-server')]:
            result = subprocess.run([sys.executable, automatic.__file__, '--harness', 'grapt-haiku',
                                     '--server-side', server_side], capture_output=True, text=True,
                                    timeout=10, check=True)
            plan = json.loads(result.stdout)
            self.assertEqual(plan['provider_calls'], 0)
            self.assertEqual(plan['adapters']['grapt-haiku']['route'], 'anthropic-direct')
            self.assertEqual(plan['compaction_modes']['grapt-haiku'], mode)
            self.assertNotIn('sk-ant-', result.stdout + result.stderr)
        rows = json.loads((run.ROOT/'bench/adapters.json').read_text())
        legacy = anthropic.adapters_for_route(rows, 'openrouter', 'claude-haiku-5-5')
        self.assertEqual(legacy['grapt-haiku']['model'], rows['grapt-haiku']['model'])
        self.assertEqual(legacy['grapt-haiku']['auth'], 'openrouter')

    def test_direct_launcher_uses_separate_monthly_ledger_without_openrouter_observation(self):
        output = self.root/'run'
        monthly = self.root/'anthropic-month.json'
        argv = ['automatic', '--execute', '--authorization-ref', 'synthetic-release',
                '--harness', 'grapt-haiku', '--output', str(output),
                '--account-source', str(self.root/'accounts'),
                '--grapt-compaction-ref', 'released-source', '--server-side', 'off',
                '--anthropic-key', str(self.key), '--anthropic-monthly-ledger', str(monthly)]

        def attempt(task, harness, adapter, root, timeout, replacements, budget=None):
            self.assertIsNone(budget)
            self.assertEqual(adapter['auth'], 'anthropic-api')
            self.assertEqual(adapter['route'], 'anthropic-direct')
            self.assertFalse(replacements['compaction_server_side'])
            self.assertEqual(replacements['anthropic_key'], self.key)
            self.assertNotIn('sk-ant-', json.dumps(adapter['argv']))
            result = {'task': task['id'], 'harness': harness, 'status': 'completed', 'success': True}
            folder = root/harness/task['id']; folder.mkdir(parents=True)
            return result

        with (patch.object(sys, 'argv', argv),
                patch.object(automatic, 'RowBudget', side_effect=AssertionError('No OpenRouter budget')),
                patch.object(automatic.shutil, 'which', return_value=sys.executable),
                patch.object(automatic, 'fingerprint', return_value={'path': sys.executable}),
                patch.object(run, 'attempt', side_effect=attempt)):
            self.assertEqual(automatic.main(), 0)
        record = json.loads((output/'results.json').read_text())
        self.assertEqual(record['compaction_modes']['grapt-haiku'], 'client-summary')
        self.assertEqual(record['attempts'][0]['anthropic_monthly_spend']['unreconciled_rows'], 1)
        self.assertEqual(json.loads(monthly.read_text())['rows'][0]['harness'], 'grapt-haiku')
        for path in output.rglob('*'):
            if path.is_file(): self.assertNotIn(b'sk-ant-', path.read_bytes())

    def test_native_profile_is_private_and_removed_at_teardown(self):
        source = self.direct_inputs()
        env = {}
        anthropic.environment(self.key, env)
        with tempfile.TemporaryDirectory(dir=self.root) as private:
            profile = accounts.execution_inputs(source, Path(private)/'profile', 'haiku-direct',
                                                 None, None, env['ANTHROPIC_API_KEY'])
            self.assertEqual(profile.stat().st_mode & 0o777, 0o700)
            self.assertEqual((profile/'anthropic.credential').stat().st_mode & 0o777, 0o600)
            self.assertEqual((profile/'anthropic.credential').read_text(), env['ANTHROPIC_API_KEY'])
            config = json.loads((profile/'opencode.jsonc').read_text())
            self.assertEqual(config['provider']['accounts'][0]['protocol_family'], 'anthropic-messages')
        self.assertFalse(profile.exists())
        self.assertTrue(all('sk-ant-' not in p.read_text() for p in source.iterdir()))

    def test_direct_preparation_accepts_native_inventory_size_without_relaxing_config_bounds(self):
        source = self.direct_inputs()
        seed = self.root/'seed'
        (seed/'account-models.json').write_text('{"models":[]}\n'+'\n'*80000)
        accounts.prepare_direct(seed, self.root/'large-preparation', 'claude-haiku-5-5')
        inventory = source/'account-models.json'
        inventory.write_text(inventory.read_text()+'\n'*80000)
        selected = accounts.execution_inputs(source, self.root/'selected', 'haiku-direct',
                                              None, None, self.key.read_text())
        self.assertEqual(json.loads((selected/'account-models.json').read_text()), {'models': []})
        inventory.write_text(' '*(16*1024*1024+1))
        with self.assertRaisesRegex(ValueError, 'bounded'):
            accounts.execution_inputs(source, self.root/'oversized', 'haiku-direct',
                                      None, None, self.key.read_text())
        self.assertFalse((self.root/'oversized').exists())
        (seed/'account-models.json').write_text(' '*(16*1024*1024+1))
        with self.assertRaisesRegex(ValueError, 'bounded'):
            accounts.prepare_direct(seed, self.root/'refused-preparation', 'claude-haiku-5-5')
        self.assertFalse((self.root/'refused-preparation').exists())

    def test_supported_direct_inventory_preserves32k_pressure(self):
        self.direct_inputs()
        adapter = {'model': 'anthropic/claude-haiku-5-5', 'auth': 'anthropic-api',
                   'route': 'anthropic-direct', 'cohort': 'haiku-direct'}
        for server_side in [False, True]:
            result = automatic_settings.configure({}, 'grapt', adapter,
                {'account_source': self.root/'accounts', 'grapt_compaction_ref': 'released-source',
                 'compaction_server_side': server_side, 'compaction_profile': 'c04-32k'})
            # configure uses the same selected cohort as its adapter.
            self.assertEqual(result['native_setting']['buffer'], 901616)
            self.assertEqual(result['native_setting']['server_side'], server_side)
            self.assertEqual(result['route'], 'anthropic-direct')

    def test_key_validation_refuses_oauth_wrong_modes_and_symlinks(self):
        for key in ['sk-ant-oat-offline', 'not-an-api-key']:
            self.key.write_text(key)
            with self.assertRaises(BudgetStop): anthropic.environment(self.key, {})
        self.key.write_text('sk-ant-usr-offline'); self.key.chmod(0o644)
        with self.assertRaises(BudgetStop): anthropic.environment(self.key, {})
        self.key.chmod(0o600)
        link = self.root/'link'; link.symlink_to(self.key)
        with self.assertRaises(BudgetStop): anthropic.environment(link, {})

    def test_capture_redacts_stdout_stderr_before_any_log_is_persisted(self):
        env = run.runtime_environment(); anthropic.environment(self.key, env)
        argv = [sys.executable, '-c',
                'import os,sys;print(os.environ["ANTHROPIC_API_KEY"]);'
                'sys.stderr.write(os.environ["ANTHROPIC_API_KEY"]+"\\n")']
        self.assertNotIn('sk-ant-', json.dumps(argv))
        for timestamp in [False, True]:
            log = self.root/str(timestamp)
            self.assertEqual(run.bounded_process(argv, self.root, '', log, 10, env,
                                                receive_timestamps=timestamp)[:2], ('completed', 0))
            self.assertNotIn('sk-ant-', log.read_text())
            if not timestamp: self.assertIn('[REDACTED]', log.read_text())

    def test_opencode_sdk_gets_only_selected_env_key_and_profile_is_removed(self):
        code = ('import os,sys;from pathlib import Path;'
                'assert os.environ["ANTHROPIC_API_KEY"].startswith("sk-ant-");'
                'print(os.environ["ANTHROPIC_API_KEY"],file=sys.stderr);'
                'Path("answer.py").write_text("value=1\\n")')
        adapter = {'model': 'anthropic/claude-haiku-5-5', 'auth': 'anthropic-api',
                   'route': 'anthropic-direct', 'harness': 'opencode', 'cohort': 'haiku-direct',
                   'argv': [sys.executable, '-c', code]}
        task = {'id': 'probe', 'category': 'bug', 'held_out': False,
                'files': {'answer.py': 'value=0\n'}, 'prompt': 'Set value to one',
                'hidden_tests': 'import unittest\nimport answer\nclass T(unittest.TestCase):\n'
                                ' def test_value(self): self.assertEqual(answer.value,1)\n'}
        # The code string uses a prefix test, never the credential literal.
        code = code.replace('.startswith("sk-ant-")', '.startswith("sk"+"-ant-")')
        adapter['argv'][-1] = code
        self.assertNotIn('sk-ant-', json.dumps(adapter['argv']))
        result = run.attempt(task, 'opencode-haiku', adapter, self.root/'evidence', 10,
                             {'anthropic_key': self.key})
        self.assertTrue(result['success'], result)
        self.assertTrue(result['private_profile_removed'])
        self.assertEqual(result['route'], 'anthropic-direct')
        self.assertEqual(result['compaction_mode'], 'native-client')
        for p in (self.root/'evidence').rglob('*'):
            if p.is_file(): self.assertNotIn(b'sk-ant-', p.read_bytes(), str(p))

    def test_monthly_ledger_keeps_unknown_billing_and_refuses_exhaustion_or_wrong_month(self):
        path = self.root/'monthly.json'
        ledger = anthropic.MonthlySpend(path, month='2026-10')
        self.assertEqual(ledger.check()['billed_total_usd'], '0')
        ledger.record({'task': 'C04', 'harness': 'grapt-haiku', 'status': 'failed'})
        data = json.loads(path.read_text())
        self.assertIsNone(data['rows'][0]['billed_usd'])
        self.assertEqual(ledger.check()['unreconciled_rows'], 1)
        with self.assertRaises(BudgetStop): anthropic.MonthlySpend(path, '2026-11').check()
        data['billed_total_usd'] = '200'; path.write_text(json.dumps(data))
        with self.assertRaisesRegex(BudgetStop, 'exhausted'): ledger.check()

    def test_failed_se_row_removes_native_credentials_without_retaining_diagnostics(self):
        self.direct_inputs()
        task = {'id': 'probe', 'category': 'bug', 'held_out': False,
                'files': {'answer.py': 'value=0\n'}, 'prompt': 'Set value to one',
                'hidden_tests': 'import unittest\nclass T(unittest.TestCase):\n'
                                ' def test_failure(self): self.assertFalse(True)\n'}
        adapter = {'model': 'anthropic/claude-haiku-5-5', 'auth': 'anthropic-api',
                   'route': 'anthropic-direct', 'harness': 'grapt', 'cohort': 'haiku-direct',
                   'profile_required': True, 'argv': [sys.executable, '-c', 'raise SystemExit(1)']}
        original = run.bounded_process
        native_profiles = []

        def process(argv, *args, **kwargs):
            if 'init-profile' in argv:
                service = Path(argv[argv.index('--service')+1])
                source = Path(argv[argv.index('--from')+1])
                shutil.copytree(source, service)
                native_profiles.append(service)
                self.assertEqual((service/'anthropic.credential').read_text(), self.key.read_text())
                self.assertEqual((service/'anthropic.credential').stat().st_mode & 0o777, 0o600)
                self.assertEqual(service.stat().st_mode & 0o777, 0o700)
                return 'completed', 0, 0.0
            return original(argv, *args, **kwargs)

        with patch.object(run, 'bounded_process', side_effect=process), \
                patch.object(run.grapt_usage, 'collect', return_value={}), \
                patch.object(run.provider_timing, 'collect', return_value={}):
            result = run.attempt(task, 'grapt-haiku', adapter, self.root/'evidence', 10,
                {'anthropic_key': self.key, 'account_source': self.root/'accounts',
                 'product_binary': sys.executable, 'grapt_binary': sys.executable,
                 'grapt_adapter': Path(run.__file__), 'keep_failed_private': True,
                 'compaction_mode': 'client-summary'})
        self.assertFalse(result['success'])
        self.assertEqual(result['status'], 'failed')
        self.assertTrue(result['private_profile_removed'])
        self.assertNotIn('private_diagnostic', result)
        self.assertEqual(result['compaction_mode'], 'client-summary')
        self.assertEqual(len(native_profiles), 1)
        self.assertFalse(native_profiles[0].exists())
        for p in (self.root/'evidence').rglob('*'):
            if p.is_file(): self.assertNotIn(b'sk-ant-', p.read_bytes(), str(p))

    def test_agent_secret_in_evidence_is_scrubbed_and_counted(self):
        evidence = self.root/'evidence'; evidence.mkdir()
        (evidence/'source.py').write_text(self.key.read_text())
        self.assertTrue(anthropic.redact_evidence(evidence))
        self.assertNotIn('sk-ant-', (evidence/'source.py').read_text())
        self.assertFalse(anthropic.redact_evidence(evidence))

    def test_unicode_escaped_key_is_redacted_after_json_decoding(self):
        env = run.runtime_environment(); anthropic.environment(self.key, env)
        command = [sys.executable, '-c',
            'import os,json; body=json.dumps({os.environ["ANTHROPIC_API_KEY"]:'
            'os.environ["ANTHROPIC_API_KEY"]});print(body.replace("-",chr(92)+"u002d"))']
        for timestamp in [False, True]:
            log = self.root/('escaped-'+str(timestamp))
            self.assertEqual(run.bounded_process(command, self.root, '', log, 10, env,
                                                receive_timestamps=timestamp)[:2], ('completed', 0))
            body = json.loads(log.read_text())
            self.assertNotIn('sk-ant-', json.dumps(body))
            self.assertNotIn(self.key.read_text(), json.dumps(body))

    def test_secret_filename_cannot_survive_returned_record_or_downstream_report(self):
        task = {'id': 'filename', 'category': 'bug', 'held_out': False,
                'files': {'answer.py': 'value=0\n'}, 'prompt': 'Offline fixture',
                'hidden_tests': 'import unittest\nclass T(unittest.TestCase):\n'
                                ' def test_ok(self): self.assertTrue(True)\n'}
        adapter = {'model': 'anthropic/claude-haiku-5-5', 'auth': 'anthropic-api',
                   'route': 'anthropic-direct', 'harness': 'opencode', 'cohort': 'haiku-direct',
                   'argv': [sys.executable, '-c', 'import os;from pathlib import Path;'
                            'Path(os.environ["ANTHROPIC_API_KEY"]).write_text("ordinary source\\n")']}
        output = self.root/'evidence'
        result = run.attempt(task, 'opencode-haiku', adapter, output, 10, {'anthropic_key': self.key})
        self.assertEqual(result['status'], 'evidence_failed')
        self.assertFalse(result['success'])
        self.assertTrue(result['private_profile_removed'])
        (output/'aggregate.json').write_text(json.dumps({'attempts': [result]}))
        self.assertNotIn('sk-ant-', json.dumps(result))
        for path in output.rglob('*'):
            self.assertNotIn('sk-ant-', str(path))
            if path.is_file(): self.assertNotIn(b'sk-ant-', path.read_bytes())

    def test_fifo_and_symlink_are_removed_without_opening_them(self):
        evidence = self.root/'evidence'; evidence.mkdir()
        os.mkfifo(evidence/'pipe')
        outside = self.root/'outside'; outside.write_text(self.key.read_text())
        (evidence/'link').symlink_to(outside)
        # A native process deadline catches the former FIFO hang.
        subprocess.run([sys.executable, '-c',
            'import sys;sys.path.insert(0,sys.argv[1]);import anthropic;'
            'assert anthropic.redact_evidence(sys.argv[2])',
            str(Path(anthropic.__file__).parent), str(evidence)], timeout=2, check=True)
        self.assertFalse((evidence/'pipe').exists())
        self.assertFalse((evidence/'link').is_symlink())
        self.assertEqual(outside.read_text(), self.key.read_text())

    def test_unreadable_agent_directory_is_disposed_and_returns_sanitized_failure(self):
        task = {'id': 'restricted', 'category': 'bug', 'held_out': False,
                'files': {'answer.py': 'value=0\n'}, 'prompt': 'Offline fixture',
                'hidden_tests': 'import unittest\nclass T(unittest.TestCase):\n'
                                ' def test_ok(self): self.assertTrue(True)\n'}
        adapter = {'model': 'anthropic/claude-haiku-5-5', 'auth': 'anthropic-api',
                   'route': 'anthropic-direct', 'harness': 'opencode', 'cohort': 'haiku-direct',
                   'argv': [sys.executable, '-c', 'import os;from pathlib import Path;'
                            'p=Path("restricted");p.mkdir();'
                            '(p/"data").write_text(os.environ["ANTHROPIC_API_KEY"]);p.chmod(0)']}
        output = self.root/'evidence'
        sibling = output/'sibling'; sibling.mkdir(parents=True)
        (sibling/'keep').write_text('keep')
        row = output/'opencode-haiku/restricted'
        try:
            result = run.attempt(task, 'opencode-haiku', adapter, output, 10,
                                 {'anthropic_key': self.key})
            self.assertEqual(result['status'], 'evidence_failed')
            self.assertFalse(result['success'])
            self.assertTrue(result['private_profile_removed'])
            self.assertFalse((row/'repo').exists())
            self.assertEqual(json.loads((row/'result.json').read_text()), result)
            self.assertNotIn('sk-ant-', json.dumps(result))
            self.assertEqual((sibling/'keep').read_text(), 'keep')
        finally:
            restricted = row/'repo/restricted'
            if restricted.exists(): restricted.chmod(0o700)

    def test_secret_directory_and_decoded_json_are_redacted_without_touching_other_paths(self):
        evidence = self.root/'evidence'; evidence.mkdir()
        secret_dir = evidence/self.key.read_text(); secret_dir.mkdir()
        (secret_dir/'data.json').write_text(json.dumps({'value': self.key.read_text()}).replace('-', '\\u002d'))
        self.assertTrue(anthropic.redact_evidence(evidence))
        for path in evidence.rglob('*'):
            self.assertNotIn('sk-ant-', str(path))
            if path.is_file(): self.assertNotIn('sk-ant-', json.dumps(json.loads(path.read_text())))

    def test_disposal_repairs_owned_permissions_without_following_links_or_opening_fifo(self):
        outside = self.root/'outside'; outside.mkdir()
        (outside/'keep').write_text(self.key.read_text())
        outside.chmod(0o500)
        self.addCleanup(outside.chmod, 0o700)
        evidence = self.root/'evidence'; evidence.mkdir()
        restricted = evidence/'restricted'; restricted.mkdir()
        (restricted/'link').symlink_to(outside, target_is_directory=True)
        os.mkfifo(restricted/'pipe')
        os.link(outside/'keep', restricted/'hardlink')
        (restricted/'data').write_text(self.key.read_text())
        restricted.chmod(0)
        evidence.chmod(0o500)
        subprocess.run([sys.executable, '-c',
            'import sys;sys.path.insert(0,sys.argv[1]);import anthropic;'
            'anthropic.discard_evidence(sys.argv[2])',
            str(Path(anthropic.__file__).parent), str(evidence)], timeout=2, check=True)
        self.assertFalse(evidence.exists())
        self.assertEqual(outside.stat().st_mode & 0o777, 0o500)
        self.assertEqual((outside/'keep').read_text(), self.key.read_text())

    def test_depth_and_entry_exhaustion_quarantine_only_the_owned_row(self):
        task = {'id': 'bound', 'category': 'bug', 'held_out': False,
                'files': {'answer.py': 'value=0\n'}, 'prompt': 'Offline fixture',
                'hidden_tests': 'import unittest\nclass T(unittest.TestCase):\n'
                                ' def test_ok(self): self.assertTrue(True)\n'}
        base = {'model': 'anthropic/claude-haiku-5-5', 'auth': 'anthropic-api',
                'route': 'anthropic-direct', 'harness': 'opencode', 'cohort': 'haiku-direct'}
        codes = [
            'import os\nfrom pathlib import Path\np=Path("restricted");p.mkdir()\n'
            'for i in range(70):\n p=p/"d";p.mkdir()\n'
            '(p/"data").write_text(os.environ["ANTHROPIC_API_KEY"]);p.chmod(0)\n',
            'import os\nfrom pathlib import Path\n'
            'for i in range(10010):\n Path("entry-"+str(i)).write_text(os.environ["ANTHROPIC_API_KEY"])\n',
        ]
        quarantine = self.root/'private-quarantine'; quarantine.mkdir(mode=0o700)
        outside = self.root/'outside'; outside.mkdir()
        (outside/'keep').write_text('outside')
        for index, code in enumerate(codes):
            output = self.root/f'evidence-{index}'
            sibling = output/'sibling'; sibling.mkdir(parents=True)
            (sibling/'keep').write_text('sibling')
            adapter = dict(base, argv=[sys.executable, '-c', code])
            with patch.object(anthropic, 'quarantine_directory', return_value=quarantine), \
                    patch('sys.stderr') as diagnostic:
                result = run.attempt(task, 'opencode-haiku', adapter, output, 10,
                                     {'anthropic_key': self.key})
            row = output/'opencode-haiku/bound'
            self.assertEqual(result['status'], 'evidence_failed')
            self.assertFalse(result['success'])
            self.assertTrue(result['private_profile_removed'])
            self.assertFalse((row/'repo').exists())
            self.assertEqual(json.loads((row/'result.json').read_text()), result)
            for path in output.rglob('*'):
                if path.is_file(): self.assertNotIn(b'sk-ant-', path.read_bytes())
            self.assertEqual((sibling/'keep').read_text(), 'sibling')
            self.assertEqual((outside/'keep').read_text(), 'outside')
            if index == 0:
                self.assertTrue(diagnostic.write.called)
                containers = list(quarantine.iterdir())
                self.assertEqual(len(containers), 1)
                self.assertEqual(containers[0].stat().st_mode & 0o777, 0o700)
                deepest = containers[0]/'evidence/repo/restricted'/Path(*(['d']*70))
                # Only this fixture's quarantined synthetic data is repaired
                # for test teardown; product cleanup stays bounded.
                deepest.chmod(0o700)
                self.assertEqual((deepest/'data').read_text(), self.key.read_text())
                shutil.rmtree(containers[0])
        self.assertEqual(list(quarantine.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
