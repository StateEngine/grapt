"""Package staging cannot advertise a partial or mixed-source release."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import os
import shutil
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'package'))
import release_job
import windows_package


class ReleaseJobTests(unittest.TestCase):
    def test_installed_job_keeps_its_controller_when_subject_helpers_change(self):
        with tempfile.TemporaryDirectory(prefix='secode-release-controller-') as directory:
            outer=Path(directory); root=outer/'subject'; control=outer/'controller'
            (root/'.git').mkdir(parents=True); control.mkdir()
            (root/'.git/secode-release-owner').write_text('Dedicated test subject\n')
            helpers=root/'scripts/build'; helpers.mkdir(parents=True)
            selected=outer/'selected.txt'
            (helpers/'heavy.py').write_text('from pathlib import Path\nPath('+repr(str(selected))+').write_text("subject")\nraise RuntimeError("mutable subject helper imported")\n')
            (control/'heavy.py').write_text('from pathlib import Path\nPath('+repr(str(selected))+').write_text("controller")\ndef inherited(): return None\n')
            shutil.copyfile(Path(release_job.__file__),control/'release_job.py')
            asset=outer/'asset'; asset.write_text('fixture asset\n')
            inputs={target:{key:str(asset) for key in release_job.ASSETS}
                    for target in release_job.TARGETS}
            inputs[release_job.TARGETS[1]]['python_runtime_archive']=str(asset)
            config=outer/'assets.json'; config.write_text(json.dumps(inputs))
            result=subprocess.run([sys.executable,'-B',str(control/'release_job.py'),
                '--repository',str(root),'--assets',str(config),'--output-root',str(outer/'output'),
                '--build-only','--staging',str(outer/'staging'),'--commit','invalid'],
                capture_output=True,text=True,timeout=10)
            self.assertNotEqual(result.returncode,0)
            self.assertIn('Internal build requires',result.stderr)
            self.assertEqual(selected.read_text(),'controller')

    def test_staging_requires_both_targets_from_the_unchanged_commit(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            root = directory / 'source'
            root.mkdir()
            subprocess.run(['git', 'init', '-q', str(root)], check=True, timeout=10)
            tracked = root / 'source'
            tracked.write_text('reviewed source\n')
            subprocess.run(['git', '-C', str(root), 'add', '--', 'source'], check=True, timeout=10)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Test', '-c', 'user.email=test@localhost',
                            'commit', '-qm', 'fixture', '--', 'source'], check=True, timeout=10)
            commit = release_job.git(root, 'rev-parse', 'HEAD')
            inputs = {target: {key: '/fixture/' + key for key in release_job.ASSETS}
                      for target in release_job.TARGETS}
            for corrupt in (True, False):
                output = directory / ('interrupted' if corrupt else 'complete')
                output.mkdir()
                def run(command):
                    target = command[command.index('--target') + 1]
                    if command[1].endswith('package.py'):
                        package = output / target
                        package.mkdir()
                        (package / 'release-manifest.json').write_text(json.dumps(
                            dict(target=target, engine_version=commit)))
                        if corrupt:
                            tracked.write_text('source moved during packaging\n')
                if corrupt:
                    with self.assertRaisesRegex(RuntimeError, 'checkout changed'):
                        release_job.stage(root, output, commit, inputs, 3, run)
                    self.assertFalse((output / 'release-job.json').exists())
                    self.assertFalse((output / release_job.TARGETS[1]).exists())
                    tracked.write_text('reviewed source\n')
                else:
                    native_calls = []
                    def native(_root, _built, package, head, _inputs):
                        native_calls.append(head)
                        package.mkdir()
                        (package / 'release-manifest.json').write_text(json.dumps(
                            dict(target=release_job.TARGETS[1], engine_version=head)))
                    release_job.stage(root, output, commit, inputs, 3, run, native)
                    self.assertEqual(native_calls, [commit])
                    record = json.loads((output / 'release-job.json').read_text())
                    self.assertEqual(record['source_commit'], commit)
                    self.assertEqual(record['status'], 'staged')
                    self.assertTrue(record['native_acceptance'].startswith('pending:'))

    @unittest.skipIf(os.name == 'nt', 'Wrong-host refusal is exercised on the Linux build box')
    def test_native_worker_refuses_linux_before_assembling_windows_payload(self):
        with self.assertRaisesRegex(RuntimeError, 'native Windows host'):
            windows_package.main()

    def test_cache_root_prefers_renamed_home_and_falls_back_to_legacy(self):
        for existing, expected in [(('grapt', 'secode'), 'grapt'),
                                   (('secode',), 'secode'), ((), 'grapt')]:
            with tempfile.TemporaryDirectory() as directory:
                home = Path(directory) / 'home'
                base = home / '.cache'
                for name in existing:
                    (base / name).mkdir(parents=True)
                with mock.patch.object(Path, 'home', return_value=home), \
                        mock.patch.dict(os.environ, {}, clear=True):
                    self.assertEqual(release_job.cache_root(), base / expected)
