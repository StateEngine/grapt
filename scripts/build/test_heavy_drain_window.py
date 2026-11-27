"""Shared progress survives forced draining across updates and process reloads."""
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from heavy_drain import Drain, MAX_DRAIN_SECONDS, MAX_EXCLUSIVE_WAIT, SHARED_WINDOW_SECONDS


class DrainWindowTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='secode-drain-window-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.now = 10000.0
        self.exclusive = dict(id='exclusive', owner='exclusive', **{'class': 1},
            enqueued_ns=int((self.now - MAX_EXCLUSIVE_WAIT - 1) * 1e9))
        self.shared = dict(id='shared', owner='shared', **{'class': 1},
            enqueued_ns=int((self.now - 10) * 1e9))
        self.holder = dict(id='holder', owner='holder', **{'class': 1},
            enqueued_ns=int((self.now - 4000) * 1e9))
        self.tickets = [self.exclusive, self.shared, self.holder]
        self.drain = Drain(self.root, self.tickets)
        self.drain.declare(self.exclusive, 1800, 1800)
        self.runs = SimpleNamespace(active={'holder': {'since': self.now - 100}})

    def update(self, now, holders=None):
        return self.drain.update([self.exclusive, self.shared],
            [self.holder] if holders is None else holders, self.runs,
            lambda row: row['id'] == 'shared', now, 3, 2 if holders is None else len(holders))

    def expire(self):
        start = self.now - MAX_DRAIN_SECONDS
        self.drain.state['active'] = dict(ticket='exclusive', since=start,
            updated=start, idle_slot_seconds=0)
        return self.update(self.now)

    def events(self):
        return [json.loads(line) for line in
                (self.root / 'exclusive-drain.jsonl').read_text().splitlines()]

    def test_forced_drain_expiry_yields_in_the_same_update(self):
        self.assertIsNone(self.expire())
        self.assertIsNone(self.drain.state['active'])
        self.assertEqual(self.drain.state['cooldown_until'], self.now + SHARED_WINDOW_SECONDS)
        self.assertEqual([row['event'] for row in self.events()], ['drain-end'])
        self.assertEqual(self.events()[0]['reason'], 'drain-time-limit')
        self.assertEqual(self.events()[0]['seconds'], MAX_DRAIN_SECONDS)

    def test_fresh_clients_keep_the_window_until_its_exact_end_then_force_again(self):
        self.expire()
        deadline = self.now + SHARED_WINDOW_SECONDS
        for now in (self.now, self.now + 1, deadline - 0.001):
            self.drain = Drain(self.root, self.tickets)
            self.assertIsNone(self.update(now))
            self.assertEqual(self.drain.state['cooldown_until'], deadline)
        self.drain = Drain(self.root, self.tickets)
        self.assertEqual(self.update(deadline), 'exclusive')
        self.assertEqual([row['event'] for row in self.events()],
                         ['drain-end', 'forced-drain', 'drain-start'])
        self.assertEqual(self.drain.state['active']['since'], deadline)

    def test_forced_candidate_preserves_the_window_during_and_after_an_exclusive_hold(self):
        self.drain.exclusive_started(self.holder, self.now)
        self.assertIsNone(self.update(self.now + 1))
        self.assertEqual(self.drain.state['yield_owner'], 'holder')
        self.assertIsNone(self.update(self.now + 2, holders=[]))
        deadline = self.now + 2 + SHARED_WINDOW_SECONDS
        self.assertEqual(self.drain.state['cooldown_until'], deadline)
        self.drain = Drain(self.root, self.tickets)
        self.assertIsNone(self.update(deadline - 0.001, holders=[]))
        self.assertEqual(self.update(deadline, holders=[]), 'exclusive')


if __name__ == '__main__':
    unittest.main()
