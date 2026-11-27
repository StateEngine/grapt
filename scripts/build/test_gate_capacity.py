"""Capacity rollback uses real queue metadata and bounded gate evidence."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import gate_capacity as capacity
from heavy_queue import Queue


class CapacityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.queue = Queue(self.root / 'queue', self.root / 'heavy')
        self.queue.heavy.touch(mode=0o600)

    def test_four_slots_require_a_live_monitor_and_expiry_returns_three(self):
        self.assertEqual(self.queue.legacy_shared_limit(), 3)
        capacity.set_limit(self.queue, 4, 100)
        with patch('heavy_queue.time.time', return_value=101):
            self.assertEqual(self.queue.legacy_shared_limit(), 4)
        with patch('heavy_queue.time.time', return_value=131):
            self.assertEqual(self.queue.legacy_shared_limit(), 3)
        capacity.set_limit(self.queue, 3, 101)
        self.assertEqual(self.queue.legacy_shared_limit(), 3)

    def test_corrupt_capacity_cannot_open_additional_slots(self):
        for row in [{'version': 1, 'limit': 5}, {'version': 1, 'limit': True},
                    {'version': 1, 'limit': 4}, {'version': 1, 'limit': 4, 'expires_unix': float('nan')}]:
            with self.subTest(row=row):
                path = self.queue.directory / 'capacity.json'
                path.write_text(json.dumps(row))
                path.chmod(0o600)
                with self.assertRaises(ValueError):
                    self.queue.legacy_shared_limit()

    def test_six_full_median_and_strict_twenty_five_percent_threshold(self):
        baseline = [{'seconds': 100}, {'seconds': 200}]
        row = {'outcome': 'pass', 'max_holders': 4}
        self.assertIsNone(capacity.verdict(baseline, [{'seconds': 999}] * 5, row))
        self.assertEqual(capacity.verdict(baseline, [{'seconds': 187.5}] * 6, row)['action'], 'keep-four')
        result = capacity.verdict(baseline, [{'seconds': 188}] * 6, row)
        self.assertEqual(result['action'], 'rollback')
        self.assertEqual(result['baseline_median_seconds'], 150)

    def test_timeout_under_four_rolls_back_before_six_fulls(self):
        row = {'outcome': 'timed-out', 'max_holders': 4}
        self.assertEqual(capacity.verdict([{'seconds': 100}], [], row)['action'], 'rollback')
        row['max_holders'] = 3
        self.assertIsNone(capacity.verdict([{'seconds': 100}], [], row))

    def test_unreadable_capacity_inventory_is_unknown_and_timeout_rolls_back(self):
        with patch.object(self.queue, 'shared_locks', side_effect=BlockingIOError):
            self.assertIsNone(capacity.occupancy(self.queue))
        row = {'outcome': 'timed-out', 'max_holders': None, 'uncertain_samples': 1}
        self.assertEqual(capacity.verdict([{'seconds': 100}], [], row)['action'], 'rollback')

    def test_completion_ignores_stale_receipts_and_reads_execution_timeout(self):
        log = self.root / 'gate.log'
        diagnostic = self.root / 'driver.log'
        gate = dict(id='one', mode='full', log=str(log), diagnostics=str(diagnostic),
                    started_monotonic=10, started_unix=100, max_holders=4, execution_limit=20)
        capacity.atomic_json(Path(str(log) + '.receipt.json'),
            {'schema': 'secode.gate.v1', 'mode': 'full', 'seconds': 1, 'finished_unix': 99})
        diagnostic.write_text('FAIL full: Gate timed out or was interrupted; its process group was stopped (20.0s)\n')
        row = capacity.completion(gate, 130, 40)
        self.assertEqual(row['outcome'], 'timed-out')
        self.assertEqual(row['seconds'], 20)
        self.assertEqual(row['duration_source'], 'gate failure')
        gate['execution_limit'] = 10800
        self.assertEqual(capacity.completion(gate, 130, 40)['outcome'], 'failed')

    def test_real_gate_census_filters_private_fixtures_and_deduplicates_ram_child(self):
        proc = self.root / 'proc'
        proc.mkdir()
        (self.root / 'driver.log').write_text('')
        for pid, start in [(123, '100'), (124, '101')]:
            path = proc / str(pid)
            path.mkdir()
            # /proc fields after the comm: state, ppid, then through starttime.
            fields = ['S', '1'] + ['0'] * 17 + [start]
            (path / 'stat').write_text(f'{pid} (python) ' + ' '.join(fields))
            (path / 'cmdline').write_bytes(b'python3\0gate.py\0full\0--log\0gate.log\0')
            claim = {'queue': str(self.queue.directory), 'heavy': str(self.queue.heavy), 'ticket': 'real'}
            (path / 'environ').write_bytes(b'SECODE_HEAVY_LEASE=' + json.dumps(claim).encode() + b'\0SECRET=do-not-record\0')
            (path / 'cwd').symlink_to(self.root, target_is_directory=True)
            (path / 'fd').mkdir()
            (path / 'fd/2').symlink_to(self.root / 'driver.log')
        gates = capacity.discover(self.queue, 100, 10, proc)
        self.assertEqual(len(gates), 1)
        gate = next(iter(gates.values()))
        self.assertEqual(gate['pid'], 123)
        self.assertNotIn('do-not-record', json.dumps(gate))
        for path in proc.iterdir():
            (path / 'environ').write_bytes(b'SECODE_HEAVY_LEASE={"queue":"private"}\0')
        self.assertEqual(capacity.discover(self.queue, 100, 10, proc), {})

    def test_six_slow_fulls_rewrite_admission_and_publish_the_comparison(self):
        directory = self.root / 'observation'
        step = 0

        def census(queue, now, monotonic):
            nonlocal step
            step += 1
            if step == 13:
                self.assertEqual(queue.legacy_shared_limit(), 3)
                raise StopIteration
            if step % 2 == 0:
                return {}
            log = self.root / f'full-{step}.log'
            capacity.atomic_json(Path(str(log) + '.receipt.json'), {
                'schema': 'secode.gate.v1', 'mode': 'full',
                'seconds': 200, 'finished_unix': now})
            return {str(step): dict(id=str(step), pid=step, mode='full', log=str(log),
                    diagnostics=None, started_unix=now, started_monotonic=monotonic,
                    execution_limit=10800)}

        with patch.object(capacity, 'discover', side_effect=census), \
                patch.object(capacity.time, 'sleep'), self.assertRaises(StopIteration):
            capacity.observe(self.queue, directory, [{'seconds': 100}], 1)
        self.assertEqual(self.queue.legacy_shared_limit(), 3)
        self.assertEqual(capacity.read_json(directory / 'report.json')['action'], 'rollback')
        state = capacity.read_json(directory / 'state.json')
        self.assertEqual(len(state['fulls']), 6)
        self.assertTrue(state['rolled_back'])

    def test_reused_driver_timeout_cannot_become_a_current_short_timeout(self):
        diagnostic = self.root / 'driver.log'
        diagnostic.write_text('FAIL full: Gate timed out (10800.0s)\n')
        info = diagnostic.stat()
        gate = dict(id='new', mode='full', log=str(self.root / 'new.log'),
                    diagnostics=str(diagnostic), diagnostics_offset=info.st_size,
                    diagnostics_identity=[info.st_dev, info.st_ino], execution_limit=10800,
                    started_monotonic=10, started_unix=100, max_holders=4)
        row = capacity.completion(gate, 101, 11)
        self.assertEqual(row['seconds'], 1)
        self.assertEqual(row['outcome'], 'unknown')
        with diagnostic.open('a') as stream:
            stream.write('FAIL full: Gate timed out or was interrupted (1.0s)\n')
        row = capacity.completion(gate, 101, 11)
        self.assertEqual(row['outcome'], 'failed')
        self.assertIsNone(capacity.verdict([{'seconds': 100}], [], row))
        gate.pop('diagnostics_offset')
        gate['diagnostics'] = str(self.root / 'only-old.log')
        Path(gate['diagnostics']).write_text('FAIL full: Gate timed out (10800.0s)\n')
        old = Path(gate['diagnostics']).stat()
        gate['diagnostics_identity'] = [old.st_dev, old.st_ino]
        self.assertEqual(capacity.completion(gate, 101, 11)['outcome'], 'unknown')

    def test_evidence_fifo_refuses_without_waiting_for_a_partner(self):
        fifo = self.root / 'diagnostics.fifo'
        os.mkfifo(fifo, 0o600)
        for method in ['tail', 'read_json']:
            script = ('from pathlib import Path\nimport gate_capacity\n'
                      'try:\n'
                      f' gate_capacity.{method}(Path({str(fifo)!r}))\n'
                      'except ValueError:\n print("refused")\n'
                      'else:\n raise AssertionError("nonregular evidence accepted")\n')
            result = subprocess.run([sys.executable, '-B', '-c', script],
                cwd=Path(capacity.__file__).parent, capture_output=True, text=True, timeout=3)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), 'refused')
        absent = self.root / 'absent'
        self.assertEqual(capacity.tail(absent), '')
        with self.assertRaises(FileNotFoundError):
            capacity.read_json(absent)
        with patch.object(capacity.os, 'open', side_effect=PermissionError('unavailable')):
            for method in [capacity.tail, capacity.read_json]:
                with self.assertRaises(PermissionError):
                    method(self.root / 'unreadable')

    def test_kernel_four_slot_timeout_actually_restores_three(self):
        directory = self.root / 'timeout-observation'
        step = 0

        def census(queue, now, monotonic):
            nonlocal step
            step += 1
            if step == 3:
                self.assertEqual(queue.legacy_shared_limit(), 3)
                raise StopIteration
            if step == 2:
                return {}
            diagnostic = self.root / 'timeout.log'
            diagnostic.write_text('FAIL full: Gate timed out (20.0s)\n')
            return {'timeout': dict(id='timeout', pid=1, mode='full',
                log=str(self.root / 'gate.log'), diagnostics=str(diagnostic),
                execution_limit=20, started_unix=now, started_monotonic=monotonic - 20)}

        with patch.object(capacity, 'discover', side_effect=census), \
                patch.object(self.queue, 'shared_locks', return_value=4), \
                patch.object(capacity.time, 'sleep'), self.assertRaises(StopIteration):
            capacity.observe(self.queue, directory, [{'seconds': 100}], 1)
        self.assertEqual(capacity.read_json(directory / 'report.json')['action'], 'rollback')
        record = json.loads((directory / 'gates.jsonl').read_text())
        self.assertEqual(record['max_holders'], 4)

    def test_nonregular_completion_evidence_stops_monitor_and_restores_three(self):
        for evidence in ['diagnostics', 'receipt']:
            with self.subTest(evidence=evidence):
                directory = self.root / evidence
                log = self.root / f'{evidence}.log'
                fifo = (self.root / 'driver.fifo' if evidence == 'diagnostics'
                        else Path(str(log) + '.receipt.json'))
                os.mkfifo(fifo, 0o600)
                gate = dict(id='one', pid=1, mode='full', log=str(log),
                    diagnostics=str(fifo) if evidence == 'diagnostics' else None,
                    started_unix=100, started_monotonic=10, execution_limit=20)
                with patch.object(capacity, 'discover', side_effect=[{'one': gate}, {}]), \
                        patch.object(capacity.time, 'sleep'), self.assertRaises(ValueError):
                    capacity.observe(self.queue, directory, [{'seconds': 100}], 1)
                self.assertEqual(self.queue.legacy_shared_limit(), 3)
                self.assertTrue((directory / 'monitor-ended.json').is_file())

    def test_early_failed_full_is_logged_but_cannot_bias_the_median(self):
        directory = self.root / 'early-failure'
        step = 0

        def census(_queue, now, monotonic):
            nonlocal step
            step += 1
            if step == 3:
                raise StopIteration
            if step == 2:
                return {}
            diagnostic = self.root / 'failed.log'
            diagnostic.write_text('FAIL full: test failure (1.0s)\n')
            return {'one': dict(id='one', pid=1, mode='full',
                log=str(self.root / 'gate.log'), diagnostics=str(diagnostic),
                started_unix=now, started_monotonic=monotonic - 1, execution_limit=20)}

        with patch.object(capacity, 'discover', side_effect=census), \
                patch.object(capacity.time, 'sleep'), self.assertRaises(StopIteration):
            capacity.observe(self.queue, directory, [{'seconds': 100}], 1)
        self.assertEqual(capacity.read_json(directory / 'state.json')['fulls'], [])
        self.assertEqual(json.loads((directory / 'gates.jsonl').read_text())['outcome'], 'failed')
