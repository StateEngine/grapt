"""Exercise public exporter failure, sampling and publication boundaries."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

import fleet_metrics as fleet
import install
import receipt
from unittest import mock


class FleetMetrics(unittest.TestCase):
    def test_gate_summary_counts_only_successful_outcomes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, code, status in [('pass', 0, 'PASS_WITH_RETRY'),
                                       ('failed', 101, 'FAIL'), ('invalid', 0, 'FAIL')]:
                fleet.publish(root / (name + '.receipt.json'),
                              {'schema': receipt.SCHEMA, 'exit': code, 'status': status,
                               'finished_unix': 200, 'seconds': 10, 'mode': 'full',
                               'retries': ['isolated retry']})
            observed = fleet.gates(root, 300)
            self.assertEqual(len(observed['recent']), 1)
            self.assertEqual(observed['flaky_retries'], 1)

    def test_queue_observation_never_rewrites_scheduler_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, value in {
                'tickets.json': {'version': 1, 'tickets': []},
                'runs.json': {'version': 1, 'active': {}, 'recent': []},
                'hardware-runs.json': {'version': 1, 'clients': {}, 'active': {},
                                       'recent': [], 'observed': 1},
            }.items():
                (root / name).write_text(json.dumps(value))
            (root / 'mutex').touch()
            before = {p.name: (p.read_bytes(), p.stat().st_mtime_ns) for p in root.iterdir()}
            observed = fleet.heavy(root, Path(__file__).resolve().parents[1] / 'build')
            self.assertEqual(observed['depth'], 0)
            self.assertEqual(observed['running'], 0)
            self.assertEqual(before, {p.name: (p.read_bytes(), p.stat().st_mtime_ns)
                                      for p in root.iterdir()})

    def test_publication_preserves_previous_snapshot_when_encoding_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'fleet.json'
            fleet.publish(destination, {'schema': 'first'})
            with self.assertRaises(ValueError):
                fleet.publish(destination, {'invalid': float('nan')})
            self.assertEqual(json.loads(destination.read_text()), {'schema': 'first'})
            self.assertEqual(list(Path(directory).iterdir()), [destination])

    def test_probes_refuse_output_overflow_timeout_and_missing_sources(self):
        with self.assertRaises(ValueError):
            fleet.command([sys.executable, '-c', "print('x' * 1048577)"], timeout=3)
        with self.assertRaises(TimeoutError):
            fleet.command([sys.executable, '-c', 'import time; time.sleep(10)'], timeout=.1)
        missing = fleet.observe(lambda: fleet.read('/nonexistent/grapt-metric'))
        self.assertEqual(missing['state'], 'unavailable')
        self.assertIsNone(missing['value'])

    def test_idle_spans_are_sampled_and_reset_after_a_gap(self):
        source = {'coverage': 'observed', 'observed_at': '2026-01-01T00:00:00Z',
                  'per_seat': {'seat': {'state': 'idle'}}}
        now = 1767225600
        first = fleet.seats(source, now, None)
        previous = {'observed_unix': now, 'seats': {'value': first}}
        source['observed_at'] = '2026-01-01T00:05:00Z'
        second = fleet.seats(source, now + 300, previous)['rows'][0]
        self.assertEqual(second['sampled_idle_seconds'], 300)
        self.assertIsNone(second['idle_seconds'])
        source['observed_at'] = '2026-01-01T00:15:00Z'
        after_gap = fleet.seats(source, now + 900, previous)['rows'][0]
        self.assertEqual(after_gap['sampled_idle_seconds'], 0)
        with self.assertRaises(ValueError):
            fleet.seats(source, now + 1600, previous)

    def test_installed_service_has_a_stable_exporter_path(self):
        specs = install.units(Path('/home/example'))
        self.assertIn('/home/example/.local/lib/grapt-metrics/fleet_metrics.py',
                      specs['secode-fleet-metrics.service'])
        self.assertIn('OnCalendar=*-*-* *:0/5:00', specs['secode-fleet-metrics.timer'])
        with self.assertRaises(ValueError):
            install.units(Path('/home/example\nExecStart=bad'))

    def test_data_root_prefers_renamed_home_and_falls_back_to_legacy(self):
        for existing, expected in [(('grapt', 'secode'), 'grapt'),
                                   (('secode',), 'secode'), ((), 'grapt')]:
            with tempfile.TemporaryDirectory() as directory:
                home = Path(directory) / 'home'
                base = home / '.local' / 'share'
                for name in existing:
                    (base / name).mkdir(parents=True)
                with mock.patch.object(Path, 'home', return_value=home), \
                        mock.patch.dict(os.environ, {}, clear=True):
                    self.assertEqual(fleet.data_root(), base / expected)

    def test_explicit_data_root_preserves_empty_relative_and_absolute_overrides(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory) / 'home'
            for value in ['', 'relative-root', str(Path(directory) / 'explicit root')]:
                with self.subTest(value=value), \
                        mock.patch.dict(os.environ, {'SECODE_DATA_ROOT': value}, clear=True), \
                        mock.patch.object(Path, 'home', return_value=home), \
                        mock.patch.object(sys, 'argv', ['fleet-metrics']), \
                        mock.patch.object(fleet, 'collect', return_value={}), \
                        mock.patch.object(fleet, 'publish') as publish:
                    fleet.main()
                    publish.assert_called_once_with(Path(value) / 'metrics/fleet.json', {})


if __name__ == '__main__':
    unittest.main()
