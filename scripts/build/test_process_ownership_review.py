"""Cleanup must retain ownership when a numeric process identity is reused."""
import contextlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import heavy


class ProcessOwnershipReviewTests(unittest.TestCase):
    def test_public_wait_paths_retain_the_leader_until_context_cleanup(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(heavy, 'QUEUE_PATH', Path(directory) / 'queue'):
            for operation in ('poll', 'wait', 'communicate'):
                with self.subTest(operation=operation):
                    process = None
                    try:
                        with heavy.owned_process([sys.executable, '-c', 'print("done")'],
                                                 stdout=subprocess.PIPE) as process:
                            if operation == 'poll':
                                deadline = time.monotonic() + 5
                                while process.poll() is None:
                                    self.assertLess(time.monotonic(), deadline)
                                    time.sleep(.01)
                            elif operation == 'wait':
                                process.wait(timeout=5)
                            else:
                                process.communicate(timeout=5)
                            self.assertEqual(process.returncode, 0)
                            retained = os.waitid(os.P_PID, process.pid,
                                                 os.WEXITED | os.WNOHANG | os.WNOWAIT)
                            self.assertIsNotNone(retained, 'the observed leader was not retained')
                        with self.assertRaises(ChildProcessError):
                            os.waitpid(process.pid, os.WNOHANG)
                    finally:
                        if process is not None and process.stdout is not None:
                            process.stdout.close()

    def test_failed_spawn_preserves_the_ownership_scope(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(heavy, 'QUEUE_PATH', Path(directory) / 'queue'):
            with self.assertRaises(FileNotFoundError):
                with heavy.owned_process([str(Path(directory) / 'missing')]):
                    self.fail('missing executable was admitted')
            with heavy.owned_process([sys.executable, '-c', 'pass']) as child:
                self.assertEqual(child.wait(timeout=5), 0)

    def test_reaped_leader_cannot_redirect_cleanup_to_a_foreign_group(self):
        with subprocess.Popen([sys.executable, '-c', 'pass'],
                              start_new_session=True) as leader, \
                subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'],
                                 start_new_session=True) as sentinel:
            leader.wait(timeout=5)
            original = leader.pid
            try:
                leader.pid = sentinel.pid
                with patch.object(heavy, 'STOP_GRACE_SECONDS', .05):
                    # A refusal is safe; signalling the unrelated group is not.
                    with contextlib.suppress(RuntimeError):
                        heavy.stop_group(leader)
                with self.assertRaises(subprocess.TimeoutExpired):
                    sentinel.wait(timeout=.2)
            finally:
                leader.pid = original
                sentinel.kill()
                sentinel.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()
