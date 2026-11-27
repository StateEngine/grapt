"""Batch composition and review decisions against disposable Git repositories."""
from contextlib import contextmanager
import json
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import batch_compose as compose
import batch_github as github
import publication_scan
from fixture_environment import isolated_environment

NAME = 'Fixture Author'
EMAIL = '123+fixture@users.noreply.github.com'
WORKSPACE = '[workspace]\nmembers = ["crates/app"]\n'


def git(root, *args):
    return subprocess.check_output(['git', *args], cwd=root, text=True,
                                   env=isolated_environment()).strip()


def commit(root, subject='fix(fixture): change input'):
    git(root, 'add', '--', '.')
    git(root, 'commit', '-q', '-m', subject + '\n\n' + compose.TRAILER)
    return git(root, 'rev-parse', 'HEAD')


@contextmanager
def repository():
    with tempfile.TemporaryDirectory(prefix='batch-fixture-') as temporary:
        root = Path(temporary)
        git(root, 'init', '-q', '-b', 'main')
        git(root, 'config', 'user.name', NAME)
        git(root, 'config', 'user.email', EMAIL)
        for path, content in {
                # The license digest refuses a memberless workspace, so seed one member.
                'Cargo.toml': WORKSPACE,
                # Like the real repository: running the copied licenses.py writes a cache.
                '.gitignore': '__pycache__/\n',
                'Cargo.lock': 'version = 3\n[[package]]\nname = "app"\nversion = "0.1.0"\n',
                'crates/app/Cargo.toml': '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n',
                'about.toml': '', 'deny.toml': '',
                'scripts/release/license-overrides.toml': 'package = []\n',
        }.items():
            destination = root / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(content)
        license_source = Path(compose.__file__).resolve().parents[1] / 'release/licenses.py'
        shutil.copyfile(license_source, root / 'scripts/release/licenses.py')
        digest = runpy.run_path(str(license_source))['dependency_digest']
        def notice():
            destination = root / compose.NOTICE
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text('License register\n<!-- Dependency inputs SHA-256: ' +
                                   digest(root) + ' -->\nUnchanged license text\n')
        notice()
        base = commit(root, 'build(fixture): seed repository')
        with patch.multiple(publication_scan, REQUIRED_AUTHOR_NAME=NAME, REQUIRED_AUTHOR_EMAIL=EMAIL):
            yield root, base, notice, digest


class BatchTests(unittest.TestCase):
    def test_cli_plan_does_not_create_state_or_change_the_repository(self):
        with repository() as (root, _, _, _):
            state = root.parent / (root.name + '-attempt')
            script = Path(compose.__file__).with_name('batch_land.py')
            result = subprocess.run([sys.executable, '-B', str(script), 'fixture', '--pr', '1',
                                     '--project', 'example/project', '--repository', str(root),
                                     '--worktrees', str(root.parent), '--state', str(state)],
                                    env=isolated_environment(), capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['pull_requests'], [1])
            self.assertFalse(state.exists())
            self.assertEqual(git(root, 'status', '--porcelain'), '')

    def test_legal_digest_merges_without_losing_either_side(self):
        with repository() as (root, base, notice, digest):
            git(root, 'checkout', '-q', '-b', 'candidate')
            (root / 'scripts/release/license-overrides.toml').write_text('# Candidate input\npackage = []\n')
            notice()
            head = commit(root)
            git(root, 'checkout', '-q', 'main')
            (root / 'about.toml').write_text('# Integrated input\n')
            notice()
            main = commit(root)
            git(root, 'checkout', '-q', '--detach', main)
            result = compose.merge(root, 1, head, main)
            self.assertEqual(git(root, 'show', '-s', '--format=%P', result).split(), [main, head])
            self.assertIn(digest(root), (root / compose.NOTICE).read_text())
            self.assertEqual(compose.scan(root, main), [])
            self.assertEqual(git(root, 'status', '--porcelain'), '')

    def test_unpublishable_history_is_squashed_as_a_whole_and_final_content_is_scanned(self):
        with repository() as (root, base, _, _):
            git(root, 'checkout', '-q', '-b', 'candidate')
            (root / 'temporary.txt').write_text('Temporary content\n')
            commit(root, 'temporary subject')
            (root / 'temporary.txt').unlink()
            (root / 'feature.txt').write_text('Reviewed content\n')
            head = commit(root)
            git(root, 'checkout', '-q', '--detach', base)
            result = compose.merge(root, 2, head, base)
            self.assertEqual(git(root, 'show', '-s', '--format=%P', result), base)
            self.assertEqual(compose.scan(root, base), [])
            self.assertEqual((root / 'feature.txt').read_text(), 'Reviewed content\n')
            self.assertFalse((root / 'temporary.txt').exists())

    def test_accept_survives_only_an_unedited_merge_of_main(self):
        with repository() as (root, base, _, _):
            git(root, 'checkout', '-q', '-b', 'candidate')
            (root / 'candidate.txt').write_text('Candidate\n')
            accepted = commit(root)
            git(root, 'checkout', '-q', 'main')
            (root / 'integrated.txt').write_text('Integrated\n')
            main = commit(root)
            git(root, 'checkout', '-q', 'candidate')
            git(root, 'merge', '--no-ff', '-m', 'merge(fixture): integrate main', main)
            head = git(root, 'rev-parse', 'HEAD')
            compose.reviewed_head(root, accepted, head, main)
            git(root, 'remote', 'add', 'github', str(root))
            git(root, 'update-ref', 'refs/pull/1/head', head)
            row = dict(state='open', draft=False, base={'ref': 'main'},
                       head=dict(sha=head, repo={'full_name': 'example/project'}))
            review = dict(id=1, submitted_at='2026-01-01T00:00:00Z', state='COMMENTED',
                          body='0 Important. ACCEPT.', commit_id=accepted)
            with patch.object(github, 'api', side_effect=lambda _, __, suffix:
                              [review] if suffix.endswith('reviews?per_page=100') else row):
                admitted = github.admission(root, 'example/project', 1, main)
                self.assertEqual(admitted.accepted, accepted)
                review['state'] = 'DISMISSED'
                with self.assertRaisesRegex(ValueError, 'ACCEPT'):
                    github.admission(root, 'example/project', 1, main)
            (root / 'candidate.txt').write_text('Unreviewed correction\n')
            moved = commit(root)
            with self.assertRaisesRegex(ValueError, 'non-merge'):
                compose.reviewed_head(root, accepted, moved, main)

    def test_success_requires_the_exact_head_and_all_mandatory_ci_jobs(self):
        run = dict(id=1, head_sha='a'*40, head_branch='batch/fixture', event='push',
                   status='completed', conclusion='success', html_url='https://example.invalid/run')
        def api(root, repository, path):
            if '/runs?' in path:
                return {'workflow_runs': [run]}
            if '/jobs?' in path:
                return {'jobs': [dict(name=name, conclusion='success') for name in ('changes', 'linux')]}
            return run
        with patch.object(github, 'api', api):
            with self.assertRaisesRegex(RuntimeError, 'incomplete'):
                github.wait_gate(Path.cwd(), 'example/project', 'batch/fixture', 'a'*40, 1)


if __name__ == '__main__':
    unittest.main()
