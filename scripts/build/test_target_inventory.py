"""Inventory observes real targets without granting deletion authority."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import disk_hygiene
import evict_targets
import target_inventory as inventory


class InventoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.cache = self.home / '.cache/secode'
        self.cache.mkdir(parents=True)

    def test_managed_owners_whole_root_and_unmanaged_targets_are_reported_without_changes(self):
        owner = self.home / 'rust-fixture'
        target = owner / 'target'
        target.mkdir(parents=True)
        (target / 'keep').write_bytes(b'x' * 8192)
        key = hashlib.sha256(str(owner).encode()).hexdigest()
        managed = self.cache / 'target/toolchain/worktrees' / key
        managed.mkdir(parents=True)
        (managed / '.secode-worktree').write_text(str(owner) + '\n')
        (managed / 'artifact').write_bytes(b'y' * 8192)
        before = {p: (p.stat().st_mtime_ns, p.stat().st_size)
                  for p in [target, target / 'keep', managed, managed / 'artifact']}
        from types import SimpleNamespace
        def disk(path):
            free = 88 if Path(path) == self.cache.resolve() else 52
            return SimpleNamespace(total=1000, used=1000-free, free=free)
        with patch.object(inventory.shutil, 'disk_usage', side_effect=disk):
            result = inventory.report(self.cache, self.home)
        self.assertEqual(result['filesystem']['path'], str(self.cache.resolve()))
        self.assertEqual(result['filesystem']['available_bytes'], 88)
        self.assertEqual(result['root_filesystem']['path'], '/')
        self.assertEqual(result['root_filesystem']['available_bytes'], 52)
        self.assertEqual(result['managed_worktrees'][0]['owner'], str(owner))
        self.assertGreaterEqual(result['managed_total']['allocated_bytes'], 8192)
        self.assertEqual(result['unmanaged_targets'][0]['path'], str(target))
        self.assertEqual(result['unmanaged_targets'][0]['mtime'], target.stat().st_mtime)
        self.assertGreaterEqual(result['unmanaged_targets'][0]['allocated_bytes'], 8192)
        self.assertEqual(result['budget_bytes'], evict_targets.CAP_BYTES)
        self.assertEqual(result['free_space_floor_bytes'], evict_targets.FREE_SPACE_FLOOR_BYTES)
        self.assertEqual(result['build_reserve_bytes'], evict_targets.BUILD_RESERVE_BYTES)
        self.assertEqual(before, {p: (p.stat().st_mtime_ns, p.stat().st_size) for p in before})

    def test_symlinks_and_stateengine_checkout_are_never_measured(self):
        excluded = self.home / '5e-authr2/target'
        excluded.mkdir(parents=True)
        (excluded / 'keep').write_text('StateEngine')
        (self.home / 'linked-owner').symlink_to(excluded.parent, target_is_directory=True)
        other = self.home / 'other'
        other.mkdir()
        (other / 'target').symlink_to(excluded, target_is_directory=True)
        seen = []
        def size(path, deadline):
            seen.append(path)
            return 4096
        with patch.object(evict_targets, 'usage', side_effect=size):
            result = inventory.report(self.cache, self.home)
        self.assertEqual(result['unmanaged_targets'], [])
        self.assertFalse(any(excluded == p or excluded in p.parents for p in seen))
        self.assertEqual((excluded / 'keep').read_text(), 'StateEngine')

    def test_missing_unreadable_and_unknown_owner_do_not_turn_into_zero(self):
        missing = inventory.measure(self.home / 'absent', time.monotonic() + 5)
        self.assertIsNone(missing['allocated_bytes'])
        self.assertIn('error', missing)
        with patch.object(evict_targets, 'usage', side_effect=subprocess.TimeoutExpired('du', 1)):
            failed = inventory.measure(self.cache, time.monotonic() + 5)
        self.assertIsNone(failed['allocated_bytes'])
        self.assertIn('error', failed)
        unknown = self.cache / 'target/toolchain/worktrees/unknown'
        unknown.mkdir(parents=True)
        result = inventory.report(self.cache, self.home)
        self.assertIsNone(result['managed_worktrees'][0]['owner'])
        self.assertIn('owner_error', result['managed_worktrees'][0])

    def test_hardlinked_rows_do_not_replace_whole_root_measurement(self):
        import os
        a = self.cache / 'target/toolchain/worktrees/a'
        b = a.with_name('b')
        a.mkdir(parents=True)
        b.mkdir()
        (a / 'artifact').write_bytes(b'x' * 65536)
        os.link(a / 'artifact', b / 'artifact')
        result = inventory.report(self.cache, self.home)
        estimates = sum(row['allocated_bytes'] for row in result['managed_worktrees'])
        self.assertLess(result['managed_total']['allocated_bytes'], estimates)

    def test_older_direct_worktree_layout_and_unattributed_roots_remain_visible(self):
        owner = self.home / 'old-worktree'
        key = hashlib.sha256(str(owner).encode()).hexdigest()
        direct = self.cache / 'target/worktrees' / key
        direct.mkdir(parents=True)
        (direct / '.secode-worktree').write_text(str(owner) + '\n')
        legacy = self.cache / 'target/1.97.1/unmarked-checkout'
        legacy.mkdir(parents=True)
        (legacy / 'artifact').write_bytes(b'x' * 8192)
        result = inventory.report(self.cache, self.home)
        self.assertEqual(result['managed_worktrees'][0]['owner'], str(owner))
        self.assertEqual(result['managed_worktrees'][0]['path'], str(direct))
        self.assertEqual(result['other_managed_roots'][0]['path'], str(legacy.parent))
        self.assertGreaterEqual(result['other_managed_roots'][0]['allocated_bytes'], 8192)
        self.assertEqual((legacy / 'artifact').read_bytes(), b'x' * 8192)

    def test_inventory_command_uses_idle_io_and_never_runs_reclaim(self):
        with patch.object(sys, 'argv', ['disk_hygiene', '--inventory']), \
                patch.object(disk_hygiene.subprocess, 'run') as run, \
                patch.object(disk_hygiene, 'reclaim') as reclaim:
            self.assertEqual(disk_hygiene.main(), 0)
        self.assertEqual(run.call_args.args[0][:3], ['ionice', '-c', '3'])
        reclaim.assert_not_called()

    def test_incomplete_cli_report_exits_nonzero_and_keeps_null_size(self):
        run = subprocess.run([sys.executable, '-B', inventory.__file__, '--cache',
                              str(self.home / 'missing-cache'), '--home', str(self.home)],
                             capture_output=True, text=True, timeout=10)
        self.assertEqual(run.returncode, 1)
        result = json.loads(run.stdout)
        self.assertIsNone(result['whole_cache']['allocated_bytes'])
        self.assertFalse((self.home / 'missing-cache').exists())
