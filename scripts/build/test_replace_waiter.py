"""Private process tests for owner-operated waiting-launcher retirement."""
import json
from pathlib import Path
import subprocess
import sys
import time
import unittest

import heavy
from heavy_drain import Drain
from fixture_environment import isolated_environment
from replace_waiter import retire
import test_heavy_ram as fixtures


class ReplaceWaiterTests(unittest.TestCase):
    setUp = fixtures.HeavyRamTests.setUp
    set_available = fixtures.HeavyRamTests.set_available
    cleanup = fixtures.HeavyRamTests.cleanup
    child = fixtures.HeavyRamTests.child
    line = fixtures.HeavyRamTests.line
    enqueued = fixtures.HeavyRamTests.enqueued
    finish = fixtures.HeavyRamTests.finish

    def ticket(self, owner):
        with self.queue.locked(time.monotonic() + 5):
            return next(row.copy() for row in self.queue.state()[0] if row['owner'] == owner)

    def replacement(self, ticket):
        return retire(self.queue, ticket['id'], ticket['owner'], ticket['pid'], ticket['start'])

    def test_retired_waiter_can_resume_unchanged_work_once_with_its_original_priority(self):
        holder = self.child('holder', 'disk')
        self.assertEqual(self.line(holder), 'entered disk')
        waiting = self.child('owned')
        self.enqueued('owned')
        ticket = self.ticket('owned')
        token = self.replacement(ticket)
        code = waiting.wait(timeout=5)
        self.assertEqual(code, 130, waiting.stderr.read(65536).decode(errors='replace'))
        self.assertIsNone(holder.poll(), 'replacement stopped the admitted holder')
        worker = fixtures.WORKER.replace('job_owner=sys.argv[2],',
                                        'job_owner=sys.argv[2],refund=sys.argv[6],')
        self.assertNotEqual(worker, fixtures.WORKER)
        retry = subprocess.Popen([sys.executable, '-B', '-u', '-c', worker,
            str(self.root), 'owned', 'ram', '1', '10800', token],
            cwd=Path(heavy.__file__).parent, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True, bufsize=0, env=isolated_environment())
        self.addCleanup(self.cleanup, retry)
        self.enqueued('owned')
        new = self.ticket('owned')
        self.assertEqual((new['class'], new['enqueued_ns']), (ticket['class'], ticket['enqueued_ns']))
        with self.queue.locked(time.monotonic() + 5):
            drain = Drain(self.queue.directory, self.queue.state()[0])
            self.assertEqual(drain.state['refunds'], [])
            with self.assertRaises(ValueError):
                drain.redeem(token, new)
        self.finish(holder)
        self.assertEqual(self.line(retry), 'entered ram')
        self.finish(retry)
        events = [json.loads(line) for line in
            (self.queue.directory / 'exclusive-drain.jsonl').read_text().splitlines()]
        self.assertEqual([row['event'] for row in events], ['waiter-replacement-refund'])

    def test_foreign_or_stale_waiting_identity_is_refused_without_signalling(self):
        holder = self.child('holder', 'disk')
        self.assertEqual(self.line(holder), 'entered disk')
        waiting = self.child('owned')
        self.enqueued('owned')
        ticket = self.ticket('owned')
        for changed in (dict(owner='other'), dict(start='stale'), dict(id='not-this-ticket')):
            with self.assertRaisesRegex(ValueError, 'identity or owner changed'):
                self.replacement(dict(ticket, **changed))
            self.assertIsNone(waiting.poll())
        self.assertFalse((self.queue.directory / 'exclusive-drain.jsonl').exists())

    def test_retirement_handles_an_interrupt_ignored_by_the_detached_parent(self):
        script = (
            'import signal,unittest; signal.signal(signal.SIGINT,signal.SIG_IGN); '
            'suite=unittest.defaultTestLoader.loadTestsFromName('
            '"test_replace_waiter.ReplaceWaiterTests.'
            'test_retired_waiter_can_resume_unchanged_work_once_with_its_original_priority"); '
            'result=unittest.TextTestRunner().run(suite); '
            'raise SystemExit(0 if result.wasSuccessful() else 1)')
        result = subprocess.run([sys.executable, '-B', '-c', script],
            cwd=Path(heavy.__file__).parent, env=isolated_environment(),
            capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_admission_after_owner_snapshot_refuses_replacement(self):
        holder = self.child('holder', 'disk')
        self.assertEqual(self.line(holder), 'entered disk')
        waiting = self.child('owned')
        self.enqueued('owned')
        ticket = self.ticket('owned')
        self.finish(holder)
        self.assertEqual(self.line(waiting), 'entered ram')
        with self.assertRaisesRegex(ValueError, 'already admitted'):
            self.replacement(ticket)
        self.assertIsNone(waiting.poll())
        self.finish(waiting)

    def test_conversion_waiter_is_already_admitted_and_cannot_be_replaced(self):
        running = self.child('owned')
        self.assertEqual(self.line(running), 'entered ram')
        ticket = self.ticket('owned')
        with self.queue.locked(time.monotonic() + 5):
            _, runs = self.queue.state()
            runs.pause(ticket)
            runs.save()
        with self.assertRaisesRegex(ValueError, 'already admitted'):
            self.replacement(ticket)
        self.assertIsNone(running.poll())
        self.finish(running)


if __name__ == '__main__':
    unittest.main()
