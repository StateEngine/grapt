import contextlib
from datetime import datetime, timedelta, timezone
import errno
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import gate_evidence as evidence


class ArchiveSafetyTests(unittest.TestCase):
    def group(self, root, ordinal, size=4, days=0, owned=False):
        stamp = (datetime.now(timezone.utc) - timedelta(days=days)).strftime('%Y%m%dT%H%M%SZ')
        group = root / f'{stamp}-{ordinal}'
        group.mkdir(parents=True)
        (group / 'gate.log').write_bytes(b'x' * size)
        if owned:
            (group / evidence.ARCHIVE_MARKER).write_bytes(evidence.ARCHIVE_MARKER_CONTENT)
        return group

    def test_archive_root_symlink_preserves_the_external_original(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            external = root / 'outside'
            original = self.group(external, 1, days=8) / 'gate.log'
            archive = root / 'archive'
            archive.symlink_to(external, target_is_directory=True)
            log = root / 'current.log'
            log.write_bytes(b'current failure')
            with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(archive)}), \
                    contextlib.redirect_stderr(io.StringIO()):
                evidence.archive_failure(log)
            self.assertTrue(original.is_file(), 'retention deleted evidence outside its root')
            self.assertEqual(original.read_bytes(), b'xxxx')
            self.assertEqual(log.read_bytes(), b'current failure')

    def test_prune_preserves_an_original_supplied_from_a_stamped_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = self.group(root, 1, days=8) / 'gate.log'
            with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(root)}), \
                    contextlib.redirect_stderr(io.StringIO()):
                evidence.archive_failure(original)
            self.assertTrue(original.is_file(), 'retention pruned its original input')
            self.assertEqual(original.read_bytes(), b'xxxx')

    def test_group_symlink_preserves_external_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            external = root / 'outside'
            external.mkdir()
            sentinel = external / 'gate.log'
            sentinel.write_bytes(b'preserve')
            archive = root / 'archive'
            archive.mkdir()
            name = (datetime.now(timezone.utc) - timedelta(days=8)).strftime('%Y%m%dT%H%M%SZ')
            (archive / f'{name}-1').symlink_to(external, target_is_directory=True)
            evidence.prune_archive(archive)
            self.assertEqual(sentinel.read_bytes(), b'preserve')

    def test_a_timestamp_does_not_authorize_deleting_unowned_originals(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'archive'
            original = self.group(archive, 1, days=8) / 'gate.log'
            original.write_bytes(b'original evidence, never archived')
            log = root / 'current.log'
            log.write_bytes(b'new failure')
            with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(archive)}), \
                    contextlib.redirect_stderr(io.StringIO()):
                evidence.archive_failure(log)
            self.assertTrue(original.is_file(), 'a timestamp was treated as archive ownership')
            self.assertEqual(original.read_bytes(), b'original evidence, never archived')
            self.assertEqual(log.read_bytes(), b'new failure')

    def test_prune_enforces_group_count_when_logs_fit_the_byte_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for ordinal in range(3):
                self.group(root, ordinal, owned=True)
            with patch.object(evidence, 'ARCHIVE_MAX_GROUPS', 2), \
                    patch.object(evidence, 'ARCHIVE_MAX_BYTES', 100):
                evidence.prune_archive(root)
            self.assertLessEqual(len(list(root.iterdir())), 2)

    def test_byte_limit_accounts_for_groups_beyond_the_scan_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for ordinal in range(3):
                self.group(root, ordinal, owned=True)
            with patch.object(evidence, 'ARCHIVE_MAX_GROUPS', 2), \
                    patch.object(evidence, 'ARCHIVE_MAX_BYTES', 4):
                evidence.prune_archive(root)
            self.assertLessEqual(sum(p.stat().st_size for p in root.rglob('gate.log')), 4)

    def test_full_disk_reports_failure_and_keeps_originals(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'archive'
            log = root / 'gate.log'
            log.write_bytes(b'original failure')
            sidecar = root / 'gate.log.deps.json'
            sidecar.write_bytes(b'{}')

            def full_disk(source, destination):
                Path(destination).write_bytes(b'partial')
                raise OSError(errno.ENOSPC, os.strerror(errno.ENOSPC))

            stderr = io.StringIO()
            with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(archive)}), \
                    patch.object(evidence.shutil, 'copy2', side_effect=full_disk), \
                    contextlib.redirect_stderr(stderr):
                evidence.archive_failure(log)
            self.assertEqual(log.read_bytes(), b'original failure')
            self.assertEqual(sidecar.read_bytes(), b'{}')
            self.assertIn(os.strerror(errno.ENOSPC), stderr.getvalue())

    def test_partial_copy_failure_does_not_bypass_the_archive_byte_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'archive'
            self.group(archive, 1, owned=True)
            log = root / 'new.log'
            log.write_bytes(b'current failure')

            def full_disk(source, destination):
                Path(destination).write_bytes(b'half')
                raise OSError(errno.ENOSPC, os.strerror(errno.ENOSPC))

            with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(archive)}), \
                    patch.object(evidence, 'ARCHIVE_MAX_BYTES', 4), \
                    patch.object(evidence.shutil, 'copy2', side_effect=full_disk), \
                    contextlib.redirect_stderr(io.StringIO()):
                evidence.archive_failure(log)
            self.assertLessEqual(sum(p.stat().st_size for p in archive.rglob('*') if p.is_file()), 4)
            self.assertEqual(log.read_bytes(), b'current failure')

    @unittest.skipUnless(os.name == 'posix', 'requires POSIX directory permissions')
    def test_failed_prune_is_visible_instead_of_claiming_success(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'archive'
            stale = self.group(archive, 1, days=8, owned=True)
            log = root / 'current.log'
            log.write_bytes(b'failure')
            stale.chmod(0o555)
            stderr = io.StringIO()
            try:
                with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(archive)}), \
                        contextlib.redirect_stderr(stderr):
                    evidence.archive_failure(log)
                self.assertTrue((stale / 'gate.log').is_file())
                self.assertIn(os.strerror(errno.EACCES), stderr.getvalue())
                self.assertEqual(log.read_bytes(), b'failure')
            finally:
                stale.chmod(0o755)
