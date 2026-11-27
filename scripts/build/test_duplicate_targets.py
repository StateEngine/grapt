"""Delete only superseded executable hashes, preserving current and foreign files."""
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

import duplicate_targets as duplicates


class DuplicateTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.now = time.time()

    def binary(self, name, age, profile='debug', magic=b'\x7fELF'):
        path = self.root / profile / 'deps' / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(magic + b'generated executable')
        path.chmod(0o755)
        os.utime(path, (self.now-age, self.now-age))
        return path

    def prune(self, dry=False):
        return duplicates.prune(self.root, time.monotonic()+5, self.now, dry)

    def test_only_old_hash_and_its_dependency_are_removed(self):
        old = self.binary('journey-1111111111111111', 7200)
        dep = old.with_name(old.name+'.d'); dep.write_text('generated dependency')
        newest = self.binary('journey-2222222222222222', 60)
        library = self.binary('libjourney-3333333333333333.rlib', 7200)
        unknown = self.binary('notes-4444444444444444', 7200, magic=b'text')
        self.assertEqual(self.prune(), [old])
        self.assertFalse(old.exists()); self.assertFalse(dep.exists())
        self.assertTrue(all(path.exists() for path in (newest, library, unknown)))

    def test_recent_hashes_and_distinct_tests_and_profiles_survive(self):
        paths = [self.binary('journey-1111111111111111', 120),
                 self.binary('journey-2222222222222222', 60),
                 self.binary('other-1111111111111111', 7200),
                 self.binary('journey-1111111111111111', 7200, 'release')]
        self.assertEqual(self.prune(), [])
        self.assertTrue(all(path.exists() for path in paths))

    def test_cross_target_pe_duplicates_and_dry_run(self):
        old = self.binary('suite-1111111111111111.exe', 7200, 'x86_64-pc-windows-msvc/debug', b'MZ')
        newest = self.binary('suite-2222222222222222.exe', 60, 'x86_64-pc-windows-msvc/debug', b'MZ')
        dependency = old.with_suffix('.d'); dependency.write_text('generated')
        self.assertEqual(self.prune(True), [old]); self.assertTrue(old.exists())
        self.assertEqual(self.prune(), [old])
        self.assertTrue(newest.exists()); self.assertFalse(dependency.exists())

    def test_linked_profile_or_executable_never_reaches_external_files(self):
        external = self.root/'outside'; external.mkdir()
        (external/'deps').mkdir()
        victim = external/'deps/journey-1111111111111111'; victim.write_bytes(b'\x7fELFkeep')
        (self.root/'debug').symlink_to(external, target_is_directory=True)
        self.assertEqual(self.prune(), []); self.assertTrue(victim.exists())

    def test_inode_change_after_inventory_refuses_deletion(self):
        old = self.binary('journey-1111111111111111', 7200)
        self.binary('journey-2222222222222222', 60)
        inventory = duplicates.candidates(self.root, time.monotonic()+5)
        old.unlink(); old.write_bytes(b'new content')
        with patch.object(duplicates, 'candidates', return_value=inventory):
            with self.assertRaisesRegex(RuntimeError, 'changed after inventory'):
                self.prune()
        self.assertEqual(old.read_bytes(), b'new content')
