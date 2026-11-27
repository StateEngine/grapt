"""Real fake-cache subprocesses remain independent of an outer RAM gate."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class FixtureEnvironmentTests(unittest.TestCase):
    def test_nested_fake_cache_suite_ignores_outer_run_claims(self):
        with tempfile.TemporaryDirectory(prefix='fixture-environment-') as directory:
            root = Path(directory)
            scripts = root / 'scripts'
            controls = scripts / 'build'
            source = Path(__file__).resolve().parent
            shutil.copytree(source, controls, ignore=shutil.ignore_patterns('__pycache__'))
            for name in ['build-cache.sh', 'gate.sh', 'shared-cargo.sh']:
                shutil.copyfile(source.parent / name, scripts / name)
            (scripts/'release').mkdir()
            for name in ('windows_version_worker.py', 'release_status.py'):
                shutil.copyfile(source.parent/'release'/name, scripts/'release'/name)
            (scripts/'package').mkdir()
            for name in ('release_job.py','native_package.py','windows_package.py',
                         'package_files.py','hidden_policy.py','hidden_runner.py'):
                shutil.copyfile(source.parent/'package'/name,scripts/'package'/name)
            subprocess.run(['git', 'init', '-q', str(root)], check=True, timeout=10)
            ram = root / 'ram'
            ram.mkdir(mode=0o700)
            # Confine the RAM-root classification to this disposable copy;
            # no test creates a fake reservation in the host's governed root.
            policy = controls / 'ram_targets.py'
            original = "ROOT = Path('/tmp/secode-ramtarget')"
            text = policy.read_text()
            self.assertEqual(text.count(original), 1)
            policy.write_text(text.replace(original, f'ROOT = Path({str(ram)!r})'))
            environment = dict(os.environ, TMPDIR=str(ram), TMP=str(ram), TEMP=str(ram),
                               SECODE_RAM_TARGET_RUN=str(ram / 'outer-run'),
                               SECODE_RAM_DISK_CACHE=str(root / 'outer-cache'),
                               SECODE_HEAVY_LEASE='stale-outer-claim',
                               SECODE_CACHE_ADMISSION_PROCESS='stale-outer-process')
            result = subprocess.run(
                [sys.executable, '-B', '-m', 'unittest', 'discover',
                 '-s', str(controls), '-p', 'test_cache.py'],
                cwd=root, env=environment, capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertRegex(result.stderr, r'Ran [1-9][0-9]* tests')
            self.assertIn('\nOK\n', result.stderr)
