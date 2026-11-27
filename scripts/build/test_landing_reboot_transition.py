"""A rebooted host archives dead-boot reservation cursors and starts fresh."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import landing_guests
import landing_reservation
import landing_reservation_history as history

BOOT_DEAD = '11111111-2222-3333-4444-555555555555'
BOOT_CURRENT = 'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee'
BOOT_STALE = '99999999-8888-7777-6666-555544443333'
GENERATION = 'dddddddd-cccc-bbbb-aaaa-000000000001'


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def pin(boot, pid=4242):
    return dict(pid=pid, start='100', boot_id=boot, cmdline_sha256='0' * 64)


def idle_record(boot, revision, generation=GENERATION):
    return dict(schema='guest-owner/v1', phase='idle', boot_id=boot,
                generation=generation, revision=revision,
                owner=None, guest=None, memory_gib=None,
                owner_process=None, qemu=None, swtpm=None, launcher=None,
                boot_intent=None, purpose=None, admission_ref=None,
                adopted=False,
                clearance=dict(kind='initial-observation', joined_stop_ref=None,
                               stopped_generation=None, stopped_guest=None,
                               observer=pin(boot)),
                updated_utc='2026-11-18T20:00:00+00:00')


def running_record(boot, revision=823, generation=GENERATION):
    return dict(schema='guest-owner/v1', phase='running', boot_id=boot,
                generation=generation, revision=revision,
                owner='5c', guest='stateengine-winvm', memory_gib=16,
                owner_process=pin(boot, 3517587), qemu=pin(boot, 3520228),
                swtpm=pin(boot, 3520218), launcher=pin(boot, 3517587),
                boot_intent='00000000-0000-4000-8000-000000000001',
                purpose='fixture reservation', admission_ref='msg-' + '0' * 24,
                adopted=False, clearance=None,
                updated_utc='2026-11-13T06:24:42+00:00')


def dump(record):
    return (json.dumps(record) + '\n').encode()


class BootTransition(unittest.TestCase):
    def _env(self, raw, *, cursor=None, authority=None, current_boot=BOOT_CURRENT,
             history_files=()):
        root = Path(raw)
        proc = root / 'proc'
        (proc / 'sys' / 'kernel' / 'random').mkdir(parents=True)
        (proc / 'sys' / 'kernel' / 'random' / 'boot_id').write_text(current_boot)
        state = root / 'state'
        state.mkdir(mode=0o700)
        lock = state / 'owner.lock'
        lock.touch()
        os.chmod(lock, 0o600)
        reader = landing_reservation.Reader(state / 'cursor.json', state / 'state.json',
                                            proc=proc)
        if cursor is not None:
            raw_cursor = cursor if isinstance(cursor, bytes) else dump(cursor)
            (state / 'cursor.json').write_bytes(raw_cursor)
            os.chmod(state / 'cursor.json', 0o600)
        if authority is not None:
            raw_authority = authority if isinstance(authority, bytes) else dump(authority)
            (state / 'state.json').write_bytes(raw_authority)
            os.chmod(state / 'state.json', 0o600)
        if history_files:
            retained = state / 'cursor.json.history'
            retained.mkdir(mode=0o700)
            for name, raw_retained in history_files:
                (retained / name).write_bytes(raw_retained)
                os.chmod(retained / name, 0o400)
        return root, reader, state

    def _census(self, proc):
        return lambda: landing_guests.active(roots=(), proc=proc)

    def _history_files(self, state):
        retained = state / 'cursor.json.history'
        if not retained.exists():
            return []
        return sorted(path.name for path in retained.iterdir())

    def test_dead_boot_cursor_archives_and_starts_fresh(self):
        dead = running_record(BOOT_DEAD)
        authority = idle_record(BOOT_CURRENT, 1315)
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=dead, authority=authority)
            dead_raw = (state / 'cursor.json').read_bytes()
            self.assertEqual(reader.observe(census=self._census(root / 'proc')), [])
            self.assertEqual(landing_reservation.load(state / 'cursor.json')['boot_id'], BOOT_CURRENT)
            self.assertEqual(landing_reservation.load(state / 'cursor.json')['revision'], 1315)
            transition = dict(schema='guest-owner-boot-transition/v1',
                              previous_boot_id=BOOT_DEAD, current_boot_id=BOOT_CURRENT,
                              retained_raw_sha256=sha(dead_raw), previous_revision=823,
                              previous_generation=GENERATION)
            transition_raw = (json.dumps(transition, sort_keys=True) + '\n').encode()
            self.assertEqual(self._history_files(state),
                             sorted([sha(dead_raw) + '.json', sha(transition_raw) + '.json']))
            self.assertEqual((state / 'cursor.json.history' / (sha(dead_raw) + '.json')).read_bytes(),
                             dead_raw)
            self.assertEqual(json.loads(
                (state / 'cursor.json.history' / (sha(transition_raw) + '.json')).read_bytes()),
                transition)

    def test_dead_boot_cursor_stale_authority_fences_then_recovers(self):
        dead = running_record(BOOT_DEAD)
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=dead, authority=idle_record(BOOT_STALE, 1314))
            dead_raw = (state / 'cursor.json').read_bytes()
            census = self._census(root / 'proc')
            with self.assertRaises(ValueError):
                reader.observe(census=census)
            self.assertEqual(len(self._history_files(state)), 2)
            self.assertEqual((state / 'cursor.json').read_bytes(), dead_raw)
            with self.assertRaises(ValueError):
                reader.observe(census=census)
            self.assertEqual(len(self._history_files(state)), 2)
            (state / 'state.json').write_bytes(dump(idle_record(BOOT_CURRENT, 1315)))
            os.chmod(state / 'state.json', 0o600)
            self.assertEqual(reader.observe(census=census), [])
            self.assertEqual(landing_reservation.load(state / 'cursor.json')['boot_id'], BOOT_CURRENT)

    def test_same_boot_malformed_cursor_still_raises(self):
        for broken in (running_record(BOOT_CURRENT, revision=0),
                       running_record(BOOT_CURRENT, generation='not-a-generation')):
            with self.subTest(revision=broken['revision']):
                with tempfile.TemporaryDirectory() as raw:
                    root, reader, state = self._env(raw, cursor=broken,
                                                    authority=idle_record(BOOT_CURRENT, 1))
                    broken_raw = (state / 'cursor.json').read_bytes()
                    with self.assertRaises(ValueError):
                        reader.observe(census=self._census(root / 'proc'))
                    self.assertEqual((state / 'cursor.json').read_bytes(), broken_raw)
                    self.assertFalse((state / 'cursor.json.history').exists())

    def test_unparseable_cursor_still_raises(self):
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=b'{"schema": "guest-owner/v1", ',
                                            authority=idle_record(BOOT_CURRENT, 1))
            with self.assertRaises(ValueError):
                reader.observe(census=self._census(root / 'proc'))
            self.assertFalse((state / 'cursor.json.history').exists())

    def test_missing_cursor_with_history_still_refuses(self):
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, authority=idle_record(BOOT_CURRENT, 1),
                                            history_files=[('e' * 64 + '.json', b'{}')])
            with self.assertRaises(ValueError):
                reader.observe(census=self._census(root / 'proc'))

    def test_missing_cursor_without_history_starts_fresh(self):
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, authority=idle_record(BOOT_CURRENT, 1))
            self.assertEqual(reader.observe(census=self._census(root / 'proc')), [])
            self.assertEqual(landing_reservation.load(state / 'cursor.json')['boot_id'], BOOT_CURRENT)

    def test_cursor_changed_between_read_and_lock_refuses_transition(self):
        dead = running_record(BOOT_DEAD)
        changed = running_record(BOOT_DEAD, revision=824)
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=dead, authority=idle_record(BOOT_CURRENT, 1315))
            dead_raw = (state / 'cursor.json').read_bytes()
            cursor_path = state / 'cursor.json'
            calls = {'cursor': 0}

            def flaky_read(path):
                result = history.read(path)
                if Path(path) == cursor_path:
                    calls['cursor'] += 1
                    if calls['cursor'] >= 2:
                        return dump(changed), history.parse(dump(changed))
                return result

            with patch.object(history, 'read', side_effect=flaky_read):
                with self.assertRaises(ValueError):
                    reader.observe(census=self._census(root / 'proc'))
            self.assertEqual((state / 'cursor.json').read_bytes(), dead_raw)
            self.assertFalse((state / 'cursor.json.history').exists())

    def test_direct_merge_same_boot_cursor_initializes(self):
        record_value = idle_record(BOOT_CURRENT, 1)
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=record_value, authority=record_value)
            self.assertFalse(reader.initialized)
            self.assertEqual(reader.merge([]), [])
            self.assertTrue(reader.initialized)
            self.assertIsNone(reader.previous['clearance']['joined_stop_ref'])
            self.assertEqual(reader.previous['boot_id'], BOOT_CURRENT)
            self.assertEqual((state / 'cursor.json').read_bytes(),
                             (json.dumps(record_value) + '\n').encode())

    def test_direct_merge_dead_boot_cursor_transitions(self):
        dead = running_record(BOOT_DEAD)
        authority = idle_record(BOOT_CURRENT, 1315)
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=dead, authority=authority)
            self.assertEqual(reader.merge([]), [])
            self.assertEqual(reader.previous['boot_id'], BOOT_CURRENT)
            self.assertEqual(landing_reservation.load(state / 'cursor.json')['revision'], 1315)
            self.assertEqual(len(self._history_files(state)), 2)

    def test_malformed_dead_boot_id_never_transitions(self):
        broken = idle_record('not-a-boot-id', 7)
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=broken, authority=idle_record(BOOT_CURRENT, 1))
            broken_raw = (state / 'cursor.json').read_bytes()
            with self.assertRaises(ValueError):
                reader.observe(census=self._census(root / 'proc'))
            self.assertEqual((state / 'cursor.json').read_bytes(), broken_raw)
            self.assertFalse((state / 'cursor.json.history').exists())

    def test_non_string_boot_id_never_transitions(self):
        broken = idle_record(BOOT_DEAD, 7)
        broken['boot_id'] = 12345
        broken['clearance']['observer']['boot_id'] = 12345
        with tempfile.TemporaryDirectory() as raw:
            root, reader, state = self._env(raw, cursor=broken, authority=idle_record(BOOT_CURRENT, 1))
            broken_raw = (state / 'cursor.json').read_bytes()
            with self.assertRaises(ValueError):
                reader.observe(census=self._census(root / 'proc'))
            self.assertEqual((state / 'cursor.json').read_bytes(), broken_raw)
            self.assertFalse((state / 'cursor.json.history').exists())


    def test_direct_merge_initializes_parsed_same_boot_and_dead_boot_cursors(self):
        for prior in [idle_record(BOOT_CURRENT, 1315), running_record(BOOT_DEAD)]:
            with self.subTest(previous_boot=prior['boot_id']):
                with tempfile.TemporaryDirectory() as raw:
                    _, reader, state = self._env(
                        raw, cursor=prior, authority=idle_record(BOOT_CURRENT, 1315))
                    self.assertEqual(reader.merge([]), [])
                    self.assertEqual(reader.previous, idle_record(BOOT_CURRENT, 1315))
                    self.assertEqual(landing_reservation.load(state / 'cursor.json'),
                                     reader.previous)
                    self.assertEqual(len(self._history_files(state)),
                                     0 if prior['boot_id'] == BOOT_CURRENT else 2)

    def test_reboot_owner_republication_clears_the_real_queue_fence(self):
        from heavy_hardware import read, write
        from heavy_queue import Queue
        from landing_priority import CEILINGS, CLAMP, Monitor

        for initial_authority in [None, idle_record(BOOT_STALE, 1314)]:
            with self.subTest(authority_absent=initial_authority is None):
                with tempfile.TemporaryDirectory() as raw:
                    root, reader, state = self._env(
                        raw, cursor=running_record(BOOT_DEAD), authority=initial_authority)
                    original = reader.cursor.read_bytes()
                    queue = Queue(root / 'queue', root / 'heavy.lock')
                    policies = {name: dict(version=1, limit=index + 3,
                                           reason='fixture ordinary ceiling')
                                for index, name in enumerate(CEILINGS)}
                    for name, value in policies.items():
                        write(queue.directory / name, value)
                    census = self._census(root / 'proc')
                    with queue.heavy.open('a+b') as lease:
                        monitor = Monitor(queue, lease)
                        for _ in range(2):
                            with self.assertRaises((OSError, ValueError)):
                                reader.observe(census=census)
                            monitor.step(None, 0)
                            self.assertTrue(monitor.blocked)
                            for name in CEILINGS:
                                self.assertEqual(read(queue.directory / name, None), CLAMP)
                            self.assertEqual(reader.cursor.read_bytes(), original)
                            self.assertEqual(len(self._history_files(state)), 2)
                        authority = idle_record(BOOT_CURRENT, 1315)
                        reader.path.write_bytes(dump(authority))
                        os.chmod(reader.path, 0o600)
                        with patch.object(reader, 'reconcile',
                                          side_effect=AssertionError('manual reconciliation used')):
                            monitor.step(reader.observe(census=census), 32 * 1024**3)
                        self.assertFalse(monitor.blocked)
                        for name, value in policies.items():
                            self.assertEqual(read(queue.directory / name, None), value)
                        self.assertFalse(monitor.fence.path.exists())
                        self.assertEqual(landing_reservation.load(reader.cursor), authority)
                        self.assertEqual(len(self._history_files(state)), 2)


if __name__ == '__main__':
    unittest.main()
