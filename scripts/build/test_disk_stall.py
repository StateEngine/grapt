"""The incident-shaped queue, filesystem guards and bounded disk-stall warning."""
import contextlib
import io
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import disk_stall
import heavy_hardware as hardware
import heavy_sampling
from heavy_queue import Queue
from heavy_runs import Admission
import heavy_runs
import install_schedules
import ram_targets
from fixture_environment import isolated_environment


class DiskStallTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.host = dict(available=64*hardware.GIB, cores=48, load=1,
                         root_free=1, nvme_free=80*hardware.GIB)

    def test_0357_legacy_maintenance_escapes_nine_refused_waiters(self):
        queue = Queue(self.root/'queue', self.root/'heavy')
        arrivals = [queue.enqueue(1, f'seat-{i}', time.monotonic()+5) for i in range(9)]
        maintenance = queue.enqueue(1, 'disk-hygiene', time.monotonic()+5)
        arrivals.append(maintenance)
        try:
            # Recreate an on-disk ticket written by the old healthy-disk client.
            tickets = queue.load()
            for ticket in tickets:
                ticket['class'] = 1
            queue.save(tickets)
            admission = Admission(queue.directory)
            admission.priorities = {ticket['id']: 1 for ticket in tickets}
            admission.save()
            ordinary = dict(peak_rss=hardware.GIB, cpu=1)
            self.assertEqual(hardware.refusal(self.host, ordinary, [], 0), 'disk free-space floor')
            ticket, notification = maintenance
            with patch.object(heavy_sampling, 'host', return_value=self.host), \
                    patch.object(heavy_sampling, 'processes', return_value={}), \
                    queue.heavy.open('a') as handle:
                self.assertEqual(queue.acquire(ticket, notification, handle, time.monotonic()+1, 1), 'disk')
                holders = [row['owner'] for row in queue.snapshot(time.monotonic()+2) if row['holder']]
                self.assertEqual(holders, ['disk-hygiene'])
        finally:
            for ticket, notification in reversed(arrivals):
                queue.remove(ticket, 0); notification.close()

    def test_verified_nonroot_placement_keeps_nvme_floor_and_other_guards(self):
        candidate = dict(peak_rss=hardware.GIB, cpu=1, root_writes=False)
        self.assertIsNone(hardware.refusal(self.host, candidate, [], 0))
        self.assertEqual(hardware.refusal(self.host, dict(candidate, root_writes=True), [], 0), 'disk free-space floor')
        self.host['nvme_free'] = 1
        self.assertEqual(hardware.refusal(self.host, candidate, [], 0), 'disk free-space floor')
        self.host['available'] = 1
        self.assertEqual(hardware.refusal(self.host, candidate, [], 0), 'available RAM and projected growth')

    def test_real_queue_admits_verified_nvme_but_not_a_lost_placement(self):
        for valid in (True, False):
            with self.subTest(valid=valid):
                queue = Queue(self.root/str(valid)/'queue', self.root/str(valid)/'heavy')
                target = self.root/str(valid)/'target'
                with patch.object(heavy_runs, 'nvme_target', return_value=target):
                    ticket, notification = queue.enqueue(1, 'seat-full', time.monotonic()+2, nvme=target)
                try:
                    with patch.object(heavy_runs, 'nvme_target', return_value=target if valid else None), \
                            patch.object(heavy_sampling, 'host', return_value=self.host), \
                            patch.object(heavy_sampling, 'processes', return_value={}), \
                            queue.heavy.open('a') as handle:
                        if valid:
                            self.assertEqual(queue.acquire(ticket, notification, handle, time.monotonic()+1, 1), 'disk')
                        else:
                            with self.assertRaises(TimeoutError):
                                queue.acquire(ticket, notification, handle, time.monotonic()+0.02, 0.02)
                finally:
                    queue.remove(ticket, 0); notification.close()

    def test_timer_runs_maintenance_without_a_heavy_queue_wait(self):
        unit = install_schedules.units(Path('/home/example/nightly'), Path('/home/example'), 'agt-test-actor')['secode-disk-hygiene.service']
        self.assertIn('/disk_hygiene.py', unit)
        self.assertNotIn('/heavy ', unit)
        self.assertIn('IOSchedulingClass=idle', unit)
        self.assertIn('TimeoutStartSec=20min', unit)

    def test_landing_transport_can_admit_from_only_its_copied_helpers(self):
        source = Path(__file__).resolve().parents[2]
        declaration = next(line for line in (source/'scripts/land.ps1').read_text().splitlines()
                           if '$helpers = @(' in line)
        directory = self.root/'transport'; directory.mkdir()
        for name in re.findall(r"'([a-z_]+\.py)'", declaration):
            origin=source/'scripts'/('release' if name=='windows_version_worker.py' else 'build')/name
            shutil.copyfile(origin, directory/name)
        probe = directory/'probe.py'
        probe.write_text('''import sys
from pathlib import Path
from unittest.mock import patch
import time
sys.path.insert(0, str(Path(__file__).parent))
import heavy_sampling
import duplicate_targets
import target_discovery
from heavy_queue import Queue
root=Path(__file__).parent
queue=Queue(root/'queue', root/'heavy.lock')
ticket, notification=queue.enqueue(1, 'fixture', time.monotonic()+2)
host=dict(available=512*1024**3, cores=128, load=0, nvme_free=1024**4, root_free=1024**4)
try:
    with patch.object(heavy_sampling, 'host', return_value=host), patch.object(heavy_sampling, 'processes', return_value={}), queue.heavy.open('a') as held:
        assert queue.acquire(ticket, notification, held, time.monotonic()+2, 2)=='disk'
finally:
    queue.remove(ticket, 0)
    notification.close()
print('isolated landing admission passed')
''')
        result = subprocess.run([sys.executable, '-I', '-B', str(probe)], cwd=directory,
                                env=isolated_environment(), capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        self.assertIn('isolated landing admission passed', result.stdout)

    def test_verified_ram_ignores_root_pressure_but_failed_allocation_cannot_fall_back_to_root(self):
        for succeeds in (True, False):
            with self.subTest(succeeds=succeeds):
                queue = Queue(self.root/str(succeeds)/'queue', self.root/str(succeeds)/'heavy')
                ticket, notification = queue.enqueue(1, 'seat-full', time.monotonic()+2, ram=True)
                reservation = (self.root/'ram-run', {'budget': hardware.GIB})
                reserve = unittest.mock.Mock(return_value=reservation) if succeeds else unittest.mock.Mock(side_effect=ValueError('capacity lost'))
                try:
                    with patch.object(ram_targets, 'capacity'), \
                            patch.object(queue, 'shared_locks', return_value=0), \
                            patch.object(heavy_sampling, 'host', return_value=self.host), \
                            patch.object(heavy_sampling, 'processes', return_value={}), \
                            queue.heavy.open('a') as handle:
                        if succeeds:
                            self.assertEqual(queue.acquire(ticket, notification, handle, time.monotonic()+1, 1, reserve), 'ram')
                        else:
                            with self.assertRaises(TimeoutError):
                                queue.acquire(ticket, notification, handle, time.monotonic()+0.02, 0.02, reserve)
                        self.assertTrue(reserve.called)
                finally:
                    queue.remove(ticket, 0); notification.close()

    def test_warning_is_loud_after_five_minutes_bounded_and_cleared_on_recovery(self):
        ticket = dict(id='1-1', owner='seat-full')
        with contextlib.redirect_stderr(io.StringIO()) as output:
            self.assertIsNone(disk_stall.observe(self.root, ticket, self.host, 'disk free-space floor', 10, 'boot'))
            self.assertIsNone(disk_stall.observe(self.root, ticket, self.host, 'disk free-space floor', 309, 'boot'))
            warning = disk_stall.observe(self.root, ticket, self.host, 'disk free-space floor', 310, 'boot')
            self.assertIn('QUEUE STALLED ON DISK FLOOR for 300s', warning)
            self.assertIsNone(disk_stall.observe(self.root, ticket, self.host, 'disk free-space floor', 311, 'boot'))
            self.assertEqual(output.getvalue().count('QUEUE STALLED'), 1)
            disk_stall.observe(self.root, ticket, self.host, None, 312, 'boot')
            self.assertIsNone(disk_stall.current(self.root, 312, 'boot'))

    def test_reboot_does_not_reuse_a_previous_stall(self):
        disk_stall.observe(self.root, dict(id='1-1', owner='seat'), self.host, 'disk free-space floor', 10, 'old-boot')
        self.assertIsNone(disk_stall.current(self.root, 11, 'new-boot'))

    def test_capacity_alert_precedes_refusal_and_retries_failed_delivery_without_spam(self):
        from types import SimpleNamespace
        import heavy
        cache = self.root/'cache'; cache.mkdir()
        messages = []
        available = [101_000_000_000]
        def space(path):
            return SimpleNamespace(free=available[0] if Path(path) == cache or
                                   available[0] >= disk_stall.CAPACITY_WARNING_BYTES else 52*hardware.GIB)
        def check(now, notify=messages.append):
            return disk_stall.capacity_warning(cache, 'agt-fixture-seat', now, notify)
        with patch.object(heavy, 'QUEUE_PATH', self.root/'queue'), \
                patch.object(heavy, 'HEAVY_LOCK', self.root/'heavy'), \
                patch.object(disk_stall.shutil, 'disk_usage', side_effect=space), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertIsNone(check(1000))
            available[0] = 88*hardware.GIB
            self.assertIsNotNone(check(1001))
            self.assertEqual(hardware.ceiling(self.root/'queue'), 6)
            self.assertEqual(hardware.ceiling(self.root/'queue', legacy=True), 6)
            hardware.set_ceiling(self.root/'queue', 3, 'operator restriction')
            self.assertEqual(len(messages), 1)
            self.assertIsNone(check(1002))
            self.assertEqual(hardware.ceiling(self.root/'queue'), 3)
            self.assertIsNone(check(2800))
            with self.assertRaisesRegex(RuntimeError, 'unavailable'):
                check(2801, lambda _body: (_ for _ in ()).throw(RuntimeError('mail unavailable')))
            self.assertIsNone(check(2802))
            self.assertIsNotNone(check(3101))
            self.assertEqual(len(messages), 2)
            self.assertIn('88.0 GiB', messages[0])
            self.assertIn('52.0 GiB', messages[0])
            units = install_schedules.units(self.root, self.root, 'agt-fixture-seat')
            self.assertIn('--alert-only --agent-id agt-fixture-seat', units['secode-disk-pressure.service'])
            self.assertIn('OnUnitInactiveSec=5min', units['secode-disk-pressure.timer'])

    def capacity_fixture(self, root_free=89*hardware.GIB, nvme_free=300*hardware.GIB):
        from types import SimpleNamespace
        cache = self.root/'capacity'; cache.mkdir()
        available = {'root': root_free, 'nvme': nvme_free}
        messages = []
        def usage(path):
            return SimpleNamespace(free=available['nvme' if Path(path) == cache else 'root'])
        self.enterContext(patch.object(disk_stall.shutil, 'disk_usage', side_effect=usage))
        self.enterContext(patch.object(disk_stall, 'reduce_admission'))
        self.enterContext(contextlib.redirect_stderr(io.StringIO()))
        def check(now, notify=None):
            return disk_stall.capacity_warning(cache, 'agt-fixture-seat', now,
                                              messages.append if notify is None else notify)
        return cache, available, messages, check

    def test_capacity_root_threshold_uses_gib_and_is_strict(self):
        _, available, messages, check = self.capacity_fixture(root_free=80*hardware.GIB)
        self.assertIsNone(check(1000))
        available['root'] -= 1
        self.assertIsNotNone(check(2800))
        self.assertEqual(len(messages), 1)

    def test_capacity_root_change_uses_gib_and_is_strict(self):
        _, available, messages, check = self.capacity_fixture()
        self.assertIsNone(check(1000))
        available['root'] -= 5*hardware.GIB
        self.assertIsNone(check(2800))
        available['root'] -= 1
        self.assertIsNotNone(check(2801))
        self.assertEqual(len(messages), 1)

    def test_capacity_healthy_nvme_change_cannot_borrow_a_low_roots_eligibility(self):
        _, available, messages, check = self.capacity_fixture()
        self.assertIsNone(check(1000))
        available['nvme'] -= 5*hardware.GIB
        self.assertIsNone(check(2800))
        available['nvme'] -= 1
        self.assertIsNone(check(2801))
        self.assertEqual(messages, [])

    def test_capacity_healthy_root_change_cannot_borrow_a_low_nvmes_eligibility(self):
        _, available, messages, check = self.capacity_fixture(
            root_free=300*hardware.GIB, nvme_free=89*hardware.GIB)
        self.assertIsNone(check(1000))
        available['root'] -= 10*hardware.GIB
        self.assertIsNone(check(2800))
        self.assertEqual(messages, [])

    def test_capacity_root_movement_ceiling_is_strict(self):
        _, available, messages, check = self.capacity_fixture(root_free=120*hardware.GIB)
        self.assertIsNone(check(1000))
        available['root'] = 100*hardware.GIB
        self.assertIsNone(check(2800))
        available['root'] -= 1
        self.assertIsNotNone(check(2801))
        self.assertEqual(len(messages), 1)

    def test_capacity_nvme_movement_ceiling_is_strict(self):
        _, available, messages, check = self.capacity_fixture(nvme_free=120*hardware.GIB)
        self.assertIsNone(check(1000))
        available['nvme'] = 100*hardware.GIB
        self.assertIsNone(check(2800))
        available['nvme'] -= 1
        self.assertIsNotNone(check(2801))
        self.assertEqual(len(messages), 1)

    def test_capacity_recovery_above_the_movement_ceiling_is_quiet(self):
        _, available, messages, check = self.capacity_fixture(root_free=70*hardware.GIB)
        self.assertIsNotNone(check(1000))
        available['root'] = 120*hardware.GIB
        self.assertIsNone(check(2799))
        self.assertIsNone(check(2800))
        self.assertIsNone(check(4600))
        self.assertEqual(len(messages), 1)

    def test_capacity_legacy_timestamps_initialize_quietly_and_stay_quiet(self):
        cache, _, messages, check = self.capacity_fixture()
        legacy = {'version': 1, 'attempted': 1000, 'sent': 1000}
        hardware.write(cache/'.disk-pressure.json', legacy)
        for now in (10000, 11800, 13600):
            self.assertIsNone(check(now))
        self.assertEqual(messages, [])
        self.assertEqual(hardware.read(cache/'.disk-pressure.json', None), legacy)
        self.assertIsNotNone(hardware.read(cache/'.disk-pressure-free.json', None))

    def test_capacity_failed_send_retains_the_last_delivered_baseline(self):
        cache, available, messages, check = self.capacity_fixture(root_free=70*hardware.GIB)
        self.assertIsNotNone(check(1000))
        delivered = hardware.read(cache/'.disk-pressure-free.json', None)
        available['root'] -= hardware.GIB
        def unavailable(_body):
            raise RuntimeError('synthetic delivery failure')
        with self.assertRaisesRegex(RuntimeError, 'synthetic delivery failure'):
            check(2800, unavailable)
        self.assertEqual(hardware.read(cache/'.disk-pressure-free.json', None), delivered)
        self.assertEqual(hardware.read(cache/'.disk-pressure.json', None)['sent'], 1000)
        self.assertIsNone(check(3099))
        self.assertIsNotNone(check(3100))
        self.assertEqual(len(messages), 2)
        self.assertEqual(hardware.read(cache/'.disk-pressure-free.json', None)['root_free'],
                         available['root'])
