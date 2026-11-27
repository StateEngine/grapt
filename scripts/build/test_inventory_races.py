"""A disappearing build output is gone; unrelated measurement failures refuse."""
import contextlib
import io
from pathlib import Path
import shutil
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_eviction
import evict_targets as eviction


class InventoryRaceTests(unittest.TestCase):
    setUp = test_eviction.EvictionTests.setUp
    target = test_eviction.EvictionTests.target
    evict = test_eviction.EvictionTests.evict

    def test_target_deleted_after_listing_is_omitted_from_inventory(self):
        target, _ = self.target('gone', age=100000)
        entries = eviction.bounded_entries

        def listed(path, *args):
            result = entries(path, *args)
            if path == target.parent:
                shutil.rmtree(target)
            return result

        diagnostic = io.StringIO()
        with patch.object(eviction, 'bounded_entries', side_effect=listed), \
                contextlib.redirect_stderr(diagnostic):
            removed, total = self.evict(self.directory / 'current')
        self.assertEqual(removed, [])
        self.assertEqual(total, eviction.usage(self.cache, time.monotonic() + 5))
        self.assertIn('Managed-target estimates: 0 bytes', diagnostic.getvalue())

    def test_disappearance_at_each_inventory_and_recheck_phase_is_gone(self):
        for phase, occurrence in [('usage', 1), ('disposable', 1), ('latest_modified', 1),
                                  ('disposable', 2), ('latest_modified', 2)]:
            with self.subTest(phase=phase, occurrence=occurrence):
                target, _ = self.target(f'{phase}-{occurrence}', age=100000)
                original = getattr(eviction, phase)
                calls = 0

                def disappear(path, *args):
                    nonlocal calls
                    if path == target:
                        calls += 1
                        if calls == occurrence:
                            shutil.rmtree(target)
                    return original(path, *args)

                with patch.object(eviction, phase, side_effect=disappear):
                    removed, total = self.evict(self.directory / 'current')
                self.assertEqual(removed, [])
                self.assertFalse(target.exists())
                self.assertEqual(total, eviction.usage(self.cache, time.monotonic() + 5))

    def test_nested_entry_deleted_after_scandir_does_not_abort_mtime_walk(self):
        target, _ = self.target('nested-entry', age=100000)
        nested = target / 'debug'
        scan = eviction.os.scandir

        def listed(path):
            if path == target:
                with scan(path) as stream:
                    result = list(stream)
                shutil.rmtree(nested)
                return contextlib.nullcontext(iter(result))
            return scan(path)

        with patch.object(eviction.os, 'scandir', side_effect=listed):
            eviction.latest_modified(target, time.monotonic() + 5)
        self.assertFalse(nested.exists())

    def test_nested_directory_deleted_before_scanning_is_skipped(self):
        target, _ = self.target('nested-directory', age=100000)
        nested = target / 'debug'
        scan = eviction.os.scandir

        def disappear(path):
            if path == nested:
                shutil.rmtree(nested)
            return scan(path)

        with patch.object(eviction.os, 'scandir', side_effect=disappear):
            eviction.latest_modified(target, time.monotonic() + 5)
        self.assertFalse(nested.exists())

    def test_unrelated_missing_file_and_permission_errors_still_refuse(self):
        target, _ = self.target('still-present', age=100000)
        for error in [FileNotFoundError('missing accounting tool'), PermissionError('unreadable'),
                      OSError('I/O failure')]:
            for phase in ['usage', 'disposable', 'latest_modified']:
                with self.subTest(error=type(error).__name__, phase=phase):
                    original = getattr(eviction, phase)

                    def fail(path, *args):
                        if path == target:
                            raise error
                        return original(path, *args)

                    with patch.object(eviction, phase, side_effect=fail):
                        with self.assertRaises(type(error)):
                            self.evict(self.directory / 'current')
        self.assertTrue(target.is_dir())

    def test_nested_permission_error_is_not_a_vanished_directory(self):
        target, _ = self.target('permission', age=100000)
        scan = eviction.os.scandir

        def refused(path):
            if path == target / 'debug':
                raise PermissionError('unreadable nested directory')
            return scan(path)

        with patch.object(eviction.os, 'scandir', side_effect=refused):
            with self.assertRaises(PermissionError):
                eviction.latest_modified(target, time.monotonic() + 5)

    def test_whole_root_measurement_failure_after_eviction_is_never_skipped(self):
        target, _ = self.target('removed', age=100000)
        usage = eviction.usage
        calls = 0

        def measure(path, deadline):
            nonlocal calls
            if path == self.cache:
                calls += 1
                if calls == 2:
                    self.assertFalse(target.exists())
                    raise FileNotFoundError('whole-root accounting failed')
            return usage(path, deadline)

        with patch.object(eviction, 'usage', side_effect=measure):
            with self.assertRaisesRegex(FileNotFoundError, 'whole-root accounting failed'):
                self.evict(self.directory / 'current')
        self.assertEqual(calls, 2)

    def test_nested_entry_permission_error_is_not_skipped(self):
        target, _ = self.target('entry-permission', age=100000)

        def refused(**_kwargs):
            raise PermissionError('unreadable entry metadata')

        entry = SimpleNamespace(stat=refused)
        with patch.object(eviction.os, 'scandir',
                          return_value=contextlib.nullcontext(iter([entry]))):
            with self.assertRaises(PermissionError):
                eviction.latest_modified(target, time.monotonic() + 5)
