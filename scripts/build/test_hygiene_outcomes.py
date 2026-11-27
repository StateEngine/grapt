"""A cleaner that stopped succeeding is a fact, not a silent no-op."""
import contextlib
import io
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import disk_alert
import disk_hygiene
import disk_stall
import hygiene_outcomes


class OutcomeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.cache = self.root / 'cache'
        self.proc = self.root / 'proc'; self.proc.mkdir(exist_ok=True)
        self.cache.mkdir()
        self.data = self.root / 'data'
        self.data.mkdir()
        outcomes = patch.object(hygiene_outcomes, 'record_path',
                                lambda directory=None: self.data / 'hygiene-outcomes.json')
        outcomes.start()
        self.addCleanup(outcomes.stop)
        # The alert state belongs to the fixture cache, never the operator's.
        self.agent = 'agt-fixture-seat'

    def record(self, *outcomes):
        for outcome in outcomes:
            hygiene_outcomes.append(hygiene_outcomes.row(outcome, 'refused census'))

    def alert(self, now=1000, notify=None):
        # Delivery is its own unit now, so the controls point at that seam. The
        # `notify` a caller used to pass is the send function here, and it is
        # called with a bound timeout like the real one.
        def send(actor, recipients, body, **_options):
            (notify or (lambda _body: None))(body)
        return disk_alert.deliver(self.cache, self.agent, now, send=send,
                                  registry=lambda: ([{'agent_id': self.agent}], self.agent))

    def lock_probe(self, path):
        script = '''import fcntl,sys
with open(sys.argv[1], 'a') as held:
    try:
        fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise SystemExit(7)
'''
        return subprocess.run([sys.executable, '-c', script, str(path)],
                              capture_output=True, timeout=3).returncode

    def test_a_recovery_between_delivery_ticks_rearms_the_next_outage(self):
        self.record('failed', 'failed', 'failed')
        messages = []
        self.assertIsNotNone(self.alert(notify=messages.append))
        self.record('succeeded', 'failed', 'failed', 'failed')
        self.assertEqual(hygiene_outcomes.trailing_failures(), 3)
        self.assertIsNotNone(self.alert(now=2000, notify=messages.append))
        self.assertEqual(len(messages), 2)

    def test_an_unsent_alert_never_becomes_a_failed_cleanup_pass_in_mail(self):
        self.record('failed', 'failed', 'failed')
        def refused(_body):
            raise RuntimeError('transport unavailable')
        with contextlib.redirect_stderr(io.StringIO()):
            for attempt in range(2):
                self.assertIsNone(self.alert(now=1000 + attempt, notify=refused))
        self.assertEqual(hygiene_outcomes.trailing_failures(), 3)
        messages = []
        self.assertIsNotNone(self.alert(now=2000, notify=messages.append))
        self.assertIn('refused 3 passes in a row', messages[0])

    def unsent(self):
        """The deliverer's own record of the last unsent delivery, or None."""
        state = disk_alert._state(self.cache / disk_alert.STATE)
        return None if state['unsent'] is None else state

    def test_real_transport_timeout_is_recorded_as_an_unsent_alert(self):
        import nightly_revert
        self.record('failed', 'failed', 'failed')
        for phase in ('discovery', 'send'):
            registry = (None if phase == 'discovery' else
                        lambda: ([{'agent_id': self.agent}], self.agent))
            timeout = subprocess.TimeoutExpired(['ssh'], 120 if registry is None else 20)
            with self.subTest(phase=phase), \
                    patch.object(nightly_revert, 'remote', side_effect=timeout), \
                    contextlib.redirect_stderr(io.StringIO()) as output:
                # Each phase starts from a clean deliverer state, so the count is
                # this phase's own sends.
                (self.cache / disk_alert.STATE).unlink(missing_ok=True)
                for attempt in range(3):
                    self.assertIsNone(disk_alert.deliver(self.cache, self.agent,
                                                        1000 + attempt, registry=registry))
                state = self.unsent()
                self.assertIsNotNone(state, 'a timeout must be recorded as unsent')
                self.assertEqual(state['failed_sends'], 3)
                self.assertEqual(state['unsent_kind'], 'TimeoutExpired')
                self.assertIn('transport has failed', output.getvalue())

    def test_installed_delivery_module_and_rendered_arguments_reach_its_parser(self):
        import install_schedules
        destination = self.root / 'controller'
        destination.mkdir()
        source = Path(__file__).resolve().parents[2]
        install_schedules.install_control(source, destination)
        self.assertTrue((destination / 'disk_alert.py').is_file())
        units = install_schedules.units(source, self.root, self.agent, destination)
        service = units['secode-cleanup-alert.service']
        command = shlex.split(next(line.partition('=')[2] for line in service.splitlines()
                                  if line.startswith('ExecStart=')))
        self.assertEqual(Path(command[1]), destination / 'disk_alert.py')
        with patch.object(sys, 'argv', command[1:]), \
                patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                patch.object(disk_alert, 'deliver') as deliver:
            self.assertEqual(disk_alert.main(), 0)
        deliver.assert_called_once_with(self.cache.resolve(), self.agent)
        self.assertIn('TimeoutStartSec=5min', service)
        self.assertIn('OnUnitInactiveSec=5min', units['secode-cleanup-alert.timer'])

    def test_the_alert_lock_is_held_until_delivery_finishes(self):
        self.record('failed', 'failed', 'failed')
        during = []
        self.assertIsNotNone(self.alert(notify=lambda _body:
                             during.append(self.lock_probe(self.cache / '.hygiene-alert.lock'))))
        self.assertEqual(during, [7])
        self.assertEqual(self.lock_probe(self.cache / '.hygiene-alert.lock'), 0)

    def test_the_outcome_lock_is_held_through_publication(self):
        original = hygiene_outcomes.os.replace
        during = []
        lock = self.data / 'hygiene-outcomes.json.lock'
        def publish(source, target):
            during.append(self.lock_probe(lock))
            return original(source, target)
        with patch.object(hygiene_outcomes.os, 'replace', side_effect=publish):
            self.record('failed')
        self.assertEqual(during, [7])
        self.assertEqual(self.lock_probe(lock), 0)

    def test_malformed_registry_data_never_replaces_the_cleanup_error(self):
        import nightly_revert
        self.record('failed', 'failed', 'failed')
        for payload in ({'project_id': 'secode', 'source': 'stateengine-registry',
                         'agents': [{'agent_id': self.agent}]}, []):
            with self.subTest(payload=payload), \
                    patch.object(nightly_revert, 'remote', return_value=json.dumps(payload)), \
                    patch.object(nightly_revert, 'send') as send, \
                    patch.object(disk_hygiene, 'reclaim', side_effect=RuntimeError('census refused')), \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, '^census refused$'):
                    disk_hygiene.scheduled_reclaim(self.cache, self.root, False, None,
                                                   proc=self.proc)
                send.assert_not_called()

    def test_a_stalled_registry_cannot_hold_the_completed_cleanup_result(self):
        self.record('failed', 'failed', 'failed')
        script = '''import sys,threading
from pathlib import Path
import disk_hygiene,hygiene_outcomes,nightly_revert
root = Path(sys.argv[1])
hygiene_outcomes.record_path = lambda directory=None: root/'data/hygiene-outcomes.json'
disk_hygiene.data_root = lambda: root/'data'
def stalled(*args, **kwargs):
    (root/'registry-entered').write_text('cleanup already refused')
    threading.Event().wait(30)
    return '{}'
nightly_revert.remote = stalled
nightly_revert.send = lambda *args, **kwargs: None
def refused(*args, **kwargs):
    raise RuntimeError('census refused')
disk_hygiene.reclaim = refused
try:
    disk_hygiene.scheduled_reclaim(root/'cache', root, False, None, proc=root/'proc')
except RuntimeError as error:
    if str(error) != 'census refused':
        raise
else:
    raise AssertionError('the original cleanup error was lost')
'''
        try:
            completed = subprocess.run([sys.executable, '-c', script, str(self.root)],
                                       cwd=Path(__file__).parent, capture_output=True,
                                       text=True, timeout=3)
        except subprocess.TimeoutExpired:
            self.assertTrue((self.root / 'registry-entered').is_file(),
                            'the child did not reach registry discovery')
            self.fail('registry discovery blocked the completed cleanup result')
        self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_multibyte_details_cannot_make_the_record_exceed_its_byte_bound(self):
        for _ in range(hygiene_outcomes.ROWS):
            hygiene_outcomes.append(hygiene_outcomes.row('failed', '\U0001f4a1' * 512))
        path = hygiene_outcomes.record_path()
        self.assertLessEqual(path.stat().st_size, hygiene_outcomes.BYTES)
        self.assertGreaterEqual(hygiene_outcomes.trailing_failures(), 3)

    def test_retention_does_not_turn_one_failure_streak_into_new_alerts(self):
        messages = []
        for index in range(hygiene_outcomes.ROWS + 3):
            self.record('failed')
            self.alert(now=1000 + index * (disk_stall.REPEAT_SECONDS + 1),
                       notify=messages.append)
        self.assertEqual(len(messages), 1, 'no success ended this failure streak')

    def test_registry_failure_is_an_unsent_alert_and_preserves_the_cleanup_error(self):
        import nightly_revert
        self.record('failed', 'failed', 'failed')
        # The pass keeps its own error and never touches the network.
        with patch.object(nightly_revert, 'registry', side_effect=RuntimeError('registry unavailable')), \
                patch.object(nightly_revert, 'send') as send, \
                patch.object(disk_hygiene, 'reclaim', side_effect=RuntimeError('census refused')), \
                contextlib.redirect_stderr(io.StringIO()):
            for _ in range(3):
                with self.assertRaisesRegex(RuntimeError, '^census refused$'):
                    disk_hygiene.scheduled_reclaim(self.cache, self.root, False, None,
                                                   proc=self.proc)
        send.assert_not_called()
        # The deliverer reports the same failure as unsent, and says so loudly.
        with patch.object(nightly_revert, 'registry', side_effect=RuntimeError('registry unavailable')), \
                patch.object(nightly_revert, 'send') as send, \
                contextlib.redirect_stderr(io.StringIO()) as output:
            for attempt in range(3):
                self.assertIsNone(disk_alert.deliver(self.cache, self.agent, 1000 + attempt))
        send.assert_not_called()
        state = self.unsent()
        self.assertIsNotNone(state, 'a discovery failure is an unsent delivery')
        self.assertEqual(state['failed_sends'], 3)
        self.assertIn('transport has failed 3 times', output.getvalue())

    def test_an_alert_lock_fifo_cannot_hold_up_delivery(self):
        # The FIFO lives beside the delivery unit's state. A cleanup never waits
        # on it, and the delivery itself must not hang either: it declines.
        self.record('failed', 'failed', 'failed')
        os.mkfifo(self.cache / '.hygiene-alert.lock')
        sent = []
        self.assertIsNone(self.alert(notify=sent.append))
        self.assertEqual(sent, [], 'a planted lock means no send, not a blocked one')

    def test_the_cleanup_pass_never_waits_for_delivery(self):
        # The pass records its outcome and returns; it does not reach the network.
        self.record('failed', 'failed', 'failed')
        script = '''import sys
from pathlib import Path
import disk_hygiene, hygiene_outcomes, nightly_revert
root = Path(sys.argv[1])
hygiene_outcomes.record_path = lambda directory=None: root/'data/hygiene-outcomes.json'
disk_hygiene.data_root = lambda: root/'data'
def stalled(*args, **kwargs):
    (root/'registry-entered').write_text('cleanup already refused')
    raise AssertionError('the cleanup pass reached the network')
nightly_revert.remote = stalled
nightly_revert.send = lambda *args, **kwargs: None
def refused(*args, **kwargs):
    raise RuntimeError('census refused')
disk_hygiene.reclaim = refused
try:
    disk_hygiene.scheduled_reclaim(root/'cache', root, False, None, proc=root/'proc')
except RuntimeError as error:
    if str(error) != 'census refused':
        raise
else:
    raise AssertionError('the original cleanup error was lost')
'''
        completed = subprocess.run([sys.executable, '-c', script, str(self.root)],
                                   cwd=Path(__file__).parent, capture_output=True,
                                   text=True, timeout=10)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertFalse((self.root / 'registry-entered').exists(),
                         'delivery must be a separate unit, never inside the pass')

    def test_rendered_units_reach_the_cleaner_and_the_deliverer(self):
        import install_schedules
        units = install_schedules.units(self.root, self.root, self.agent)
        cleaner = next(line.partition('=')[2] for line in
                       units['secode-disk-hygiene.service'].splitlines()
                       if line.startswith('ExecStart='))
        flags = shlex.split(cleaner)[2:]
        with patch.object(sys, 'argv', ['disk_hygiene.py', *flags]), \
                patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                patch.object(disk_hygiene, 'data_root', return_value=self.data), \
                patch.object(disk_hygiene, 'scheduled_reclaim') as scheduled:
            self.assertEqual(disk_hygiene.main(), 0)
        scheduled.assert_called_once()
        self.assertEqual(scheduled.call_args.args[0], self.cache)
        self.assertEqual(scheduled.call_args.args[3], self.data / 'diagnostics')
        # The deliverer names the agent, so the cleaner's own arguments do not.
        self.assertEqual(len(scheduled.call_args.args), 4)
        deliverer = next(line.partition('=')[2] for line in
                         units['secode-cleanup-alert.service'].splitlines()
                         if line.startswith('ExecStart='))
        self.assertIn('--agent-id ' + self.agent, deliverer)
        self.assertIn('secode-cleanup-alert.timer', units)

    def test_two_failures_are_not_yet_a_pattern_and_three_alert_once(self):
        self.record('refused', 'refused')
        self.assertIsNone(self.alert())
        self.record('refused')
        messages = []
        self.assertIsNotNone(self.alert(notify=messages.append))
        self.assertEqual(len(messages), 1)
        self.assertIn('3 passes in a row', messages[0])
        self.assertIn('refused census', messages[0])
        # One alert per streak: a fourth failure does not send again.
        self.record('refused')
        self.assertIsNone(self.alert(now=1100, notify=messages.append))
        self.assertEqual(len(messages), 1)

    def test_a_success_resets_the_streak_and_rearms_the_next_one(self):
        self.record('refused', 'refused', 'refused')
        self.assertIsNotNone(self.alert())
        self.record('succeeded')
        self.assertEqual(hygiene_outcomes.trailing_failures(), 0)
        self.assertIsNone(self.alert(now=1200))
        self.record('refused', 'refused', 'refused')
        messages = []
        self.assertIsNotNone(self.alert(now=1300, notify=messages.append))
        self.assertEqual(len(messages), 1)

    def test_a_dead_transport_is_loud_and_recorded_without_failing_the_pass(self):
        self.record('refused', 'refused', 'refused')
        def dead(_body):
            raise RuntimeError('ssh refused')
        with contextlib.redirect_stderr(io.StringIO()) as output:
            for attempt in range(3):
                self.assertIsNone(self.alert(now=1000 + attempt, notify=dead))
        text = output.getvalue()
        self.assertIn('could not be sent', text)
        self.assertIn('3 times', text)
        # Unsent deliveries live in the deliverer's state and never inflate the
        # pass count: the cleaner is still only 3 passes down, not 6.
        self.assertEqual(hygiene_outcomes.trailing_failures(), 3)
        self.assertIsNotNone(self.unsent())
        # A failing alert never raises into the cleaner: it returned None above,
        # which is what a scheduled pass sees.
        # Once the transport works, the still-unannounced outage is delivered.
        messages = []
        self.assertIsNotNone(self.alert(now=1400, notify=messages.append))
        self.assertEqual(len(messages), 1)

    def test_a_pass_that_succeeds_after_alerting_stops_the_alert(self):
        self.record('refused', 'refused', 'refused')
        self.assertIsNotNone(self.alert())
        self.record('succeeded')
        messages = []
        self.assertIsNone(self.alert(now=2000, notify=messages.append))
        self.assertEqual(messages, [])

    def test_the_scheduled_pass_records_its_own_outcome(self):
        recorded = []
        with patch.object(disk_hygiene, 'reclaim', side_effect=RuntimeError('unknown reader')), \
                patch.object(hygiene_outcomes, 'append',
                             side_effect=lambda sample, directory=None: recorded.append(sample)):
            with contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaisesRegex(RuntimeError, 'unknown reader'):
                    disk_hygiene.scheduled_reclaim(self.cache, self.root, False, None,
                                                   proc=self.proc)
        self.assertEqual([sample['outcome'] for sample in recorded], ['failed'])
        self.assertEqual(recorded[0]['detail'], 'unknown reader')

    def test_a_damaged_or_unwritable_record_never_changes_the_cleanup(self):
        path = self.data / 'hygiene-outcomes.json'
        damaged = '{"version": 1, "samples": "not a list"}'
        path.write_text(damaged)
        self.assertEqual(hygiene_outcomes.samples(), [])
        with contextlib.redirect_stderr(io.StringIO()) as output:
            with patch.object(disk_hygiene, 'reclaim'):
                disk_hygiene.scheduled_reclaim(self.cache, self.root, False, None,
                                                   proc=self.proc)
        # A decision was still made and the pass still succeeded; only its
        # witness was unavailable, and saying so is the whole point.
        self.assertEqual(path.read_text(), damaged)
        self.assertIn('not recorded', output.getvalue())


if __name__ == '__main__':
    unittest.main()
