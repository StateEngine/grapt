"""Resource admission, independent of the workstation's changing load."""
import json
from pathlib import Path
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest.mock import patch
from heavy_queue import Queue
import heavy_hardware as hardware


class HardwarePolicyTests(unittest.TestCase):
    def host(self, **changes):
        return dict(available=64*hardware.GIB, cores=48, load=10,
                    nvme_free=200*hardware.GIB, root_free=80*hardware.GIB, **changes)

    def test_a_fifth_job_fits_hardware_without_a_fixed_four_slot_limit(self):
        peers = [dict(peak_rss=8*hardware.GIB, rss=6*hardware.GIB, cpu=4) for _ in range(4)]
        candidate = dict(peak_rss=8*hardware.GIB, cpu=8)
        self.assertIsNone(hardware.refusal(self.host(), candidate, peers, 4))
        self.assertEqual(hardware.refusal(self.host(), candidate, peers, 12), 'safety ceiling')

    def test_memory_reserve_and_unmaterialized_growth_are_required(self):
        host = self.host(); host['available'] = 23*hardware.GIB
        candidate = dict(peak_rss=8*hardware.GIB, cpu=8)
        self.assertEqual(hardware.refusal(host, candidate, [], 0), 'available RAM and projected growth')
        host['available'] = 24*hardware.GIB
        self.assertIsNone(hardware.refusal(host, candidate, [], 0))
        peer = dict(peak_rss=24*hardware.GIB, rss=20*hardware.GIB, cpu=1)
        self.assertEqual(hardware.refusal(host, candidate, [peer], 1), 'available RAM and projected growth')
        host['available'] = 28*hardware.GIB
        # The governor's existing 20 GiB is already reflected in MemAvailable.
        self.assertIsNone(hardware.refusal(host, candidate, [peer], 1))

    def test_cpu_load_and_both_disk_floors_are_independent_guards(self):
        candidate = dict(peak_rss=hardware.GIB, cpu=8)
        peers = [dict(peak_rss=hardware.GIB, rss=hardware.GIB, cpu=9.1) for _ in range(4)]
        self.assertEqual(hardware.refusal(self.host(), candidate, peers, 4), 'projected CPU')
        busy = self.host(); busy['load'] = 100
        self.assertIsNone(hardware.refusal(busy, candidate, peers, 4, weighted_cpu=True))
        self.assertEqual(hardware.refusal(busy, candidate, peers, 12, weighted_cpu=True), 'safety ceiling')
        busy['available'] = hardware.GIB
        self.assertEqual(hardware.refusal(busy, candidate, peers, 4, weighted_cpu=True), 'available RAM and projected growth')
        for key, value, reason in [('load', 37, 'load guard'),
                                   ('nvme_free', hardware.DISK_FLOOR-1, 'disk free-space floor'),
                                   ('root_free', hardware.DISK_FLOOR-1, 'disk free-space floor')]:
            host=self.host();host[key]=value
            self.assertEqual(hardware.refusal(host,candidate,[],0),reason)
            if key.endswith('_free'):
                self.assertEqual(hardware.refusal(host,candidate,[],0,weighted_cpu=True),reason)
        host=self.host();host['available']=float('nan')
        with self.assertRaisesRegex(ValueError,'hardware sample'):
            hardware.refusal(host,candidate,[],0)

    def test_acquire_timeout_reports_real_load_ram_and_disk_refusals(self):
        cases = [
            ('load-guard', {'load': 45.0, 'cores': 48}, ('blocked by load guard', 'load=45.00', 'cpu_limit=44')),
            ('ram', {'available': 1 * hardware.GIB}, ('blocked by available RAM and projected growth', 'avail=1.0GiB')),
            ('disk', {'nvme_free': 1 * hardware.GIB}, ('blocked by disk free-space floor', 'nvme_free=1.0GiB')),
        ]
        for label, overrides, expected_fragments in cases:
            with self.subTest(blocker=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                queue = Queue(root / 'queue', root / 'heavy.lock')
                sample = self.host()
                sample.update(overrides)
                ticket, notification = queue.enqueue(1, f'timeout-{label}', time.monotonic() + 5, jobs=3)
                try:
                    with (root / 'heavy.lock').open('a') as handle, \
                            patch('heavy_sampling.host', return_value=sample), \
                            patch('heavy_sampling.processes', return_value={}), \
                            patch('disk_stall.observe'):
                        with self.assertRaises(TimeoutError) as caught:
                            queue.acquire(ticket, notification, handle, time.monotonic() + 0.02, 0.02)
                    message = str(caught.exception)
                    for fragment in expected_fragments:
                        self.assertIn(fragment, message)
                finally:
                    notification.close()
                    queue.remove(ticket)

    def test_raw_flock_timeout_does_not_report_cleared_earlier_load_refusal(self):
        import fcntl
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            queue = Queue(root / 'queue', root / 'heavy.lock')
            high_load = self.host()
            high_load.update({'load': 45.0, 'cores': 48})
            healthy = self.host()
            # First sample triggers load guard (setting last_blocker); subsequent
            # samples are healthy while a raw exclusive flock holds heavy.lock.
            samples = iter([high_load, healthy, healthy, healthy])
            calls = []
            def next_sample():
                value = next(samples, healthy)
                calls.append(value['load'])
                return value
            ticket, notification = queue.enqueue(1, 'raw-flock-after-load', time.monotonic() + 5, jobs=3)
            try:
                with (root / 'heavy.lock').open('a') as raw_holder, \
                        (root / 'heavy.lock').open('a') as handle:
                    fcntl.flock(raw_holder, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    try:
                        with patch('heavy_sampling.host', side_effect=next_sample), \
                                patch('heavy_sampling.processes', return_value={}), \
                                patch('external_holders.measure', return_value=(None, None)), \
                                patch('disk_stall.observe'):
                            with self.assertRaises(TimeoutError) as caught:
                                queue.acquire(ticket, notification, handle, time.monotonic() + 0.25, 0.25)
                    finally:
                        fcntl.flock(raw_holder, fcntl.LOCK_UN)
                self.assertGreaterEqual(len(calls), 2, f'expected >=2 samples, got {calls}')
                message = str(caught.exception)
                self.assertNotIn('load guard', message)
            finally:
                notification.close()
                queue.remove(ticket)

    def test_measured_pressure_blocks_new_work_even_when_profiles_and_free_memory_fit(self):
        candidate = dict(peak_rss=hardware.GIB, cpu=1)
        host = self.host();host['pressure'] = dict(cpu=0, memory=0, io=0)
        self.assertIsNone(hardware.refusal(host, candidate, [], 0, weighted_cpu=True))
        for resource, value in (('cpu', 90), ('memory', 20), ('io', 80)):
            with self.subTest(resource=resource):
                host['pressure'] = dict(cpu=0, memory=0, io=0)
                host['pressure'][resource] = value
                self.assertEqual(hardware.refusal(host, candidate, [], 0, weighted_cpu=True),
                                 f'measured {resource} pressure')
        import heavy_sampling
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            for name in ('cpu','memory','io'):
                (root/name).write_text('some avg10=2.50 avg60=0 avg300=0 total=1\nfull avg10=0.50 avg60=0 avg300=0 total=1\n')
            self.assertEqual(heavy_sampling.pressure(root), dict(cpu=2.5, memory=0.5, io=0.5))
            (root/'memory').write_text('full avg10=nan\n')
            with self.assertRaisesRegex(ValueError,'pressure'):
                heavy_sampling.pressure(root)
            (root/'memory').unlink()
            with self.assertRaises(FileNotFoundError):
                heavy_sampling.pressure(root)

    def test_cleanup_can_recover_disk_floor_but_retains_ram_and_cpu_guards(self):
        host=self.host();host['root_free']=1
        candidate=dict(peak_rss=hardware.GIB,cpu=1,maintenance=True)
        self.assertIsNone(hardware.refusal(host,candidate,[],0))
        host['available']=hardware.GIB
        self.assertEqual(hardware.refusal(host,candidate,[],0),'available RAM and projected growth')

    def test_legacy_nested_ram_keeps_its_ceiling_without_unverified_rss_credit(self):
        with tempfile.TemporaryDirectory() as directory:
            ticket = dict(id='1-1', owner='nested-nvme-gate')
            allocation = dict(slot='disk', budget=0, path=None, ram_row=None, targets=['ram'])
            profiles = hardware.Profiles(Path(directory), [ticket],
                                         SimpleNamespace(active={'1-1': allocation}, recent=[]))
            # Older telemetry only observed the process family, excluding tmpfs.
            profiles.value['active']['1-1'] = dict(rss=8*hardware.GIB, peak_rss=8*hardware.GIB, cpu=1)
            peer, = profiles.peers([ticket], 1)
            self.assertEqual(peer['peak_rss'], 24*hardware.GIB)
            self.assertEqual(peer['rss'], 0)
            host = self.host(); host['available'] = 40*hardware.GIB
            self.assertEqual(hardware.refusal(host, dict(peak_rss=8*hardware.GIB, cpu=1), [peer], 1),
                             'available RAM and projected growth')
            allocation['targets'] = ['disk']
            peer, = profiles.peers([ticket], 1)
            self.assertEqual(peer['peak_rss'], 8*hardware.GIB)
            self.assertEqual(peer['rss'], 8*hardware.GIB)

    def test_nested_ram_observation_does_not_record_process_rss_as_governed_pages(self):
        with tempfile.TemporaryDirectory() as directory:
            ticket = dict(id='1-1', owner='nested-nvme-gate', pid=1, start='boot:7')
            allocation = dict(slot='disk', targets=['ram'])
            runs = SimpleNamespace(active={'1-1': allocation}, recent=[], holders=lambda _: [ticket])
            profiles = hardware.Profiles(Path(directory), [ticket], runs)
            forest = {1: dict(start='7', parent=0, rss=8*hardware.GIB, ticks=10)}
            # Retained metadata can contain the previous boot's longer uptime.
            profiles.value['observed'] = 1000
            with patch.object(hardware.heavy_sampling, 'processes', return_value=forest), \
                    patch.object(hardware.time, 'monotonic', return_value=100):
                profiles.observe()
            self.assertEqual(profiles.value['active']['1-1']['rss'], 0)
            self.assertEqual(profiles.value['active']['1-1']['ticks'], 10)

    def test_profiles_learn_owner_then_kind_without_learning_from_incomplete_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            ticket=dict(id='1-1',owner='seat-full', **{'class':1})
            runs=SimpleNamespace(recent=[],active={})
            profiles=hardware.Profiles(Path(directory),[ticket],runs)
            profiles.register(ticket,8,None)
            def row(identity,owner,rss,cpu,code=0):
                return dict(id=identity,owner=owner,rss=0,peak_rss=rss,ticks=0,cpu=0,
                    peak_cpu=cpu,at=0,since=0,holders=1,seconds=None,exit=code)
            profiles.value['recent']=[row('2-2','other-full',4*hardware.GIB,2)]
            runs.recent=[dict(id='2-2',exit=0)]
            self.assertEqual(profiles.projection(ticket)['peak_rss'],5*hardware.GIB)
            profiles.value['recent'].append(row('3-3','seat-full',hardware.GIB,1))
            runs.recent.append(dict(id='3-3',exit=0))
            self.assertEqual(profiles.projection(ticket)['peak_rss'],1.25*hardware.GIB)
            profiles.value['recent'].append(row('4-4','seat-full',10*hardware.GIB,6,1))
            self.assertEqual(profiles.projection(ticket)['peak_rss'],12.5*hardware.GIB)

    def test_slow_test_execution_does_not_change_the_hardware_ceiling(self):
        for duration, expected in [(125,12),(126,12)]:
            with self.subTest(duration=duration), tempfile.TemporaryDirectory() as directory:
                root=Path(directory);ticket=dict(id='9-9',owner='seat-full', **{'class':1})
                profiles=hardware.Profiles(root,[ticket],SimpleNamespace(recent=[],active={}))
                log=root/'full.log';profiles.register(ticket,8,log)
                def row(identity):
                    return dict(id=identity,owner='seat-full',rss=0,peak_rss=hardware.GIB,
                        ticks=0,cpu=1,peak_cpu=2,at=0,since=1,holders=5,seconds=100,exit=0)
                profiles.value['recent']=[row(f'{i}-{i}') for i in range(3)]
                profiles.value['active'][ticket['id']]=row(ticket['id'])
                source=dict(commit='a'*40,clean=True)
                Path(str(log)+'.receipt.json').write_text(json.dumps(dict(schema='secode.gate.v1',mode='full',exit=0,
                    seconds=duration,finished_unix=200,source_before=source,source_after=source)))
                profiles.finish(ticket,0)
                self.assertEqual(hardware.ceiling(root),expected)

    def test_legacy_timing_backoff_does_not_lower_the_pressure_based_ceiling(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            hardware.set_ceiling(root, 12, 'ordinary ceiling')
            hardware.write(root/'hardware-ceiling.json', {'version':1,'limit':4,'reason':'old timing backoff'})
            self.assertEqual(hardware.ceiling(root), 12)
            self.assertEqual(hardware.ceiling(root, legacy=True), 4)

    def test_ram_current_and_process_family_measurement_include_only_their_owners(self):
        import heavy_sampling
        forest={1:dict(parent=0,rss=100,ticks=20),2:dict(parent=1,rss=200,ticks=30),
                3:dict(parent=0,rss=900,ticks=900)}
        self.assertEqual(heavy_sampling.family(1,forest),dict(rss=300,ticks=50))
        import cpu_priority
        with tempfile.TemporaryDirectory() as directory, patch.object(cpu_priority, 'CGROUP', Path(directory)):
            root = Path(directory)
            unit = 'secode-ram-run-private.scope'
            allocation = dict(slot='ram', budget=1024, ram_row=dict(unit=unit))
            self.assertIsNone(heavy_sampling.ram_current(allocation))
            for slice_name in ('app.slice', cpu_priority.SLICE):
                group = root / slice_name / unit
                group.mkdir(parents=True)
                (group / 'memory.max').write_text('1024')
                (group / 'memory.current').write_text('512')
                self.assertEqual(heavy_sampling.ram_current(allocation), 512)
                (group / 'memory.max').unlink()
                with self.assertRaisesRegex(ValueError, 'incomplete'):
                    heavy_sampling.ram_current(allocation)
                (group / 'memory.current').unlink()
                group.rmdir()
            run = root / 'run-private'
            run.mkdir(mode=0o700)
            row = dict(unit=unit, budget=1024, checkout=str(root),
                       owner=[1, '1', '00000000-0000-0000-0000-000000000000'])
            marker = run / '.secode-ram-run'
            marker.write_text(json.dumps(row)); marker.chmod(0o600)
            selected = cpu_priority.SLICE[:-6] + '-' + 'a'*32 + '.slice'
            cpu_priority.ram_slice(run, selected)
            group = root / cpu_priority.SLICE / selected / unit
            group.mkdir(parents=True)
            (group / 'memory.max').write_text('1024')
            (group / 'memory.current').write_text('768')
            self.assertEqual(heavy_sampling.ram_current(dict(slot='ram', budget=1024,
                ram_row=row, path=str(run))), 768)
    def test_expired_four_slot_observer_does_not_pin_hardware_admission_to_three(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            queue = Queue(root/'queue', root/'heavy')
            (queue.directory/'capacity.json').write_text(json.dumps(
                {'version': 1, 'limit': 4, 'expires_unix': time.time()-1}))
            self.assertEqual(queue.shared_limit(), 12,
                             'hardware admission must not inherit the dead fixed-slot observer')


if __name__ == '__main__':
    unittest.main()
