"""New producer checkout creation uses the nominated parent and accepted base."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

from fixture_environment import isolated_environment


class CreateWorktreeTests(unittest.TestCase):
    def test_creation_binds_accepted_source_and_local_build_defaults(self):
        scripts = Path(__file__).resolve().parent
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source'
            subprocess.run(['git', 'init', '-q', str(source)], check=True, timeout=10)
            (source / 'crates/grapt-types').mkdir(parents=True)
            (source / 'crates/grapt-types/Cargo.toml').write_text('fixture\n')
            (source / 'scripts/build').mkdir(parents=True)
            for name in ('configure-worktree.sh', 'rustc-cache.sh', 'tmp_scratch.py',
                         'operator_paths.py'):
                shutil.copyfile(scripts / name, source / 'scripts/build' / name)
            shutil.copyfile(scripts.parent / 'build-cache.sh', source / 'scripts/build-cache.sh')
            files = ['crates/grapt-types/Cargo.toml', 'scripts/build-cache.sh',
                     'scripts/build/configure-worktree.sh', 'scripts/build/rustc-cache.sh',
                     'scripts/build/tmp_scratch.py', 'scripts/build/operator_paths.py']
            subprocess.run(['git', '-C', str(source), 'add', '--', *files], check=True, timeout=10)
            subprocess.run(['git', '-C', str(source), '-c', 'user.name=Test', '-c', 'user.email=test@localhost',
                            'commit', '-q', '-F', '-', '--', *files], input=b'accepted fixture\n', check=True, timeout=10)
            accepted = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
            tools = root / 'bin'
            tools.mkdir()
            for name, command in (('rustc', 'printf "rustc fixture-version\\n"'), ('sccache', 'exit 0')):
                path = tools / name
                path.write_text('#!/bin/sh\n' + command + '\n')
                path.chmod(0o755)
            environment = dict(isolated_environment(), PATH=str(tools) + os.pathsep + os.environ['PATH'],
                               SECODE_WORKTREE_PARENT=str(root / 'nvme'), SECODE_CACHE_DIR=str(root / 'cache'))
            subprocess.run(['bash', str(scripts / 'create-worktree.sh'), 'producer', accepted],
                           cwd=source, env=environment, check=True, timeout=20, stdout=subprocess.DEVNULL)
            checkout = root / 'nvme/producer'
            actual = subprocess.check_output(['git', '-C', str(checkout), 'rev-parse', 'HEAD'], text=True).strip()
            self.assertEqual(actual, accepted)
            config = tomllib.loads((checkout / '.cargo/config.toml').read_text())
            self.assertTrue(Path(config['build']['target-dir']).is_relative_to(root / 'cache'))
