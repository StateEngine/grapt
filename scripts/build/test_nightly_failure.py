"""Termination must leave an honest reason and the observed final child status."""
from contextlib import nullcontext
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import nightly
import nightly_failure
import test_nightly


class FailureTests(unittest.TestCase):
    setUp = test_nightly.NightlyTests.setUp
    commit = test_nightly.NightlyTests.commit

    def run_gate(self, script):
        (self.root/'scripts/build').mkdir(parents=True)
        (self.root/'scripts/build/gate.py').write_text('FULL_CAPACITY_PROTOCOL = 1\n')
        (self.root/'scripts/gate.sh').write_text(script)
        self.state.mkdir()
        with tempfile.TemporaryFile() as lease, patch.object(nightly.heavy, 'slot') as slot, \
                patch.object(nightly.full_capacity,'reserve',return_value=nullcontext()):
            slot.return_value.__enter__.return_value = lease
            outcome = nightly.full_gate(self.root, self.state, 'f'*40)
        return outcome, json.loads((self.state/'last-failure.json').read_text())

    def test_signal_exit_records_status_and_stderr_without_source_attribution(self):
        outcome, failure = self.run_gate('echo last-stderr-line >&2\nkill -TERM $$\n')
        self.assertFalse(outcome)
        self.assertEqual(outcome.failures, ())
        self.assertEqual(failure['signal'], 'SIGTERM')
        self.assertIsNone(failure['exit_status'])
        self.assertIn('last-stderr-line', failure['stderr_tail'][0]['text'])
        self.assertIn('signal=SIGTERM', outcome.diagnostic)

    def test_nonzero_exit_retains_only_a_bounded_tail(self):
        outcome, failure = self.run_gate('for i in {1..2000}; do echo "diagnostic-$i" >&2; done\nexit 7\n')
        self.assertFalse(outcome)
        self.assertEqual(failure['exit_status'], 7)
        self.assertIsNone(failure['signal'])
        text = failure['stderr_tail'][0]['text']
        self.assertIn('diagnostic-2000', text)
        self.assertNotIn('diagnostic-100\n', text)
        self.assertLessEqual(len(text.splitlines()), 20)

    def test_blank_interruption_has_a_reason_and_preserves_the_candidate(self):
        candidate = self.commit(True, 'candidate')
        reports = []
        class Publisher:
            def report(self, result, failures):
                reports.append((result, failures))
        with self.assertRaises(KeyboardInterrupt):
            nightly.audit(self.root, self.state, candidate,
                          lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()),
                          dry_run=False, publish=Publisher())
        self.assertIn('KeyboardInterrupt', (self.state/'last-result').read_text())
        self.assertIn('KeyboardInterrupt', reports[0][1][0])
        self.assertEqual(nightly.git(self.root, 'rev-parse', 'HEAD'), candidate)

    def test_parent_sigterm_records_requested_signal_after_child_cleanup(self):
        (self.root/'scripts/build').mkdir(parents=True)
        (self.root/'scripts/build/gate.py').write_text('FULL_CAPACITY_PROTOCOL = 1\n')
        (self.root/'scripts/gate.sh').write_text('echo before-stop >&2\nexec sleep 30\n')
        self.state.mkdir()
        code = '''import contextlib,os,signal,sys,threading,tempfile,time
from pathlib import Path
import nightly,heavy
heavy.slot=lambda *args,**kwargs:tempfile.TemporaryFile()
nightly.full_capacity.reserve=lambda *args,**kwargs:contextlib.nullcontext()
nightly.gate.nightly_start_allowed=lambda:None
signal.signal(signal.SIGTERM,nightly.request_stop)
def stop_when_launched():
    deadline=time.monotonic()+5
    while time.monotonic()<deadline:
        paths=list(Path(sys.argv[2]).glob('*.driver.log'))
        if paths and 'before-stop' in paths[0].read_text():
            break
        time.sleep(0.01)
    os.kill(os.getpid(),signal.SIGTERM)
threading.Thread(target=stop_when_launched).start()
try:
    nightly.full_gate(Path(sys.argv[1]),Path(sys.argv[2]),'f'*40)
except KeyboardInterrupt:
    raise SystemExit(42)
'''
        environment = __import__('os').environ.copy()
        environment['PYTHONPATH'] = str(Path(__file__).resolve().parent)
        result = subprocess.run([sys.executable, '-c', code, str(self.root), str(self.state)],
                                env=environment, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 42, result.stderr)
        failure = json.loads((self.state/'last-failure.json').read_text())
        self.assertEqual(failure['requested_signal'], 'SIGTERM')
        self.assertEqual(failure['signal'], 'SIGTERM')
        self.assertIn('before-stop', failure['stderr_tail'][0]['text'])

    def test_service_observer_records_an_uncatchable_parent_kill(self):
        self.state.mkdir()
        log = self.state/'driver.log'
        log.write_text('last worker stderr\n')
        (self.state/'current-attempt.json').write_text(json.dumps(dict(commit='f'*40, logs=[str(log)])))
        (self.state/'last-result').write_text('RUNNING main '+'f'*40)
        process = subprocess.Popen([sys.executable, '-c', 'import os,signal;os.kill(os.getpid(),signal.SIGKILL)'])
        status = process.wait(timeout=5)
        self.assertEqual(status, -signal.SIGKILL)
        nightly_failure.service_exit(self.state, dict(SERVICE_RESULT='signal', EXIT_CODE='killed', EXIT_STATUS='KILL'))
        failure = json.loads((self.state/'last-failure.json').read_text())
        self.assertEqual(failure['signal'], 'SIGKILL')
        self.assertEqual(failure['service_exit_status'], 'KILL')
        self.assertIn('last worker stderr', failure['stderr_tail'][0]['text'])
        self.assertIn('SIGKILL', (self.state/'last-result').read_text())

    def test_service_observation_preserves_a_handled_diagnosis_and_success(self):
        self.state.mkdir()
        (self.state/'current-attempt.json').write_text('{"commit": null, "logs": []}')
        nightly_failure.record(self.state, RuntimeError('receipt invalid'), returncode=7)
        (self.state/'last-result').write_text('INCONCLUSIVE receipt invalid')
        nightly_failure.service_exit(self.state, dict(SERVICE_RESULT='exit-code', EXIT_CODE='exited', EXIT_STATUS='2'))
        failure = json.loads((self.state/'last-failure.json').read_text())
        self.assertEqual(failure['message'], 'receipt invalid')
        self.assertEqual(failure['exit_status'], 7)
        self.assertEqual(failure['service_exit_status'], '2')
        before = (self.state/'last-failure.json').read_bytes()
        nightly_failure.service_exit(self.state, dict(SERVICE_RESULT='success'))
        self.assertEqual(before, (self.state/'last-failure.json').read_bytes())
