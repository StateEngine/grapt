"""Managed-cache deletion and live-build protection, without compiling Rust."""
import hashlib
import json
import contextlib
import io
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import evict_targets as eviction
import orphan_cache
from fixture_environment import isolated_environment


class _SyntheticProcess:
    """The handle shape the teardown expects, with nothing to tear down."""

    def __init__(self, *args, **kwargs):
        return None

    def terminate(self):
        return None

    def wait(self, timeout=None):
        return 0

    stdout = None


class EvictionTests(unittest.TestCase):
    def test_target_probe_scales_to_a_hard_cap_and_caches_success_only(self):
        self.addCleanup(eviction.rust_targets.cache_clear)
        for load, expected in [(0, 30.0), (4, 120.0), (100, 240.0)]:
            with self.subTest(load=load), \
                    patch.object(eviction.os, 'getloadavg', return_value=(load, 0, 0)), \
                    patch.object(eviction.os, 'cpu_count', return_value=1), \
                    patch.object(eviction.subprocess, 'check_output') as probe:
                eviction.rust_targets.cache_clear()
                probe.side_effect = [subprocess.TimeoutExpired('rustc', expected),
                                     'fixture-target\n']
                with self.assertRaises(subprocess.TimeoutExpired):
                    eviction.rust_targets()
                self.assertEqual(eviction.rust_targets(), frozenset({'fixture-target'}))
                self.assertEqual(eviction.rust_targets(), frozenset({'fixture-target'}))
                self.assertEqual(probe.call_count, 2)
                for call in probe.call_args_list:
                    self.assertEqual(call.args[0][1:], ['--print', 'target-list'])
                    self.assertEqual(call.kwargs['timeout'], expected)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        # A fixture process table: empty unless a test builds an entry, and it
        # owns the boot ID so identity never comes from the host.
        self.proc = self.directory / 'proc'
        (self.proc / 'sys/kernel/random').mkdir(parents=True, exist_ok=True)
        (self.proc / 'sys/kernel/random/boot_id').write_text('fixture-boot\n')
        self.cache = self.directory / 'cache'
        self.toolchain = self.cache / 'target' / 'rustc-test'
        # Eviction records belong to the data root; keep this fixture's out of
        # the operator's, and outside the cache the cap measures.
        self.diagnostics = self.directory / 'diagnostics'
        records = patch.object(eviction, 'record_directory', return_value=self.diagnostics)
        records.start()
        self.addCleanup(records.stop)
        self.now = time.time()
        # Legacy fixtures exercise eviction decisions independently of host load.
        self.disk = patch.object(eviction.shutil, 'disk_usage',
                                 return_value=SimpleNamespace(free=100*1024**3))
        self.disk.start()
        self.addCleanup(self.disk.stop)
        self.queue = patch.object(eviction, 'HEAVY_QUEUE', self.directory / 'no-queue')
        self.queue.start()
        self.addCleanup(self.queue.stop)
        references = patch.object(orphan_cache, 'active', return_value=set())
        references.start()
        self.addCleanup(references.stop)

    def target(self, name, age=0, present=True, toolchain=None):
        checkout = self.directory / name
        if present:
            checkout.mkdir()
        key = hashlib.sha256(str(checkout).encode()).hexdigest()
        target = (toolchain or self.toolchain) / 'worktrees' / key
        target.mkdir(parents=True)
        marker = target / '.secode-worktree'
        marker.write_text(str(checkout) + '\n')
        (target / 'debug').mkdir()
        (target / 'debug' / 'artifact').write_bytes(b'x' * 8192)
        timestamp = self.now - age
        for path in (target, marker, target / 'debug', target / 'debug' / 'artifact'):
            os.utime(path, (timestamp, timestamp))
        return target, checkout

    def test_configured_whole_cache_budget_is_one_hundred_fifty_gib(self):
        self.assertEqual(eviction.CAP_BYTES, 150 * 1024**3)

    def test_whole_targets_go_stale_after_twelve_hours(self):
        # The cap can only bind if idle targets become candidates inside a day.
        self.assertEqual(eviction.STALE_SECONDS, 12 * 3600)

    def test_a_target_idle_past_the_stale_window_is_evicted_and_a_younger_one_is_not(self):
        # The cap is only a ceiling if the idle rule actually reaches a target
        # that is a half-day old. Both ages sit well past RECENT_SECONDS, so
        # the stale threshold is the only thing that can separate them. This
        # uses the configured cap, not `pressure`'s cap of 1: a 1-byte cap puts
        # the cache over its limit and lets the recency fallback evict both.
        # The stale window gates candidacy only while the cache is under its
        # cap; over-cap pressure evicts any disposable target. So this runs at
        # the configured cap, where the fixture is far below it.
        current, _ = self.target('current', age=100000)
        stale, _ = self.target('stale', age=eviction.STALE_SECONDS + 60)
        younger, _ = self.target('younger', age=eviction.STALE_SECONDS - 60)
        removed, _ = self.evict(current)
        self.assertIn(stale, removed)
        self.assertNotIn(younger, removed)
        self.assertTrue(younger.is_dir())

    def test_cargo_census_uses_the_same_confirmed_exit_guard_as_open_references(self):
        for state in ('Z', 'S'):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as temporary:
                proc = Path(temporary)
                process = proc/'123'; process.mkdir()
                (process/'stat').write_text('123 (fixture) S 1 1')
                (process/'comm').write_text('fixture')
                (process/'task/123').mkdir(parents=True)
                readlink = os.readlink
                def exiting(path, *args, **kwargs):
                    if Path(path) == process/'exe':
                        (process/'stat').write_text(f'123 (fixture) {state} 1 1')
                        raise FileNotFoundError('fixture process exe disappeared')
                    return readlink(path, *args, **kwargs)
                with patch.object(os, 'readlink', exiting):
                    observed = eviction.live_targets({}, proc, time.monotonic()+5)
                if state == 'Z':
                    self.assertEqual(observed, set())
                else:
                    self.assertIsNone(observed)

    def test_an_eviction_leaves_a_durable_record_beside_the_cache(self):
        # Attribution used to live only in the evicting job's stderr, which dies
        # with its scratch. The record must survive the run, on the cache's side.
        current, _ = self.target('current', age=100000)
        stale, _ = self.target('stale', age=eviction.STALE_SECONDS + 60)
        removed, _ = self.evict(current)
        self.assertIn(stale, removed)
        record = self.diagnostics / 'evictions.jsonl'
        self.assertTrue(record.is_file(), 'an eviction left no durable record')
        rows = [json.loads(line) for line in record.read_text().splitlines()]
        evicted = [row for row in rows if row['path'] == str(stale)]
        self.assertEqual(len(evicted), 1, f'expected one record for the evicted target: {rows}')
        row = evicted[0]
        self.assertEqual(row['reason'], 'stale')
        self.assertEqual(row['pid'], os.getpid())
        self.assertIsInstance(row['bytes'], int)
        self.assertIsInstance(row['removed_unix'], float)

    def test_an_unwritable_record_directory_still_evicts(self):
        # The target is already gone by the time the record is written, so a
        # logging failure must never fail or undo the eviction. The assertions
        # are on the eviction itself; the warning is only how we know the write
        # was attempted and refused.
        diagnostics = self.diagnostics
        diagnostics.parent.mkdir(parents=True, exist_ok=True)
        diagnostics.write_text('a file where the record directory belongs')
        current, _ = self.target('current', age=100000)
        stale, _ = self.target('stale', age=eviction.STALE_SECONDS + 60)
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            removed, _ = self.evict(current)
        self.assertIn(stale, removed, 'an unwritable record directory blocked eviction')
        self.assertFalse(stale.exists(), 'the eviction was undone by a record failure')
        self.assertTrue(current.is_dir(), 'the current target was not spared')
        self.assertIn('Eviction record unavailable', stderr.getvalue(),
                      'the refused record was not reported')

    def test_the_eviction_record_keeps_only_the_newest_rows(self):
        # The record is bounded by whole lines so it stays parseable per line.
        diagnostics = self.diagnostics
        diagnostics.mkdir(parents=True, exist_ok=True)
        path = diagnostics / 'evictions.jsonl'
        older = [json.dumps({'path': f'/old/{index}'}) for index in range(eviction.EVICTION_RECORDS_KEPT + 5)]
        path.write_text('\n'.join(older) + '\n')
        current, _ = self.target('current', age=100000)
        stale, _ = self.target('stale', age=eviction.STALE_SECONDS + 60)
        removed, _ = self.evict(current)
        self.assertIn(stale, removed)
        lines = path.read_text().splitlines()
        self.assertEqual(len(lines), eviction.EVICTION_RECORDS_KEPT)
        for line in lines:
            json.loads(line)
        self.assertEqual(json.loads(lines[-1])['path'], str(stale), 'the newest row was trimmed')

    def evict(self, current, cap=eviction.CAP_BYTES):
        with patch.object(eviction, 'live_targets', return_value=set()):
            return eviction.evict(self.cache, current, self.now, cap, proc=self.proc)

    def pressure(self, current):
        removed, total = self.evict(current, cap=1)
        self.assertTrue(all(not path.exists() for path in removed))
        self.assertEqual(total, eviction.usage(self.cache, time.monotonic()+5))
        return removed, total

    def cargo_process(self, checkout, target=None):
        """A synthetic live Cargo: no real child, no link into the host table.

        The process identity the census needs -- pid, start time and boot ID --
        comes from this fixture too, so nothing here is borrowed from the host.
        """
        self.process_count = getattr(self, 'process_count', 0) + 1
        pid = str(3000 + self.process_count)
        proc = self.directory / 'proc'
        proc.mkdir(exist_ok=True)
        (proc / 'sys/kernel/random').mkdir(parents=True, exist_ok=True)
        (proc / 'sys/kernel/random/boot_id').write_text('fixture-boot\n')
        live = proc / pid
        (live / 'fd').mkdir(parents=True)
        (live / 'task' / pid).mkdir(parents=True)
        (live / 'stat').write_bytes(pid.encode() + b' (cargo) S ' + b'0 ' * 18 + b'77\n')
        (live / 'task' / pid / 'children').write_text('')
        (live / 'comm').write_text('cargo\n')
        (live / 'exe').symlink_to('/fixture/cargo')
        (live / 'cwd').symlink_to(checkout, target_is_directory=True)
        environment = b'HOME=/fixture\0'
        if target is not None:
            environment += b'CARGO_TARGET_DIR=' + os.fsencode(target) + b'\0'
        (live / 'environ').write_bytes(environment)
        (live / 'cmdline').write_bytes(b'cargo\0check\0')
        return _SyntheticProcess(proc), proc

    def test_removes_orphans_and_24h_old_but_keeps_recent_and_current(self):
        current, _ = self.target('current', age=100000)
        stale, _ = self.target('stale', age=86401)
        orphan, _ = self.target('orphan', age=100000, present=False)
        recent, _ = self.target('recent', age=100)
        removed, _ = self.evict(current)
        self.assertEqual(set(removed), {stale, orphan})
        self.assertTrue(current.is_dir())
        self.assertTrue(recent.is_dir())

    def test_finished_orphan_is_evicted_immediately_even_below_the_cap(self):
        current, _ = self.target('current')
        finished, _ = self.target('finished', age=60, present=False)
        (finished/eviction.FINISHED).write_text('a'*40+'\n')
        removed, _ = self.evict(current)
        self.assertEqual(removed, [finished])
        self.assertTrue(current.exists())

    def test_protected_cache_above_the_cap_keeps_build_admission_open(self):
        current, _ = self.target('current')
        cap_gib = eviction.CAP_BYTES // 1024**3
        for gib in (cap_gib + 38, cap_gib - 1):
            diagnostic = io.StringIO()
            with self.subTest(gib=gib), contextlib.redirect_stderr(diagnostic), \
                    patch.object(eviction, 'usage', return_value=gib*1024**3), \
                    patch.object(eviction.shutil, 'disk_usage', return_value=SimpleNamespace(free=1024**4)), \
                    patch.object(eviction, 'live_targets', return_value={current}):
                removed, total = eviction.evict(self.cache, current, self.now, proc=self.proc)
                self.assertEqual((removed, total), ([], gib*1024**3))
                self.assertEqual('WARNING: cache remains above' in diagnostic.getvalue(), gib*1024**3 > eviction.CAP_BYTES)
                self.assertTrue(current.exists())

    def test_cap_evicts_lru_across_toolchains(self):
        current, _ = self.target('current')
        older, _ = self.target('older', age=7200, toolchain=self.cache / 'target' / 'old-rustc')
        for name in ('checkpoint-tests', 'x86_64-pc-windows-msvc'):
            directory = older/name
            directory.mkdir()
            os.utime(directory, (self.now-7200,)*2)
        os.utime(older, (self.now-7200,)*2)
        recent, _ = self.target('recent', age=100)
        cap = eviction.usage(self.cache, time.monotonic()+5) - eviction.usage(older, time.monotonic()+5) + 4096
        removed, total = self.evict(current, cap)
        self.assertEqual(removed, [older])
        self.assertLessEqual(total, cap)

    def test_full_root_filesystem_does_not_refuse_a_roomy_cache_mount(self):
        current, _ = self.target('current')
        def space(path):
            return SimpleNamespace(free=100 * 1024**3 if Path(path) == self.cache.resolve() else 0)
        with patch.object(eviction.shutil, 'disk_usage', side_effect=space):
            removed, total = self.evict(current)
        self.assertEqual(removed, [])
        self.assertGreater(total, 0)

    def test_full_cache_mount_refuses_even_when_root_has_space(self):
        current, _ = self.target('current')
        def space(path):
            return SimpleNamespace(free=1024**3 if Path(path) == self.cache.resolve() else 100 * 1024**3)
        with patch.object(eviction.shutil, 'disk_usage', side_effect=space):
            with self.assertRaisesRegex(eviction.DiskAdmissionRefused, 'Cache admission refused'):
                self.evict(current)
        self.assertTrue(current.exists())

    def test_rechecks_cache_mount_capacity_after_eviction(self):
        current, _ = self.target('current')
        older, _ = self.target('older', age=7200)
        def space(path):
            free = 1024**3 if Path(path) == self.cache.resolve() and older.exists() else 100 * 1024**3
            return SimpleNamespace(free=free)
        with patch.object(eviction.shutil, 'disk_usage', side_effect=space) as usage:
            removed, _ = self.evict(current)
        self.assertEqual(removed, [older])
        self.assertTrue(current.exists())
        self.assertTrue(all(call.args == (self.cache.resolve(),) for call in usage.call_args_list))

    def test_recent_nested_binary_survives_pressure_and_oldest_stale_goes_first(self):
        current, _ = self.target('current')
        oldest, _ = self.target('oldest', age=100000)
        older, _ = self.target('older', age=90000)
        recent, _ = self.target('recent', age=100000)
        nested = recent / 'debug' / 'deps'
        nested.mkdir(parents=True, exist_ok=True)
        binary = nested / 'grapt-server'
        binary.write_bytes(b'built minutes ago')
        # Even old directory mtimes must not hide a new binary's modification.
        for path in (recent, recent / 'debug', nested):
            os.utime(path, (self.now - 100000, self.now - 100000))
        os.utime(binary, (self.now - 120, self.now - 120))
        diagnostic = io.StringIO()
        with contextlib.redirect_stderr(diagnostic):
            removed, total = self.pressure(current)
        self.assertEqual(removed, [oldest, older])
        self.assertEqual(binary.read_bytes(), b'built minutes ago')
        self.assertGreater(total, 1)
        self.assertIn('Whole cache:', diagnostic.getvalue())

    def test_recent_orphan_is_protected_until_grace_expires(self):
        current, _ = self.target('current')
        orphan, _ = self.target('orphan', age=100, present=False)
        self.assertEqual(self.pressure(current)[0], [])
        with patch.object(eviction, 'live_targets', return_value=set()):
            removed, _ = eviction.evict(self.cache, current, self.now + eviction.RECENT_SECONDS, cap=1,
                              proc=self.proc)
            self.assertEqual(removed, [orphan])
        self.assertFalse(orphan.exists())

    def test_target_modified_after_inventory_survives_the_deletion_recheck(self):
        current, _ = self.target('current')
        old, _ = self.target('old', age=100000)
        acquire = eviction.lease
        def finish_build(target):
            held = acquire(target)
            os.utime(target / 'debug' / 'artifact', (self.now, self.now))
            return held
        with patch.object(eviction, 'lease', side_effect=finish_build):
            removed, _ = self.pressure(current)
        self.assertEqual(removed, [])
        self.assertTrue(old.is_dir())

    def test_running_product_binary_protects_an_old_target_without_cargo(self):
        current, _ = self.target('current')
        active, checkout = self.target('active', age=100000)
        binary = active / 'product'
        shutil.copyfile(sys.executable, binary)
        binary.chmod(0o755)
        for path in (active, binary):
            os.utime(path, (self.now - 100000, self.now - 100000))
        process = subprocess.Popen([str(binary), '-c',
                                    'import time; print("ready", flush=True); time.sleep(30)'],
                                   cwd=self.directory, stdout=subprocess.PIPE)
        proc = self.directory / 'proc'
        proc.mkdir(exist_ok=True)
        (proc / str(process.pid)).symlink_to(f'/proc/{process.pid}', target_is_directory=True)
        scanner = eviction.live_targets
        try:
            self.assertEqual(process.stdout.readline(), b'ready\n')
            with patch.object(eviction, 'live_targets', side_effect=lambda owners, proc_root=None, deadline=None, **options: scanner(owners, proc, deadline, **options)):
                removed, _ = eviction.evict(self.cache, current, self.now, cap=1,
                                              proc=self.proc)
                self.assertNotIn(active, removed)
            self.assertTrue(binary.is_file())
        finally:
            process.terminate()

    def test_build_lease_survives_an_independent_evictor(self):
        current, _ = self.target('current')
        active, _ = self.target('active', age=100000)
        held = eviction.lease(active)
        try:
            removed, total = self.pressure(current)
            self.assertEqual(removed, [])
            self.assertGreater(total, 1)  # Honest pressure report, never delete active output.
            self.assertTrue(active.is_dir())
        finally:
            held.close()
        self.assertIn(active, self.evict(current)[0])

    def test_live_cargo_without_a_lease_is_protected(self):
        current, _ = self.target('current')
        active, checkout = self.target('active', age=100000)
        process, proc = self.cargo_process(checkout, active)
        scanner = eviction.live_targets
        try:
            self.assertIn(active, scanner({active: checkout}, proc))
            with patch.object(eviction, 'live_targets', side_effect=lambda owners, proc_root=None, deadline=None, **options: scanner(owners, proc, deadline, **options)):
                removed, _ = eviction.evict(self.cache, current, self.now, proc=self.proc)
            self.assertNotIn(active, removed)
            self.assertTrue(active.is_dir())
        finally:
            process.terminate()
        self.assertIn(active, self.evict(current)[0])

    def test_cargo_inside_an_owned_target_keeps_that_target_without_blocking_idle_eviction(self):
        current, _ = self.target('current')
        active, checkout = self.target('active', age=100000)
        idle, _ = self.target('idle', age=100000)
        nested = active/'tests/trybuild/fixture'
        nested.mkdir(parents=True)
        process, proc = self.cargo_process(nested, target=nested/'target')
        scanner = eviction.live_targets
        try:
            owners = {active: checkout}
            self.assertEqual(scanner(owners, proc), {active, (nested/'target').resolve()})
            with patch.object(eviction, 'live_targets', side_effect=lambda owners, proc_root=None, deadline=None, **options: scanner(owners, proc, deadline, **options)):
                removed, _ = eviction.evict(self.cache, current, self.now, proc=self.proc)
            self.assertIn(idle, removed)
            self.assertNotIn(active, removed)
            self.assertTrue(active.is_dir())
        finally:
            process.terminate()

    def test_unknown_live_cargo_prevents_all_deletion(self):
        current, _ = self.target('current')
        stale, _ = self.target('stale', age=100000)
        process, proc = self.cargo_process(self.directory)
        scanner = eviction.live_targets
        try:
            self.assertIsNone(scanner({stale: self.directory / 'stale'}, proc))
            with patch.object(eviction, 'live_targets', side_effect=lambda owners, proc_root=None, deadline=None, **options: scanner(owners, proc, deadline, **options)):
                self.assertEqual(eviction.evict(self.cache, current, self.now, proc=self.proc)[0], [])
            self.assertTrue(stale.is_dir())
        finally:
            process.terminate()

    def test_symlink_entries_and_unmarked_outputs_are_preserved(self):
        current, _ = self.target('current')
        external = self.directory / 'external'
        external.mkdir()
        (external / 'keep').write_text('protected')
        (current.parent / ('a' * 64)).symlink_to(external, target_is_directory=True)
        unmarked = current.parent / ('b' * 64)
        unmarked.mkdir()
        self.assertEqual(self.evict(current)[0], [])
        self.assertEqual((external / 'keep').read_text(), 'protected')
        self.assertTrue(unmarked.is_dir())
        _, total = self.pressure(current)
        self.assertGreater(total, 1)

    def test_symlinks_inside_an_evicted_target_are_not_followed(self):
        current, _ = self.target('current')
        stale, _ = self.target('stale', age=100000)
        external = self.directory / 'external'
        external.mkdir()
        (external / 'keep').write_text('protected')
        (stale / 'debug' / 'link').symlink_to(external, target_is_directory=True)
        os.utime(stale / 'debug' / 'link', (self.now - 100000, self.now - 100000), follow_symlinks=False)
        os.utime(stale / 'debug', (self.now - 100000, self.now - 100000))
        os.utime(stale, (self.now - 100000, self.now - 100000))
        self.assertIn(stale, self.evict(current)[0])
        self.assertEqual((external / 'keep').read_text(), 'protected')

    def test_wrong_owner_and_inventory_overflow_refuse_maintenance(self):
        current, _ = self.target('current')
        other, _ = self.target('other')
        (other / '.secode-worktree').write_text('/another/checkout\n')
        self.assertEqual(self.evict(current)[0], [])
        with self.assertRaisesRegex(RuntimeError, 'inventory exceeds'):
            eviction.bounded_entries(current.parent, 1)
        with patch.object(eviction, 'MAX_TARGET_ENTRIES', 1):
            with self.assertRaisesRegex(RuntimeError, 'entry bound'):
                eviction.latest_modified(current, time.monotonic() + 5)
        self.assertTrue(other.is_dir())

    def test_prepare_records_usage_and_holds_target_until_build_ends(self):
        target, checkout = self.target('checkout', age=100000)
        with patch.object(eviction, 'live_targets', return_value=set()):
            held = eviction.prepare(self.toolchain, target, checkout, proc=self.proc)
        try:
            self.assertGreater(eviction.marker_owner(target)[1], self.now - 10)
            with self.assertRaises(BlockingIOError):
                eviction.lease(target)
        finally:
            held.close()
        next_lease = eviction.lease(target)
        next_lease.close()

    def test_concurrent_builds_share_the_target_lease(self):
        target, checkout = self.target('checkout', age=100000)
        with patch.object(eviction, 'live_targets', return_value=set()):
            first = eviction.prepare(self.toolchain, target, checkout, proc=self.proc)
            try:
                # A second build of the same checkout is admitted; Cargo serializes
                # the two on its own lock. It once failed here with EAGAIN.
                second = eviction.prepare(self.toolchain, target, checkout, proc=self.proc)
                second.close()
            finally:
                first.close()

    def test_overlapping_first_registrations_are_both_admitted(self):
        # BL-H1: with a shared lease two FIRST registrations can overlap. The
        # first is paused once its owner file exists but holds nothing yet, the moment
        # an in-place write exposes, and the second registers in full meanwhile.
        current, _ = self.target('current')
        checkout = self.directory / 'fresh'
        checkout.mkdir()
        target = self.toolchain / 'worktrees' / hashlib.sha256(str(checkout).encode()).hexdigest()
        original = eviction.write_owner
        overlap = []

        def paused(path, owner):
            path.write_text('')
            if not overlap:
                overlap.append(None)
                overlap[0] = eviction.register(self.toolchain, target, checkout)
            original(path, owner)

        with patch.object(eviction, 'write_owner', side_effect=paused):
            first = eviction.register(self.toolchain, target, checkout)
        second = overlap[0]
        try:
            self.assertIsNotNone(second)
            owner, _ = eviction.marker_owner(target)
            self.assertEqual(owner, checkout.resolve())
            self.assertEqual(sorted(path.name for path in target.iterdir()), ['.secode-worktree'])
            # Both hold the shared lease: the target is protected from eviction.
            first.close()
            os.utime(target, (self.now - 100000, self.now - 100000))
            os.utime(target / '.secode-worktree', (self.now - 100000, self.now - 100000))
            self.assertNotIn(target, self.pressure(current)[0])
        finally:
            first.close()
            second.close()

    def test_overlapping_registration_does_not_change_the_owner_being_read(self):
        target, checkout = self.target('checkout', age=100000)
        marker = target / '.secode-worktree'
        original = os.fstat
        inode = marker.stat().st_ino
        overlap = []

        def paused(descriptor):
            info = original(descriptor)
            if info.st_ino == inode and not overlap:
                overlap.append(None)
                overlap[0] = eviction.register(self.toolchain, target, checkout)
            return info

        try:
            # A real second registration lands between the reader's two stats.
            with patch.object(os, 'fstat', side_effect=paused):
                owner, used = eviction.marker_owner(target)
            self.assertEqual(owner, checkout.resolve())
            self.assertGreater(used, self.now - 10)
            self.assertEqual(marker.read_text(), str(checkout) + '\n')
            with self.assertRaises(BlockingIOError):
                eviction.lease(target)
        finally:
            for held in overlap:
                if held is not None:
                    held.close()
        eviction.lease(target).close()

    def test_registration_still_refuses_a_marker_naming_another_owner(self):
        target, checkout = self.target('checkout')
        (target / '.secode-worktree').write_text('/another/checkout\n')
        with self.assertRaisesRegex(RuntimeError, 'does not match'):
            eviction.register(self.toolchain, target, checkout)
        # The refused registration released its lease.
        eviction.lease(target).close()

    def test_a_stale_inherited_survivor_does_not_refuse_a_new_build(self):
        # The 2026-10-09 failure: a build's child outlived it, still holding the
        # inherited lease, and every later shared-cargo in that worktree refused.
        current, _ = self.target('current')
        target, checkout = self.target('checkout', age=100000)
        with patch.object(eviction, 'live_targets', return_value=set()):
            held = eviction.prepare(self.toolchain, target, checkout, proc=self.proc)
        survivor = subprocess.Popen(
            [sys.executable, '-c', 'import sys, time; print("ready", flush=True); time.sleep(30)'],
            pass_fds=(held.fileno(),), stdout=subprocess.PIPE)
        self.addCleanup(survivor.stdout.close)
        try:
            self.assertEqual(survivor.stdout.readline(), b'ready\n')
            held.close()  # The build ended; only its survivor holds the lease now.
            with patch.object(eviction, 'live_targets', return_value=set()):
                again = eviction.prepare(self.toolchain, target, checkout, proc=self.proc)
            again.close()
            # The survivor still protects its target from eviction.
            removed, _ = self.pressure(current)
            self.assertNotIn(target, removed)
            self.assertTrue(target.is_dir())
        finally:
            survivor.terminate()
            survivor.wait(timeout=5)
        os.utime(target, (self.now - 100000, self.now - 100000))
        os.utime(target / '.secode-worktree', (self.now - 100000, self.now - 100000))
        self.assertIn(target, self.pressure(current)[0])

    def test_a_shared_build_lease_protects_its_target_from_eviction(self):
        current, _ = self.target('current')
        active, _ = self.target('active', age=100000)
        held = eviction.lease(active, shared=True)
        try:
            removed, _ = self.pressure(current)
            self.assertEqual(removed, [])
            self.assertTrue(active.is_dir())
            with self.assertRaises(BlockingIOError):
                eviction.lease(active)
        finally:
            held.close()
        self.assertIn(active, self.evict(current)[0])

    def test_live_cargo_with_deleted_cwd_keeps_known_target_protected(self):
        current, _ = self.target('current')
        active, checkout = self.target('active', age=100000)
        process, proc = self.cargo_process(checkout, active)
        scanner = eviction.live_targets
        try:
            checkout.rmdir()
            self.assertIsNone(scanner({active: checkout}, proc))
            with patch.object(eviction, 'live_targets', side_effect=lambda owners, proc_root=None, deadline=None, **options: scanner(owners, proc, deadline, **options)):
                self.assertEqual(eviction.evict(self.cache, current, self.now, proc=self.proc)[0], [])
            self.assertTrue(active.is_dir())
            self.assertIsNone(process.poll() if hasattr(process, 'poll') else None)
        finally:
            process.terminate()

    def test_slow_unknown_process_scan_cannot_succeed_past_deadline(self):
        current, _ = self.target('current')
        clock = [0.0]
        def slow_scan(*args, **kwargs):
            clock[0] = 0.08
            return None
        # Spend the deadline in the process scan, independent of du and host load.
        with patch.object(eviction, 'usage', return_value=8192), \
             patch.object(eviction.time, 'monotonic', side_effect=lambda: clock[0]), \
             patch.object(eviction, 'live_targets', side_effect=slow_scan) as scan:
            with self.assertRaisesRegex(RuntimeError, 'time limit'):
                eviction.evict(self.cache, current, self.now, deadline=0.02,
                                   proc=self.proc)
            scan.assert_called_once()
        self.assertTrue(current.is_dir())

    def test_unreadable_accounting_and_timeout_refuse_and_release_lease(self):
        target, checkout = self.target('checkout')
        idle, _ = self.target('idle', age=100000)
        for error in [PermissionError('unreadable cache'), subprocess.TimeoutExpired(['du'],60),
                      subprocess.CalledProcessError(1, ['du'])]:
            with self.subTest(error=type(error).__name__), patch.object(eviction,'usage',side_effect=error):
                with self.assertRaises(type(error)):
                    eviction.prepare(self.toolchain,target,checkout, proc=self.proc)
                self.assertTrue(idle.is_dir())
                eviction.lease(target).close()

    def test_accounting_accepts_total_when_a_walked_directory_vanishes(self):
        vanished = self.cache/'debug/deps/rustc-vanished'
        vanished.mkdir(parents=True)
        du = shutil.which('du')
        self.assertIsNotNone(du)
        fake = self.directory/'bin'
        fake.mkdir()
        # Model a name retained by a walk after rustc removes it. Real du
        # reports the surviving root total and ENOENT for that vanished name.
        (fake/'du').write_text(f'''#!{sys.executable}
import subprocess, sys
from pathlib import Path
root = Path(sys.argv[-1])
listed = list((root/'debug/deps').iterdir())
assert len(listed) == 1
vanished = listed[0]
vanished.rmdir()
sys.exit(subprocess.call([{du!r}, *sys.argv[1:], str(vanished)]))
''')
        (fake/'du').chmod(0o755)
        with patch.dict(os.environ, {'PATH':str(fake)+os.pathsep+os.environ['PATH']}):
            measured = eviction.usage(self.cache, time.monotonic()+10)
        expected = subprocess.check_output([du, '-sx', '--block-size=1', '--', str(self.cache)])
        self.assertEqual(measured, int(expected.split(None, 1)[0]))
        self.assertFalse(vanished.exists())

    def test_accounting_retries_unparseable_exit_one_once_with_remaining_time(self):
        self.cache.mkdir()
        clock = [0.0]
        def walk(command, **kwargs):
            clock[0] += 2
            return subprocess.CompletedProcess(command, 1 if clock[0] == 2 else 0,
                                               b'' if clock[0] == 2 else b'4096 cache\n')
        with patch.object(eviction.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(eviction.subprocess, 'run', side_effect=walk) as walks:
            self.assertEqual(eviction.usage(self.cache, 5), 4096)
        self.assertEqual([call.kwargs['timeout'] for call in walks.call_args_list], [5, 3])

    def test_accounting_garbage_negative_and_oversized_totals_refuse(self):
        self.cache.mkdir()
        for code, output, attempts in [(0, b'', 1), (1, b'garbage', 2),
                                       (1, b'-1 cache\n', 1), (1, b'1 '+b'x'*8192, 1)]:
            with self.subTest(code=code, output=output[:16]), \
                    patch.object(eviction.subprocess, 'run', return_value=
                                 subprocess.CompletedProcess(['du'], code, output)) as walks:
                with self.assertRaises(RuntimeError):
                    eviction.usage(self.cache, time.monotonic()+5)
                self.assertEqual(walks.call_count, attempts)

    def test_accounting_timeout_never_retries(self):
        with patch.object(eviction.subprocess, 'run',
                          side_effect=subprocess.TimeoutExpired(['du'], 1)) as walks:
            with self.assertRaises(subprocess.TimeoutExpired):
                eviction.usage(self.cache, time.monotonic()+5)
            walks.assert_called_once()

    def test_busy_maintenance_lock_refuses_within_deadline(self):
        import fcntl
        target, checkout = self.target('checkout')
        with (self.cache/'.eviction.lock').open('a') as busy:
            fcntl.flock(busy,fcntl.LOCK_EX|fcntl.LOCK_NB)
            with patch.object(eviction, 'MAINTENANCE_SECONDS', 0.05):
                with self.assertRaises(subprocess.SubprocessError):
                    eviction.prepare(self.toolchain,target,checkout, proc=self.proc)
        eviction.lease(target).close()

    def test_whole_root_counts_protected_and_legacy_categories(self):
        current, _ = self.target('current')
        for category in ['sccache', 'fixtures', 'seat-evidence', 'unknown', 'target/legacy']:
            path = self.cache / category
            path.mkdir(parents=True)
            (path / 'keep').write_bytes(b'p'*8192)
        total = eviction.usage(self.cache, time.monotonic()+5)
        removed, observed = self.evict(current, cap=total)
        self.assertEqual((removed, observed), ([], total))
        self.assertEqual(self.evict(current, cap=total-1), ([], total))
        for category in ['sccache', 'fixtures', 'seat-evidence', 'unknown', 'target/legacy']:
            self.assertEqual((self.cache/category/'keep').read_bytes(), b'p'*8192)

    def test_hardlink_reclaim_is_recounted_not_subtracted(self):
        current, _ = self.target('current')
        old, _ = self.target('old', age=100000)
        evidence = self.cache / 'evidence'
        evidence.mkdir()
        (old/'debug'/'artifact').write_bytes(b'x'*65536)
        os.link(old/'debug'/'artifact', evidence/'retained')
        for path in [old, old/'debug', old/'debug'/'artifact']:
            os.utime(path, (self.now-100000,)*2)
        total = eviction.usage(self.cache,time.monotonic()+5)
        # Deleting the target cannot reclaim the linked 64KiB. A sum-based
        # subtraction would falsely report that the cleanup target was met.
        diagnostic = io.StringIO()
        with contextlib.redirect_stderr(diagnostic):
            removed, observed = self.evict(current, cap=total-32768)
        self.assertEqual(removed, [old])
        self.assertEqual(observed, eviction.usage(self.cache, time.monotonic()+5))
        self.assertIn('WARNING: cache remains above', diagnostic.getvalue())
        self.assertFalse(old.exists())
        self.assertEqual((evidence/'retained').stat().st_size,65536)
        self.assertGreater(eviction.usage(self.cache,time.monotonic()+5),total-32768)

    def test_qualified_logs_and_unknown_target_content_are_preserved(self):
        current, _ = self.target('current')
        targets=[]
        for name, protected in [('qualified','.secode-retain'),('logs','gate-logs'),('unknown','source')]:
            target, owner = self.target(name,age=100000)
            (target/protected).write_text('retained evidence')
            os.utime(target/protected,(self.now-100000,)*2)
            os.utime(target,(self.now-100000,)*2)
            targets.append(target)
        self.assertEqual(self.pressure(current)[0],[])
        self.assertTrue(all(target.exists() for target in targets))
        eviction.retain(targets[0], self.directory/'qualified',False)
        os.utime(targets[0],(self.now-100000,)*2)
        os.utime(targets[0]/eviction.RELEASED,(self.now-100000,)*2)
        self.assertIn(targets[0], self.evict(current)[0])

    def test_an_absent_boot_id_refuses_and_admits_nothing(self):
        # The fixture itself owns a boot ID. A reader that cannot find one has
        # no identity to prove, so it refuses rather than filling a blank.
        target, checkout = self.target('checkout')
        absent = self.directory / 'no-boot'
        entry = absent / '4321'
        (entry / 'task' / '4321').mkdir(parents=True)
        (entry / 'stat').write_bytes(b'4321 (cargo) S ' + b'0 ' * 18 + b'88\n')
        (entry / 'comm').write_text('cargo\n')
        (entry / 'task' / '4321' / 'children').write_text('')
        with self.assertRaises(FileNotFoundError):
            eviction.process_identity(4321, proc=absent)
        # And a blank file is refused the same way, never returned as ''.
        blank = self.directory / 'blank-boot'
        (blank / 'sys/kernel/random').mkdir(parents=True)
        (blank / 'sys/kernel/random/boot_id').write_text('   \n')
        (blank / '4321' / 'task' / '4321').mkdir(parents=True)
        (blank / '4321' / 'stat').write_bytes(b'4321 (cargo) S ' + b'0 ' * 18 + b'88\n')
        (blank / '4321' / 'comm').write_text('cargo\n')
        (blank / '4321' / 'task' / '4321' / 'children').write_text('')
        with self.assertRaises(ValueError):
            eviction.process_identity(4321, proc=blank)
        record = target / '.secode-admission'
        self.assertFalse(record.exists(), 'no identity means no admission record')

    def test_an_admission_identity_missing_a_component_is_refused_not_reused(self):
        # A stored record matches on identity alone, before capacity is
        # rechecked, so an identity missing any part must never reach the
        # comparison. This is the case that bit when a missing boot ID was
        # blank-filled: it matched a record from a different boot.
        target, checkout = self.target('checkout')
        complete = [123, 'start', 'boot']
        with patch.object(eviction, 'evict') as inventory:
            eviction.prepare(self.toolchain, target, checkout, complete, proc=self.proc).close()
            self.assertEqual(inventory.call_count, 1)
            stored = target / '.secode-admission'
            self.assertTrue(stored.is_file(), 'a complete identity writes its record')
            before = stored.read_bytes()
            # Every missing or blank component refuses, and none of them may
            # touch the stored record or skip the capacity check.
            incomplete = [[123, 'start', ''], [123, '', 'boot'], [123, '   ', 'boot'],
                          [0, 'start', 'boot'], [123, 'start'], 'not-a-list', []]
            for identity in incomplete:
                with self.subTest(identity=identity):
                    with self.assertRaises(ValueError):
                        eviction.prepare(self.toolchain, target, checkout, identity,
                                         proc=self.proc)
                    self.assertEqual(stored.read_bytes(), before,
                                     'a refused identity must not rewrite the record')
                    self.assertEqual(inventory.call_count, 1,
                                     'a refused identity must not reuse the record')

    def test_parent_admission_reuses_only_same_process_and_budget(self):
        target, checkout = self.target('checkout')
        identity = [123, 'start', 'boot']
        with patch.object(eviction,'evict') as inventory:
            eviction.prepare(self.toolchain,target,checkout,identity, proc=self.proc).close()
            eviction.prepare(self.toolchain,target,checkout,identity, proc=self.proc).close()
            self.assertEqual(inventory.call_count,1)
            eviction.prepare(self.toolchain,target,checkout,[123,'later','boot'], proc=self.proc).close()
            self.assertEqual(inventory.call_count,2)
            with patch.object(eviction,'CAP_BYTES', eviction.CAP_BYTES+1):
                eviction.prepare(self.toolchain,target,checkout,[123,'later','boot'], proc=self.proc).close()
            self.assertEqual(inventory.call_count,3)
        with patch.object(eviction,'evict',side_effect=RuntimeError('over budget')):
            with self.assertRaisesRegex(RuntimeError,'over budget'):
                eviction.prepare(self.toolchain,target,checkout,[124,'fresh','boot'], proc=self.proc)
        with patch.object(eviction,'evict') as inventory:
            eviction.prepare(self.toolchain,target,checkout,[124,'fresh','boot'], proc=self.proc).close()
            inventory.assert_called_once()

    def test_inherited_admission_requires_real_external_lease_descriptor(self):
        target, checkout = self.target('checkout')
        # The lease records a process identity, so the fixture supplies one for
        # this process. It is a real entry in the fixture table, not the host's.
        me = str(os.getpid())
        entry = self.proc / me
        (entry / 'task' / me).mkdir(parents=True, exist_ok=True)
        (entry / 'stat').write_bytes(me.encode() + b' (python3) S ' + b'0 ' * 18 + b'66\n')
        (entry / 'comm').write_text('python3\n')
        (entry / 'task' / me / 'children').write_text('')
        with patch.dict(os.environ,{},clear=False):
            os.environ['SECODE_CACHE_ADMITTED_TARGET']=str(target)
            os.environ['SECODE_CACHE_ADMISSION_PROCESS']=json.dumps(eviction.process_identity(os.getpid(), cargo_only=False, proc=self.proc))
            os.environ['SECODE_CACHE_LEASE_FD']='999999'
            self.assertIsNone(eviction.inherited_lease(target, proc=self.proc))
            with (self.directory/'unrelated').open('w') as unrelated:
                os.environ['SECODE_CACHE_LEASE_FD']=str(unrelated.fileno())
                self.assertIsNone(eviction.inherited_lease(target, proc=self.proc))
            held=eviction.register(self.toolchain,target,checkout)
            try:
                eviction.inherit(held,target,proc=self.proc)
                inherited=eviction.inherited_lease(target, proc=self.proc)
                self.assertIsNotNone(inherited)
                origin = os.environ['SECODE_CACHE_ADMISSION_PROCESS']
                expired = json.loads(origin)
                expired[1] = 'different-start-time'
                os.environ['SECODE_CACHE_ADMISSION_PROCESS'] = json.dumps(expired)
                self.assertIsNone(eviction.inherited_lease(target, proc=self.proc))
                os.environ['SECODE_CACHE_ADMISSION_PROCESS'] = origin
                held.close()
                with self.assertRaises(BlockingIOError):
                    eviction.lease(target)
                inherited.close()
            finally:
                held.close()


    def test_release_does_not_waive_the_temporary_recent_target_protection(self):
        current, _ = self.target('current')
        target, owner = self.target('released', age=0)
        # This is the exact CLI ordering that previously refreshed the grace period.
        held = eviction.register(self.toolchain, target, owner)
        eviction.retain(target, owner, retained=False)
        held.close()
        cap = eviction.usage(self.cache, time.monotonic()+5) - 8192
        self.assertEqual(self.evict(current, cap)[0], [])
        self.assertTrue(target.exists())
        self.assertTrue(owner.exists())

    def test_new_build_and_post_release_write_restore_recent_protection(self):
        current, _ = self.target('current')
        target, owner = self.target('released')
        eviction.retain(target, owner, retained=False)
        held = eviction.register(self.toolchain, target, owner)
        held.close()
        self.assertFalse((target/eviction.RELEASED).exists())
        self.assertEqual(self.pressure(current)[0], [])
        eviction.retain(target, owner, retained=False)
        timestamp = (target/eviction.RELEASED).stat().st_mtime + 1
        os.utime(target/'debug/artifact', (timestamp,timestamp))
        self.assertEqual(self.pressure(current)[0], [])
        self.assertTrue(target.exists())

    def test_release_does_not_bypass_live_lease_or_unknown_content(self):
        current, _ = self.target('current')
        target, owner = self.target('released')
        eviction.retain(target, owner, retained=False)
        with eviction.lease(target, shared=True):
            self.assertEqual(self.pressure(current)[0], [])
        (target/'keep-evidence').write_text('keep')
        self.assertEqual(self.pressure(current)[0], [])

    def test_cache_census_has_bounded_fifteen_minute_io_window(self):
        target, owner = self.target('current')
        def census(cache, current, deadline, **options):
            self.assertGreater(deadline-time.monotonic(), 60)
            self.assertLessEqual(deadline-time.monotonic(), 900)
        with patch.object(eviction,'evict',side_effect=census):
            eviction.prepare(self.toolchain, target, owner, proc=self.proc).close()


if __name__ == '__main__':
    unittest.main()
