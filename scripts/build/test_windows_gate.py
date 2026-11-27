"""Cross-target coverage, persistent cache isolation and admission failures."""
import io
import os
from pathlib import Path
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import evict_targets
import gate
import gate_plan
import heavy
import windows_gate
from test_gate import package


class WindowsPlanTests(unittest.TestCase):
    def test_full_and_affected_keep_cross_checks_within_the_affected_graph(self):
        packages = [package('grapt-product'), package('consumer', ['grapt-product']),
                    package('unrelated')]
        for mode in ('affected', 'full', 'composition'):
            with self.subTest(mode=mode):
                commands = gate_plan.commands(mode, packages,
                    {'crates/grapt-product/src/main.rs'}, Path('/repo'))
                cross = [row for row in commands if row[:2] == ['cargo', 'xwin']]
                self.assertEqual(len(cross), 4)
                for row in cross:
                    selected = [row[index+1] for index, value in enumerate(row) if value == '-p']
                    self.assertEqual(selected, ['consumer', 'grapt-product'])
                    self.assertNotIn('--workspace', row)
                    if '--features' in row:
                        self.assertIn('grapt-product/loopback-provider-testkit', row[row.index('--features')+1])
                self.assertEqual(sum('--features' in row for row in cross), 2)

    def test_docs_and_build_only_slices_add_no_unaffected_windows_builds(self):
        for mode in ('affected', 'full', 'composition'):
            for path in ('docs/guide.md', 'scripts/build/gate.py'):
                with self.subTest(mode=mode, path=path):
                    planned = gate_plan.commands(mode, [package('grapt-product')], {path}, Path('/repo'))
                    self.assertFalse(any(row[:2] == ['cargo', 'xwin'] for row in planned))

    def test_nightly_also_covers_the_unchanged_windows_workspace(self):
        planned = gate_plan.commands('full', [], set(), Path('/repo'), nightly=True)
        cross = [row for row in planned if row[:2] == ['cargo', 'xwin']]
        self.assertEqual(len(cross), 4)
        for row in cross:
            self.assertIn('--workspace', row)
            self.assertNotIn('-p', row)

    def test_cross_commands_link_tests_without_running_and_lint_every_target_strictly(self):
        lint, compile_tests, ordinary_lint, ordinary_tests = windows_gate.commands(['grapt-product'])
        self.assertEqual(lint[:3], ['cargo', 'xwin', 'clippy'])
        self.assertIn('--all-targets', lint)
        self.assertEqual(lint[-3:], ['--', '-D', 'warnings'])
        self.assertEqual(compile_tests[:3], ['cargo', 'xwin', 'test'])
        self.assertIn('--no-run', compile_tests)
        self.assertNotIn('--', compile_tests)
        for row in (lint, compile_tests):
            self.assertIn('--locked', row)
            self.assertEqual(row[row.index('--target')+1], windows_gate.TARGET)
            self.assertEqual(row[row.index('--features')+1],
                             gate.test_features.arguments(['grapt-product'])[1])
        self.assertNotIn('--features', ordinary_lint)
        self.assertNotIn('--features', ordinary_tests)
        self.assertIn('--all-targets', ordinary_lint)
        self.assertEqual(ordinary_lint[-3:], ['--', '-D', 'warnings'])
        self.assertIn('--no-run', ordinary_tests)

    def test_crates_without_fixture_features_need_only_one_configuration(self):
        planned = windows_gate.commands(['grapt-types'])
        self.assertEqual(len(planned), 2)
        self.assertFalse(any('--features' in row for row in planned))


class WindowsEnvironmentTests(unittest.TestCase):
    target = Path('/srv/build/cache/target/toolchain/worktrees/fixture')
    environment = {
        'SECODE_WINDOWS_TARGET_DIR': str(target),
        'CARGO_TARGET_DIR': '/tmp/secode-ramtarget/run-fixture/target/worktrees/fixture',
        'SECODE_TARGET_ROOT': '/tmp/secode-ramtarget/run-fixture/target',
        'SECODE_WORKTREE_TARGET': '/tmp/secode-ramtarget/run-fixture/target/worktrees/fixture',
        'SECODE_RAM_TARGET_RUN': '/tmp/secode-ramtarget/run-fixture',
        'SECODE_RAM_DISK_CACHE': '/srv/build/cache',
        'SECODE_CACHE_LEASE_FD': '900',
        'SECODE_CACHE_ADMITTED_TARGET': '/tmp/secode-ramtarget/run-fixture/target/worktrees/fixture',
        'SECODE_CACHE_ADMISSION_PROCESS': 'parent RAM claim',
        'SECODE_HEAVY_LEASE': 'preserve scheduler claim',
        'RUSTC_WRAPPER': '/fixture/scripts/build/rustc-cache.sh',
    }

    def test_ram_parent_keeps_its_claim_and_cross_artifacts_stay_on_nvme(self):
        with patch.dict(os.environ, self.environment, clear=True), \
                patch.object(heavy, 'nvme_target', return_value=self.target):
            before = dict(os.environ)
            child = windows_gate.environment(['cargo', 'xwin', 'test'])
            self.assertEqual(dict(os.environ), before)
        self.assertEqual(child['CARGO_TARGET_DIR'], str(self.target))
        self.assertEqual(child['SECODE_WORKTREE_TARGET'], str(self.target))
        self.assertEqual(child['SECODE_TARGET_ROOT'], str(self.target.parent.parent))
        self.assertEqual(child['SECODE_HEAVY_LEASE'], self.environment['SECODE_HEAVY_LEASE'])
        self.assertEqual(child['RUSTC_WRAPPER'], self.environment['RUSTC_WRAPPER'])
        self.assertFalse(any(name.startswith('SECODE_CACHE_') or name.startswith('SECODE_RAM_')
                             for name in child))

    def test_missing_or_non_nvme_target_refuses_without_changing_parent(self):
        for target in ('', 'relative/target', '/tmp/target'):
            with self.subTest(target=target), patch.dict(os.environ,
                    {'SECODE_WINDOWS_TARGET_DIR': target}, clear=True), \
                    patch.object(heavy, 'nvme_target', return_value=None):
                with self.assertRaisesRegex(ValueError, 'checkout NVMe target'):
                    windows_gate.environment(['cargo', 'xwin', 'test'])

    def test_linux_cargo_does_not_select_or_validate_a_cross_target(self):
        with patch.object(heavy, 'nvme_target') as nvme:
            self.assertIsNone(windows_gate.environment(['cargo', 'test']))
            nvme.assert_not_called()

    def test_execute_admits_and_lends_only_the_actual_disk_target(self):
        with tempfile.TemporaryFile() as held, patch.dict(os.environ, self.environment, clear=True), \
                patch.object(heavy, 'nvme_target', return_value=self.target), \
                patch.object(heavy, 'inherited_fds', return_value=()), \
                patch.object(evict_targets, 'prepare', return_value=held) as prepare, \
                patch.object(heavy, 'owned_process') as owned, \
                patch.object(heavy, 'wait_owned', return_value=0):
            owned.return_value.__enter__.return_value = SimpleNamespace(stdout=None)
            before = dict(os.environ)
            gate.execute(['cargo', 'xwin', 'test'], io.StringIO(), time.monotonic()+30,
                         cwd=Path('/fixture'))
            prepare.assert_called_once_with(self.target.parent.parent, self.target,
                                            Path('/fixture'), proc=Path('/proc'))
            child = owned.call_args.kwargs['env']
            self.assertEqual(child['SECODE_CACHE_ADMITTED_TARGET'], str(self.target))
            self.assertIn(int(child['SECODE_CACHE_LEASE_FD']), owned.call_args.kwargs['pass_fds'])
            self.assertEqual(dict(os.environ), before)
            self.assertTrue(held.closed)

    def test_disk_admission_failure_never_starts_the_cross_compiler(self):
        with patch.dict(os.environ, self.environment, clear=True), \
                patch.object(heavy, 'nvme_target', return_value=self.target), \
                patch.object(heavy, 'inherited_fds', return_value=()), \
                patch.object(evict_targets, 'prepare', side_effect=RuntimeError('disk admission refused')), \
                patch.object(heavy, 'owned_process') as owned:
            with self.assertRaisesRegex(RuntimeError, 'disk admission refused'):
                gate.execute(['cargo', 'xwin', 'test'], io.StringIO(), time.monotonic()+30)
            owned.assert_not_called()
