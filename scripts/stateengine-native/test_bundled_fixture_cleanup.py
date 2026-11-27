"""Private cleanup must preserve foreign files and report producer refusals."""
import json
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import Mock, patch

from bundled_mcp_fixture import Fixture, remove_private_home
from windows_bound_mcp import isolated_environment


class PrivateCleanup(unittest.TestCase):
    def test_windows_readonly_git_file_is_removed_inside_its_private_home(self):
        with tempfile.TemporaryDirectory() as scratch:
            home = Path(scratch) / 'home'
            home.mkdir()
            target = home / 'git-object'
            target.write_bytes(b'private fixture')
            target.chmod(stat.S_IREAD)
            def removal(root, onerror):
                self.assertEqual(root, home)
                onerror(os.unlink, target, (PermissionError, PermissionError('readonly'), None))
            with patch('bundled_mcp_fixture.sys.platform', 'win32'), patch(
                    'bundled_mcp_fixture.shutil.rmtree', side_effect=removal):
                remove_private_home(home)
            self.assertFalse(target.exists())

    def test_readonly_recovery_rejects_outside_files_and_other_errors(self):
        with tempfile.TemporaryDirectory() as scratch:
            home = Path(scratch) / 'home'
            home.mkdir()
            outside = Path(scratch) / 'outside'
            outside.write_bytes(b'untouched')
            def removal(root, onerror):
                action = Mock()
                with self.assertRaises(RuntimeError):
                    onerror(action, outside, (PermissionError, PermissionError('readonly'), None))
                with self.assertRaises(OSError):
                    onerror(action, home, (OSError, OSError('other failure'), None))
                action.assert_not_called()
            with patch('bundled_mcp_fixture.sys.platform', 'win32'), patch(
                    'bundled_mcp_fixture.shutil.rmtree', side_effect=removal):
                remove_private_home(home)
            self.assertEqual(outside.read_bytes(), b'untouched')

    def test_public_producer_refusal_retains_exit_and_safe_class_without_stdout_payload(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            environment = isolated_environment(root / 'bundle', root, {})
            profile = dict(repo_root=str(root / 'home/project'),
                registry_root=environment['STATEENGINE_REGISTRY_ROOT'], project='private',
                agent_id='agt-private', human_id='human-private')
            fixture = Fixture(root / 'bundle', environment, profile)
            for reason, expected in [('detach-spawn-failed', 'detach-spawn-failed'),
                                     ('PRIVATE', 'producer-command-refused')]:
                reply = Mock(returncode=1, stdout=json.dumps(dict(reason=reason, secret='PRIVATE')).encode(), stderr=b'')
                with patch('bundled_mcp_fixture.subprocess.run', return_value=reply):
                    with self.assertRaises(RuntimeError) as failure:
                        fixture.cli(['up', '--detach'])
                self.assertIn('exit 1, ' + expected, str(failure.exception))
                self.assertNotIn('PRIVATE', str(failure.exception))
            self.assertFalse((root / 'producer-refusal.stdout').exists())


if __name__ == '__main__':
    unittest.main()
