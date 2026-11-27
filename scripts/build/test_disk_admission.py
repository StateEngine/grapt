"""Capacity refusals preserve builds and remain distinct from failed tests."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from fixture_environment import isolated_environment
import evict_targets as eviction
import test_eviction
import gate


class BudgetTests(unittest.TestCase):
    def setUp(self):
        self.fixture = test_eviction.EvictionTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.cache = self.fixture.cache
        self.proc = self.fixture.proc
        # A fixture entry for this process, so a queue ticket can carry a real
        # identity without the census reading the host's table.
        me = str(os.getpid())
        self_spot = self.proc / me
        (self_spot / 'task' / me).mkdir(parents=True, exist_ok=True)
        (self_spot / 'stat').write_bytes(
            me.encode() + b' (python3) S ' + b'0 ' * 18 + b'55\n')
        (self_spot / 'comm').write_text('python3\n')
        (self_spot / 'task' / me / 'children').write_text('')
        # An unrelated cwd, as the real test process has: the census reads it for
        # a non-build process, so a missing link would blind the whole scan.
        unrelated_cwd = self.fixture.directory / 'unrelated-cwd'
        unrelated_cwd.mkdir(exist_ok=True)
        (self_spot / 'cwd').symlink_to(unrelated_cwd, target_is_directory=True)
        # Every surface the census reads on a readable process must exist: a
        # missing link is not "unrelated", it blinds the census.
        (self_spot / 'exe').symlink_to('/fixture/python3')
        (self_spot / 'environ').write_bytes(b'HOME=/fixture\0')
        (self_spot / 'cmdline').write_bytes(b'python3\0')
        self.required = eviction.FREE_SPACE_FLOOR_BYTES + eviction.BUILD_RESERVE_BYTES

    def disk(self, free):
        return patch.object(eviction.shutil, 'disk_usage', return_value=SimpleNamespace(free=free))

    def evict(self, current):
        with patch.object(eviction, 'live_targets', return_value=set()):
            return eviction.evict(self.cache, current, self.fixture.now, proc=self.proc)

    def test_effective_cap_clamps_to_zero_and_configured_limit(self):
        self.assertEqual(eviction.effective_cap(0, 0, eviction.CAP_BYTES), 0)
        self.assertEqual(eviction.effective_cap(100, self.required - 50, 1000), 50)
        self.assertEqual(eviction.effective_cap(100, self.required + 5000, 1000), 1000)

    def test_zero_cache_still_refuses_below_floor_and_reserve(self):
        self.cache.mkdir()
        with self.disk(self.required - 1), patch.object(eviction, 'usage', return_value=0):
            with self.assertRaises(eviction.DiskAdmissionRefused):
                self.evict(None)

    def test_exact_floor_plus_reserve_is_allowed_and_one_byte_below_refuses(self):
        current, _ = self.fixture.target('current')
        with self.disk(self.required):
            self.assertEqual(self.evict(current)[0], [])
        with self.disk(self.required - 1):
            with self.assertRaisesRegex(eviction.DiskAdmissionRefused, 'not a test failure'):
                self.evict(current)

    def test_real_idle_target_eviction_restores_floor_without_deleting_current_target(self):
        current, _ = self.fixture.target('current')
        idle, owner = self.fixture.target('idle', age=100000)
        deadline = time.monotonic() + 10
        initial = eviction.usage(self.cache, deadline)
        def free(_path):
            reclaimed = initial - eviction.usage(self.cache, deadline)
            return SimpleNamespace(free=self.required - 8192 + reclaimed)
        with patch.object(eviction.shutil, 'disk_usage', side_effect=free):
            removed, _ = self.evict(current)
        self.assertEqual(removed, [idle])
        self.assertTrue(owner.exists())
        self.assertTrue(current.exists())
        self.assertFalse(idle.exists())

    def test_protected_recent_unknown_pinned_leased_and_queued_targets_survive(self):
        current, _ = self.fixture.target('current')
        recent, _ = self.fixture.target('recent', age=60)
        unknown, _ = self.fixture.target('unknown', age=100000)
        (unknown / 'user-data').write_text('keep')
        pinned, owner = self.fixture.target('pinned', age=100000)
        eviction.retain(pinned, owner)
        leased, _ = self.fixture.target('leased', age=100000)
        queued, _ = self.fixture.target('queued', age=100000)
        idle, _ = self.fixture.target('idle', age=100000)
        with eviction.lease(leased, shared=True), self.disk(self.required - 1), \
                patch.object(eviction, 'live_targets', return_value={queued}):
            with self.assertRaises(eviction.DiskAdmissionRefused) as refusal:
                eviction.evict(self.cache, current, self.fixture.now, proc=self.proc)
        self.assertFalse(idle.exists())
        self.assertTrue(all(p.exists() for p in [current, recent, unknown, pinned, leased, queued]))
        message = str(refusal.exception)
        for field in ['DISK-ADMISSION', 'C=', 'F=', 'floor=', 'reserve=', 'whole_cache_bytes_remaining=']:
            self.assertIn(field, message)
        self.assertEqual((unknown / 'user-data').read_text(), 'keep')

    def test_interim_limits_admit_protected_cache_when_free_space_exceeds_floor(self):
        # The first real smoke reclaimed eligible targets but left a protected
        # cache larger than 80 GiB with less than 24 GiB above the floor.
        current, _ = self.fixture.target('protected-current')
        with self.disk(78_955_053_056), patch.object(eviction, 'usage', return_value=91_422_011_392):
            removed, total = self.evict(current)
        self.assertEqual(removed, [])
        self.assertEqual(total, 91_422_011_392)
        self.assertTrue(current.exists())

    def test_new_invocation_rechecks_free_space_despite_previous_process_stamp(self):
        target, owner = self.fixture.target('current')
        old = [123, 'old-start', 'boot']
        with self.disk(self.required + 1024**2), patch.object(eviction, 'live_targets', return_value=set()):
            eviction.prepare(self.fixture.toolchain, target, owner, old, proc=self.proc).close()
        self.assertTrue(json.loads((target / '.secode-admission').read_text())['admitted'])
        # A new Cargo lifetime has a different start identity even if its PID is reused.
        with self.disk(self.required - 1), patch.object(eviction, 'live_targets', return_value=set()):
            with self.assertRaises(eviction.DiskAdmissionRefused):
                eviction.prepare(self.fixture.toolchain, target, owner, [123, 'new-start', 'boot'],
                                  proc=self.proc)
        self.assertFalse(json.loads((target / '.secode-admission').read_text())['admitted'])
        self.assertTrue(target.exists())

    def test_nvme_budget_revalidates_sata_admission_without_deleting_protected_cache(self):
        target, owner = self.fixture.target('current')
        identity = [123, 'same-start', 'boot']
        record = target / '.secode-admission'
        record.write_text(json.dumps({
            'process': identity, 'budget': eviction.CAP_BYTES - 10 * 1024**3, 'admitted': True,
        }))
        with self.disk(1024**4), patch.object(eviction, 'usage', return_value=eviction.CAP_BYTES + 1), \
                patch.object(eviction, 'live_targets', return_value={target}), \
                patch.object(eviction, 'evict', wraps=eviction.evict) as inventory:
            eviction.prepare(self.fixture.toolchain, target, owner, identity, proc=self.proc).close()
            self.assertEqual(json.loads(record.read_text()), {
                'process': identity, 'budget': eviction.CAP_BYTES, 'admitted': True,
            })
            eviction.prepare(self.fixture.toolchain, target, owner, identity, proc=self.proc).close()
            inventory.assert_called_once()
        self.assertTrue((target / 'debug' / 'artifact').is_file())

    def test_gate_rechecks_before_each_cargo_command_and_never_starts_refused_command(self):
        target, owner = self.fixture.target('current')
        binary = self.fixture.directory / 'bin'
        binary.mkdir()
        marker = self.fixture.directory / 'started'
        cargo = binary / 'cargo'
        cargo.write_text(f'#!/bin/sh\nprintf "started\\n" >> "{marker}"\n')
        cargo.chmod(0o700)
        environment = {'SECODE_TARGET_ROOT': str(self.fixture.toolchain),
                       'CARGO_TARGET_DIR': str(target), 'PATH': str(binary) + ':' + os.environ['PATH']}
        with patch.dict(os.environ, environment), patch.object(eviction, 'live_targets', return_value=set()):
            # A protected cache above the cleanup cap must still execute Cargo.
            with self.disk(self.required + 1024**2), \
                    patch.object(eviction, 'usage', return_value=eviction.CAP_BYTES+1), \
                    tempfile.TemporaryFile(mode='w+') as log:
                gate.execute(['cargo', 'check'], log, time.monotonic() + 10, cwd=owner)
            with self.disk(self.required - 1), tempfile.TemporaryFile(mode='w+') as log:
                with self.assertRaises(eviction.DiskAdmissionRefused):
                    gate.execute(['cargo', 'test'], log, time.monotonic() + 10, cwd=owner)
        self.assertEqual(marker.read_text().splitlines(), ['started'])

    def test_ram_admission_checks_original_disk_cache_instead_of_bypassing_budget(self):
        import ram_targets
        target, owner = self.fixture.target('ram-fixture')
        disk = self.fixture.directory / 'disk-cache'
        disk.mkdir()
        with patch.object(ram_targets, 'validate', return_value=True), \
                patch.dict(os.environ, {'SECODE_RAM_DISK_CACHE': str(disk)}), self.disk(self.required - 1), \
                patch.object(eviction, 'live_targets', return_value=set()):
            with self.assertRaises(eviction.DiskAdmissionRefused):
                eviction.prepare(self.fixture.toolchain, target, owner, proc=self.proc)

    def test_capacity_refusal_before_ram_launch_is_not_a_failed_test_or_disk_fallback(self):
        import ram_targets
        log = self.fixture.directory / 'refused.log'
        with patch.dict(os.environ, {'CARGO_TARGET_DIR': '/disk', 'SECODE_TARGET_ROOT': '/disk'}, clear=True), \
                patch.object(eviction, 'prepare', side_effect=eviction.DiskAdmissionRefused('DISK-ADMISSION fixture')), \
                patch.object(ram_targets, 'allocate') as allocate:
            with self.assertRaises(eviction.DiskAdmissionRefused):
                ram_targets.run_gate(['affected'], log)
        allocate.assert_not_called()
        record = json.loads(log.with_suffix('.log.ram.json').read_text())
        self.assertEqual(record['outcome'], 'disk-admission-refused')
        self.assertEqual(record['exit'], 75)
        self.assertIsNone(record['fallback'])

    def test_cli_refusal_is_temporary_capacity_exit_and_never_starts_cargo(self):
        target, owner = self.fixture.target('current')
        private = self.fixture.directory / 'scripts'
        private.mkdir()
        source = Path(eviction.__file__)
        text = source.read_text().replace('FREE_SPACE_FLOOR_BYTES = 60_000_000_000',
                                          'FREE_SPACE_FLOOR_BYTES = 10**30')
        (private / source.name).write_text(text)
        for name in ('ram_targets.py', 'cargo_targets.py', 'cargo_observation.py'):
            shutil.copyfile(source.with_name(name), private / name)
        binary = self.fixture.directory / 'bin'
        binary.mkdir()
        marker = self.fixture.directory / 'cargo-started'
        cargo = binary / 'cargo'
        cargo.write_text(f'#!/bin/sh\ntouch "{marker}"\n')
        cargo.chmod(0o700)
        environment = dict(isolated_environment(), SECODE_TARGET_ROOT=str(self.fixture.toolchain),
                           CARGO_TARGET_DIR=str(target), SECODE_WORKTREE_ROOT=str(owner),
                           PATH=str(binary) + ':' + os.environ['PATH'])
        result = subprocess.run([sys.executable, '-B', str(private / source.name), 'test'],
                                env=environment, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn('DISK-ADMISSION refusal (not a test failure)', result.stderr)
        self.assertFalse(marker.exists())

    def test_unprovable_and_stale_queue_tickets_add_no_protection(self):
        current, owner = self.fixture.target('current')
        queue = self.fixture.directory / 'tickets.json'
        identity = eviction.process_identity(os.getpid(), cargo_only=False, proc=self.proc)
        row = {'pid': identity[0], 'start': f'{identity[2]}:{identity[1]}'}
        queue.write_text(json.dumps({'version': 1, 'tickets': [row]}))
        with patch.object(eviction, 'HEAVY_QUEUE', queue):
            self.assertEqual(eviction.queued_targets({current: owner}, time.monotonic() + 5, self.proc), set())
            row['start'] += '-stale'
            queue.write_text(json.dumps({'version': 1, 'tickets': [row]}))
            self.assertEqual(eviction.queued_targets({current: owner}, time.monotonic() + 5, self.proc), set())

    def test_unprovable_queued_ticket_does_not_prevent_idle_eviction_and_admission(self):
        current, _ = self.fixture.target('current')
        idle, owner = self.fixture.target('idle', age=100000)
        queue = self.fixture.directory / 'tickets.json'
        identity = eviction.process_identity(os.getpid(), cargo_only=False, proc=self.proc)
        queue.write_text(json.dumps({'version': 1, 'tickets': [
            {'pid': identity[0], 'start': f'{identity[2]}:{identity[1]}'}]}))
        # The test process cwd is unrelated to either checkout. Its live ticket
        # cannot identify a future build and must not veto reclaiming idle output.
        deadline = time.monotonic() + 10
        initial = eviction.usage(self.cache, deadline)
        def free(_path):
            reclaimed = initial - eviction.usage(self.cache, deadline)
            return SimpleNamespace(free=self.required - 8192 + reclaimed)
        # An empty proc fixture isolates live-process scanning while retaining
        # the actual queue reader, real PID identity and unrelated cwd.
        proc = self.fixture.directory / 'empty-proc'
        proc.mkdir()
        scan = eviction.live_targets
        def live(owners, proc=None, deadline=None, **options):
            return scan(owners, proc=proc, deadline=deadline, **options)
        with patch.object(eviction, 'HEAVY_QUEUE', queue), \
                patch.object(eviction, 'live_targets', side_effect=live), \
                patch.object(eviction.shutil, 'disk_usage', side_effect=free):
            removed, _ = eviction.evict(self.cache, current, self.fixture.now, proc=self.proc)
        self.assertEqual(removed, [idle])
        self.assertFalse(idle.exists())
        self.assertTrue(current.exists())
        self.assertTrue(owner.exists())

    def test_real_queued_process_checkout_is_protected(self):
        target, owner = self.fixture.target('queued-checkout', age=100000)
        process, _ = self.fixture.cargo_process(owner)
        try:
            # The proof runs over the fixture table: process.pid names the fixture
            # entry, whose identity, cwd and boot id are all injected.
            pid = 3000 + self.fixture.process_count
            entry = self.proc / str(pid)
            if not (entry / 'cwd').exists():
                (entry / 'cwd').symlink_to(owner, target_is_directory=True)
            identity = eviction.process_identity(pid, cargo_only=False, proc=self.proc)
            queue = self.fixture.directory / 'tickets.json'
            unrelated = eviction.process_identity(os.getpid(), cargo_only=False, proc=self.proc)
            queue.write_text(json.dumps({'version': 1, 'tickets': [
                {'pid': unrelated[0], 'start': f'{unrelated[2]}:{unrelated[1]}'},
                {'pid': 'invalid'},
                {'pid': pid, 'start': f'{identity[2]}:{identity[1]}'}]}))
            with patch.object(eviction, 'HEAVY_QUEUE', queue):
                self.assertEqual(eviction.queued_targets({target: owner}, time.monotonic() + 5,
                                                         self.proc), {target})
        finally:
            process.terminate()

    def test_queue_symlinks_and_fifos_add_no_protection_without_following_or_blocking(self):
        real = self.fixture.directory / 'real.json'
        real.write_text(json.dumps({'version': 1, 'tickets': []}))
        queue = self.fixture.directory / 'queue'
        queue.symlink_to(real)
        with patch.object(eviction, 'HEAVY_QUEUE', queue):
            self.assertEqual(eviction.queued_targets({}, time.monotonic() + 5, self.proc), set())
        queue.unlink()
        os.mkfifo(queue)
        program = ('import sys,time; from pathlib import Path; import evict_targets as e; '
                   'e.HEAVY_QUEUE=Path(sys.argv[1]); print(e.queued_targets({},time.monotonic()+2,Path("/proc")))')
        result = subprocess.run([sys.executable, '-B', '-c', program, str(queue)],
                                cwd=Path(eviction.__file__).parent, capture_output=True,
                                text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), 'set()')
