"""Release outcomes must be observed through the supervisor and installer."""
from contextlib import contextmanager
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(SCRIPTS / name) for name in ('package', 'release', 'build')]
import install_release_job as installer
import release_job as job
import release_status as status

COMMIT = 'b' * 40


class ReleaseConsumerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repository = self.root / 'repository'
        (self.repository / '.git').mkdir(parents=True)
        (self.repository / '.git/secode-release-owner').write_text('fixture')
        self.status = self.root / 'status'

    def run_job(self, exit_code):
        @contextmanager
        def owned(_command, **kwargs):
            kwargs['stdout'].write('FAIL full: synthetic gate failure\n')
            kwargs['stdout'].flush()
            yield SimpleNamespace(returncode=exit_code)

        fake_heavy = SimpleNamespace(__file__=str(SCRIPTS / 'build/heavy.py'),
                                     inherited=lambda: None, owned_process=owned,
                                     wait_owned=lambda _child, _timeout: exit_code)
        def git(_root, *args):
            if args == ('status', '--porcelain'):
                return ''
            if args == ('remote', 'get-url', 'origin'):
                return 'https://github.com/StateEngine/grapt'
            return COMMIT

        argv = ['release_job.py', '--repository', str(self.repository),
                '--assets', str(self.root / 'assets.json'),
                '--output-root', str(self.root / 'output')]
        previous = Path.cwd()
        try:
            with patch.dict(sys.modules, {'heavy': fake_heavy}), \
                    patch.dict(os.environ, {'GRAPT_RELEASE_STATUS_ROOT': str(self.status)}, clear=True), \
                    patch.object(sys, 'argv', argv), patch.object(job, 'assets', return_value={}), \
                    patch.object(job, 'git', side_effect=git), patch.object(job, 'source'):
                try:
                    job.main()
                except job.subprocess.CalledProcessError as error:
                    # The command-line entrypoint's exact error handoff.
                    job.record_failure(str(error))
        finally:
            os.chdir(previous)
        return status.latest_status(self.status)

    def test_failed_gate_keeps_the_pinned_commit_exit_and_tail(self):
        row = self.run_job(23)
        self.assertIsNotNone(row)
        self.assertEqual(row['commit'], COMMIT)
        self.assertEqual(row['failure']['exit'], 23)
        self.assertIn('synthetic gate failure', row['failure']['tail'])

    def test_successful_run_supersedes_the_previous_failure(self):
        status.write_status(self.status, 'previous', {
            'run': 'previous', 'commit': 'a' * 40, 'outcome': 'failed'})
        row = self.run_job(0)
        self.assertIsNotNone(row)
        self.assertEqual(row['commit'], COMMIT)
        self.assertNotEqual(row['outcome'], 'failed')

    def test_failed_gate_names_the_phase_instead_of_the_python_executable(self):
        row = self.run_job(23)
        self.assertIn('full', row['failure']['step'])

    def test_complete_snapshot_producer_includes_the_failure_recorder(self):
        import install_schedules
        control = self.root / 'complete-controller'
        install_schedules.install_control(SCRIPTS.parent, control)
        self.assertEqual((control / 'release_status.py').read_bytes(),
                         (SCRIPTS / 'release/release_status.py').read_bytes())

    @unittest.skipUnless(os.name == 'posix', 'release controller is a Linux service')
    def test_failure_recording_reads_only_a_bounded_log_tail(self):
        import subprocess
        log = self.root / 'large.log'
        with log.open('wb') as output:
            output.seek(256 * 1024 * 1024)
            output.write(b'\nFAIL full: synthetic tail marker\n')
        script = (
            "import resource,sys; from pathlib import Path; "
            "resource.setrlimit(resource.RLIMIT_AS,(64*1024*1024,64*1024*1024)); "
            "sys.path[:0]=[sys.argv[1],sys.argv[2]]; import release_job; "
            "release_job.record_outcome('full',23,Path(sys.argv[3]),'b'*40,failed=True)"
        )
        child = subprocess.run([sys.executable, '-c', script, str(SCRIPTS / 'package'),
                                str(SCRIPTS / 'release'), str(log)],
                               env=dict(os.environ, GRAPT_RELEASE_STATUS_ROOT=str(self.status),
                                        GRAPT_RELEASE_ACTOR=''),
                               capture_output=True, text=True, timeout=10, check=False)
        self.assertEqual(child.returncode, 0, child.stderr)
        self.assertIn('synthetic tail marker', status.latest_status(self.status)['failure']['tail'])

    def test_explicit_controller_preserves_mismatched_or_linked_status_helper(self):
        for linked in (False, True):
            with self.subTest(linked=linked):
                control = self.root / ('linked' if linked else 'regular')
                control.mkdir()
                (control / 'heavy').write_text('fixture launcher')
                for name in ('release_job.py', 'native_package.py', 'windows_package.py',
                             'package_files.py', 'hidden_policy.py', 'hidden_runner.py'):
                    (control / name).write_bytes((SCRIPTS / 'package' / name).read_bytes())
                victim = self.root / ('victim-linked' if linked else 'victim-regular')
                victim.write_text('preserve controller bytes')
                target = control / 'release_status.py'
                if linked:
                    target.symlink_to(victim)
                else:
                    target.write_bytes(victim.read_bytes())
                argv = ['install_release_job.py', '--repository', str(self.repository),
                        '--assets', str(self.root / 'assets.json'),
                        '--control-directory', str(control)]
                with patch.object(sys, 'argv', argv), patch.object(Path, 'home', return_value=self.root), \
                        patch.dict(os.environ, {'SECODE_CANONICAL_REMOTE': 'https://github.com/StateEngine/grapt'}), \
                        patch.object(installer.release_job, 'assets'), \
                        patch.object(installer.release_job, 'git', side_effect=[
                            '', 'https://github.com/StateEngine/grapt']), \
                        patch.object(installer.subprocess, 'run'):
                    with self.assertRaises(ValueError):
                        installer.main()
                self.assertEqual(victim.read_text(), 'preserve controller bytes')
                self.assertEqual(target.read_text(), 'preserve controller bytes')


if __name__ == '__main__':
    unittest.main()
