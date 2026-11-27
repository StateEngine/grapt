"""Reject false responsiveness passes for spun idles, slow echoes and lost tails."""
import unittest

from r_responsive_walk import (ECHO_PROBES, composer_content, evaluate_clear,
                                 evaluate_echo, probe_timing, verify_evidence)


def evidence():
    return {
        'source': 'db7457354a4c890f080892feb861c1bf863f0d7a',
        'fixture_errors': [],
        'model_completed': [['stream', None]],
        'idle': {'wall_seconds': 30.1, 'cpu_percent_one_core': 0.0,
                 'thread_cpu_seconds': {}},
        'echo': {'latencies_ms': [42.0] * ECHO_PROBES},
        'navigation': {'open_ms': 40.0, 'close_ms': 35.0},
        'stream': {'wall_seconds': 6.3, 'cpu_percent_one_core': 1.7,
                   'thread_cpu_seconds': {}, 'outcome': 'succeeded',
                   'last_line_visible': True},
    }


class RResponsiveEvidenceTests(unittest.TestCase):
    def test_healthy_evidence_is_accepted(self):
        verify_evidence(evidence())

    def test_spun_idle_cannot_pass_the_spin_guard(self):
        bad = evidence()
        bad['idle']['cpu_percent_one_core'] = 98.5
        with self.assertRaisesRegex(RuntimeError, 'spin guard'):
            verify_evidence(bad)

    def test_short_idle_window_cannot_pass(self):
        bad = evidence()
        bad['idle']['wall_seconds'] = 2.0
        with self.assertRaisesRegex(RuntimeError, 'shorter than the admitted bound'):
            verify_evidence(bad)

    def test_slow_echo_cannot_pass_the_linux_guard(self):
        bad = evidence()
        bad['echo']['latencies_ms'][-1] = 400.0
        with self.assertRaisesRegex(RuntimeError, 'echo p95'):
            verify_evidence(bad)

    def test_missing_echo_probe_cannot_pass(self):
        bad = evidence()
        bad['echo']['latencies_ms'] = bad['echo']['latencies_ms'][:-1]
        with self.assertRaisesRegex(RuntimeError, 'probe count'):
            verify_evidence(bad)

    def test_unsucceeded_stream_cannot_pass(self):
        bad = evidence()
        bad['stream']['outcome'] = 'failed-or-interrupted'
        with self.assertRaisesRegex(RuntimeError, 'did not succeed'):
            verify_evidence(bad)

    def test_lost_stream_tail_cannot_pass(self):
        bad = evidence()
        bad['stream']['last_line_visible'] = False
        with self.assertRaisesRegex(RuntimeError, 'missing from the terminal'):
            verify_evidence(bad)

    def test_hot_stream_cannot_pass_the_burst_smoke_guard(self):
        bad = evidence()
        bad['stream']['cpu_percent_one_core'] = 7.5
        with self.assertRaisesRegex(RuntimeError, 'burst-smoke guard'):
            verify_evidence(bad)

    def test_vacuous_stream_window_cannot_pass(self):
        bad = evidence()
        bad['stream']['wall_seconds'] = 0.5
        with self.assertRaisesRegex(RuntimeError, 'vacuous'):
            verify_evidence(bad)

    def test_fixture_error_cannot_hide_behind_passing_metrics(self):
        bad = evidence()
        bad['fixture_errors'] = ['model call bound']
        with self.assertRaisesRegex(RuntimeError, 'fixture provider'):
            verify_evidence(bad)

    def test_extra_model_action_cannot_pass(self):
        bad = evidence()
        bad['model_completed'] = [['stream', None], ['stream', None]]
        with self.assertRaisesRegex(RuntimeError, 'model actions'):
            verify_evidence(bad)


class RResponsiveFrameTests(unittest.TestCase):
    def test_empty_composer_frame_reads_as_empty(self):
        frame = '\u2502> ' + ' ' * 18 + '\u2502'
        self.assertEqual(composer_content(frame), '')

    def test_box_borders_are_not_composer_content(self):
        frame = '\u2502> z' + ' ' * 17 + '\u2502'
        self.assertEqual(composer_content(frame), 'z')

    def test_missing_composer_line_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, 'composer line missing'):
            composer_content('no composer here')


class RResponsiveEchoProbeTests(unittest.TestCase):
    def test_stale_composer_cannot_be_accepted_for_the_alternating_probe(self):
        with self.assertRaisesRegex(RuntimeError, 'composer not empty before'):
            evaluate_echo('az', ['az'], 'z')

    def test_stale_other_character_cannot_pass_the_exact_match(self):
        with self.assertRaisesRegex(RuntimeError, 'did not reach the composer'):
            evaluate_echo('', ['a', 'a'], 'z')

    def test_dropped_input_cannot_pass_the_probe(self):
        with self.assertRaisesRegex(RuntimeError, 'did not reach the composer'):
            evaluate_echo('', ['', ''], 'a')

    def test_exact_composer_transition_is_accepted(self):
        self.assertEqual(evaluate_echo('', ['', 'z'], 'z'), 1)

    def test_unacknowledged_clear_cannot_pass(self):
        with self.assertRaisesRegex(RuntimeError, 'did not clear'):
            evaluate_clear(['z', 'z'])
        with self.assertRaisesRegex(RuntimeError, 'did not clear'):
            evaluate_clear([])

    def test_acknowledged_clear_passes(self):
        evaluate_clear(['z', ''])

    def test_clear_time_cannot_enter_the_echo_latency(self):
        # 1 ms echo plus a 1000 ms clearing must report ~1 ms, not ~1001 ms.
        self.assertAlmostEqual(probe_timing(0.0, 0.001, 1.001), 1.0, places=3)

    def test_out_of_order_probe_timestamps_are_rejected(self):
        with self.assertRaisesRegex(RuntimeError, 'out of order'):
            probe_timing(0.0, 1.0, 0.5)


if __name__ == '__main__':
    unittest.main()
