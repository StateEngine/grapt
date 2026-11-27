import os
import shutil
import tempfile
import time
from pathlib import Path
import unittest
from unittest.mock import patch

import tmp_scratch


class TmpScratchTests(unittest.TestCase):
    def test_the_run_directory_becomes_the_job_temp(self):
        with tempfile.TemporaryDirectory() as base:
            with patch.dict(os.environ, {'SECODE_TEST_TMP_BASE': base}):
                environment, run_dir = tmp_scratch.job_environment({'PATH': '/bin'})
                self.assertIsNotNone(run_dir)
                self.assertTrue(os.path.isdir(run_dir))
                for key in tmp_scratch.KEYS:
                    self.assertEqual(environment[key], run_dir)
                self.assertEqual(environment['PATH'], '/bin')
                self.assertTrue(os.path.basename(run_dir).startswith('run-'))
                shutil.rmtree(run_dir)

    def test_back_to_back_runs_get_distinct_directories(self):
        with tempfile.TemporaryDirectory() as base:
            with patch.dict(os.environ, {'SECODE_TEST_TMP_BASE': base}):
                _, first = tmp_scratch.job_environment({})
                _, second = tmp_scratch.job_environment({})
                self.assertNotEqual(first, second)
                shutil.rmtree(first)
                shutil.rmtree(second)

    def test_the_base_degrades_to_the_caller_environment(self):
        environment = {'TMPDIR': '/kept'}
        with patch.object(tmp_scratch, 'resolve_base', return_value=None):
            self.assertEqual(tmp_scratch.job_environment(environment), (environment, None))

    def test_the_default_base_follows_the_operator_data_root(self):
        with tempfile.TemporaryDirectory() as root:
            with patch.object(tmp_scratch.operator_paths, 'data_root', return_value=Path(root)):
                self.assertEqual(tmp_scratch.resolve_base(), Path(root) / 'test-tmp')

    def test_an_unwritable_base_degrades_to_none(self):
        with tempfile.TemporaryDirectory() as root:
            blocker = Path(root) / 'blocker'
            blocker.write_text('a file cannot host the scratch base')
            with patch.dict(os.environ, {'SECODE_TEST_TMP_BASE': str(blocker / 'scratch')}):
                self.assertIsNone(tmp_scratch.resolve_base())

    def test_the_sweep_removes_only_idle_run_directories(self):
        with tempfile.TemporaryDirectory() as base:
            idle = os.path.join(base, 'run-1-1')
            fresh = os.path.join(base, 'run-1-2')
            foreign = os.path.join(base, 'not-a-run')
            for path in (idle, fresh, foreign):
                os.mkdir(path)
            idle_time = time.time() - tmp_scratch.SWEEP_SECONDS - 60
            os.utime(idle, (idle_time, idle_time))
            tmp_scratch.sweep(base)
            self.assertFalse(os.path.exists(idle))
            self.assertTrue(os.path.exists(fresh))
            self.assertTrue(os.path.exists(foreign))


if __name__ == '__main__':
    unittest.main()
