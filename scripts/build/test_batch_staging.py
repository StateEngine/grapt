"""Staging qualifies whole touched crates without borrowing a workspace gate."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import batch_staging
import gate_plan


class BatchStaging(unittest.TestCase):
    def plan(self, root, paths):
        packages = [dict(id=name, name=name, dependencies=[],
                         manifest_path=str(root / 'crates' / name / 'Cargo.toml'),
                         targets=[dict(kind=['lib'], doctest=True)])
                    for name in ['leaf', 'consumer', 'unrelated', 'grapt-types', 'grapt-store']]
        packages[1]['dependencies'] = [dict(name='leaf')]
        return gate_plan.commands('affected', packages, paths, root, staging=True)

    def test_whole_touched_crate_lint_and_tests_keep_all_laws_and_licenses(self):
        with tempfile.TemporaryDirectory() as raw:
            commands = self.plan(Path(raw), {'crates/leaf/src/lib.rs', 'scripts/build/batch_land.py'})
        self.assertTrue(all('--workspace' not in command for command in commands))
        leaf_tests = [command for command in commands
                      if command[:2] in (['cargo', 'test'], ['cargo', 'nextest'])
                      and '-p' in command and command[command.index('-p') + 1] == 'leaf']
        self.assertTrue(leaf_tests, commands)
        for command in leaf_tests:
            self.assertNotIn('--test', command)
            self.assertNotIn('--lib', command)
            self.assertNotIn('-E', command)
            self.assertNotIn('--skip', command)
        lint = [command for command in commands if command[:2] == ['cargo', 'clippy']]
        self.assertEqual(len(lint), 1)
        self.assertIn('--all-targets', lint[0])
        self.assertEqual(lint[0][-3:], ['--', '-D', 'warnings'])
        self.assertIn('leaf', lint[0])
        self.assertNotIn('consumer', lint[0])
        self.assertNotIn('unrelated', lint[0])
        for law in ('no_god_files', 'direct_include_format', 'crate_graph', 'write_chokepoint'):
            self.assertTrue(any(law in command for command in commands), (law, commands))
        self.assertTrue(any(command[-2:] == ['scripts/release/licenses.py', 'check']
                            for command in commands), commands)
        self.assertTrue(any('unittest' in command and 'scripts/build' in command
                            for command in commands), commands)

    def test_changed_ci_helpers_run_the_ci_suite_without_widening_crates(self):
        with tempfile.TemporaryDirectory() as raw:
            commands = self.plan(Path(raw), {'scripts/ci/changes.py'})
        self.assertTrue(all('--workspace' not in command for command in commands))
        self.assertTrue(any('unittest' in command and 'scripts/ci' in command
                            for command in commands), commands)
        for law in ('no_god_files', 'direct_include_format', 'crate_graph', 'write_chokepoint'):
            self.assertTrue(any(law in command for command in commands), (law, commands))

    def test_staging_uses_its_own_scheduler_and_the_pinned_base(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / 'Cargo.lock').write_text('version = 3\n')
            log = root / 'staging.log'
            with patch.object(batch_staging.heavy, 'inherited', return_value=None), \
                    patch.object(batch_staging.heavy, 'run') as run, \
                    patch.object(batch_staging.compose, 'git'):
                batch_staging.qualify(root, 'a' * 40, log, timeout=120)
            commands = [call.args[0] for call in run.call_args_list]
            gates = [command for command in commands if 'scripts/gate.sh' in command]
            self.assertEqual(len(gates), 1)
            self.assertIn('--staging', gates[0])
            self.assertIn('--precheck', gates[0])
            self.assertEqual(gates[0][gates[0].index('--base') + 1], 'a' * 40)
            self.assertEqual(gates[0][gates[0].index('--log') + 1], str(log))
            self.assertTrue(all('--workspace' not in command for command in commands))
            with patch.object(batch_staging.heavy, 'inherited', return_value=object()), \
                    patch.object(batch_staging.heavy, 'run') as run:
                with self.assertRaises(ValueError):
                    batch_staging.qualify(root, 'a' * 40, log)
                run.assert_not_called()

    def test_lock_drift_cannot_become_a_staging_pass(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            lock = root / 'Cargo.lock'
            lock.write_text('version = 3\n')
            def drift(command, **_options):
                if 'scripts/gate.sh' in command:
                    lock.write_text('changed\n')
            with patch.object(batch_staging.heavy, 'inherited', return_value=None), \
                    patch.object(batch_staging.heavy, 'run', side_effect=drift), \
                    patch.object(batch_staging.compose, 'git'):
                with self.assertRaises(ValueError):
                    batch_staging.qualify(root, 'a' * 40, root / 'staging.log')


if __name__ == '__main__':
    unittest.main()
