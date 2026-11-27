"""Only a created staging record and a witnessed removal can change admission."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import durable_jobs


class DurableAdmissionOwnershipTests(unittest.TestCase):
    def old(self, path):
        stamp = time.time() - durable_jobs.EMPTY_RECORD_AGE - 3600
        os.utime(path, (stamp, stamp))

    def test_cleanup_preserves_names_that_are_not_staged_job_identities(self):
        with tempfile.TemporaryDirectory() as temporary:
            jobs = Path(temporary) / 'jobs'
            jobs.mkdir(mode=0o700)
            unrelated = jobs / 'staging-preserved-notes'
            unrelated.mkdir(mode=0o700)
            (unrelated / 'retained.txt').write_text('preserve this record')
            self.old(unrelated)
            with patch.object(durable_jobs, 'JOBS', jobs):
                durable_jobs.archive_completed(durable_jobs.MAX_HISTORY)
            self.assertTrue((unrelated / 'retained.txt').is_file(),
                            'a prefix alone is not a staged job identity')

    def test_a_failed_staging_removal_does_not_hide_a_full_history(self):
        with tempfile.TemporaryDirectory() as temporary:
            jobs = Path(temporary) / 'jobs'
            jobs.mkdir(mode=0o700)
            staged = jobs / ('staging-' + 'a' * 32)
            staged.mkdir(mode=0o700)
            (staged / 'request.json').write_text('{}')
            self.old(staged)
            finished = jobs / ('b' * 32)
            finished.mkdir(mode=0o700)
            durable_jobs.write(finished / 'request.json', {})
            durable_jobs.write(finished / 'result.json', dict(finished_unix=time.time() - 10000))
            remove = shutil.rmtree

            def denied(path, *args, **kwargs):
                if Path(path) == staged:
                    if kwargs.get('ignore_errors'):
                        return None
                    raise PermissionError('fixture removal denied')
                return remove(path, *args, **kwargs)

            with patch.object(durable_jobs, 'JOBS', jobs), \
                    patch.object(durable_jobs.shutil, 'rmtree', side_effect=denied):
                moved = durable_jobs.archive_completed(2)
            self.assertTrue(staged.is_dir(), 'the fixture must retain its failed removal')
            self.assertEqual(moved, 1, 'an unremoved staging record must still consume its slot')
            self.assertFalse(finished.exists())
            self.assertTrue((jobs.parent / 'jobs-archive' / ('b' * 32)).is_dir())

    def test_a_creation_collision_never_removes_the_existing_stage(self):
        with tempfile.TemporaryDirectory() as temporary:
            jobs = Path(temporary) / 'jobs'
            jobs.mkdir(mode=0o700)
            token = 'c' * 32
            staged = jobs / ('staging-' + token)
            staged.mkdir(mode=0o700)
            retained = staged / 'retained.txt'
            retained.write_text('preserve this record')
            with patch.object(durable_jobs, 'JOBS', jobs), \
                    patch.object(durable_jobs.uuid, 'uuid4', return_value=SimpleNamespace(hex=token)):
                with self.assertRaises(FileExistsError):
                    durable_jobs.run([sys.executable, '-B', '-c', 'pass'], 'fixture-owner')
            self.assertTrue(retained.is_file(), 'this admission never acquired the existing stage')

    def test_a_partial_copy_failure_removes_only_the_stage_it_created(self):
        with tempfile.TemporaryDirectory() as temporary:
            jobs = Path(temporary) / 'jobs'
            with patch.object(durable_jobs, 'JOBS', jobs), \
                    patch.object(durable_jobs.subprocess, 'check_output', return_value='d' * 40), \
                    patch.object(durable_jobs.shutil, 'copyfile', side_effect=OSError('fixture copy failed')), \
                    patch.object(durable_jobs.subprocess, 'run') as launch:
                with self.assertRaisesRegex(OSError, 'fixture copy failed'):
                    durable_jobs.run([sys.executable, '-B', '-c', 'pass'], 'fixture-owner')
                launch.assert_not_called()
            self.assertEqual(list(jobs.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
