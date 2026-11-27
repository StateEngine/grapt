"""Resume batch publication against disposable Git remotes and synthetic services."""
from contextlib import contextmanager
from dataclasses import asdict
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

import batch_pipeline as pipeline
from batch_state import load
from test_batch_land import repository, git, commit


class Interrupted(BaseException):
    """A vanished controller cannot run its error handler."""


@contextmanager
def batch():
    with repository() as (root, base, _, _), tempfile.TemporaryDirectory() as raw:
        outside = Path(raw)
        remote = outside / 'remote.git'
        git(root, 'init', '--bare', '-q', str(remote))
        git(root, 'remote', 'add', 'github', str(remote))
        git(root, 'push', '-q', 'github', base + ':refs/heads/main')
        rows = []
        for number in (11, 12):
            git(root, 'checkout', '-q', '-b', f'candidate-{number}', base)
            (root / f'feature-{number}.txt').write_text('Reviewed fixture content\n')
            head = commit(root)
            rows.append(pipeline.github.PullRequest(number, head, head))
        git(root, 'checkout', '-q', 'main')
        options = SimpleNamespace(repository=root, worktrees=outside / 'worktrees',
                                  state=outside / 'state', name='fixture',
                                  project='example/project', timeout=10, staging_timeout=10,
                                  mirror_host='fixture-mirror', mirror_path='C:/fixture/repository')
        options.worktrees.mkdir()
        options.state.mkdir(mode=0o700)
        state = dict(version=2, name='fixture', repository=str(root), project=options.project,
                     phase='admission', base=base, excluded=[], requested=[11, 12],
                     rows=[asdict(row) for row in rows],
                     mail=dict(actor='agt-fixture-controller', lead='agt-fixture-lead',
                               authors={'11': 'agt-fixture-first', '12': 'agt-fixture-second'}))
        events = dict(pushes=[], reconciled=[], mirrored=[], delivered=[], mail_down=False,
                      crash_after_push=False)
        real_git = pipeline.compose.git
        def transport(selected, *args):
            publication = args[:2] == ('push', 'github') and args[-1].endswith(':refs/heads/main')
            if publication:
                events['pushes'].append(args[-1])
            result = real_git(selected, *args)
            if publication and events['crash_after_push']:
                events['crash_after_push'] = False
                raise Interrupted()
            return result
        def reconcile(_root, _project, row, _head):
            events['reconciled'].append(row.number)
            return 'closed'
        def send(_actor, recipient, body, **_options):
            if events['mail_down'] and recipient == 'agt-fixture-lead':
                raise OSError('fixture mail unavailable')
            events['delivered'].append((recipient, body))
        with patch.object(pipeline.compose, 'git', side_effect=transport), \
                patch.object(pipeline, 'qualify', return_value=.01), \
                patch.object(pipeline.github, 'wait_gate', return_value='https://example.invalid/gate'), \
                patch.object(pipeline.github, 'admission', side_effect=lambda _r, _p, n, _b:
                             next(row for row in rows if row.number == n)), \
                patch.object(pipeline.github, 'reconcile', side_effect=reconcile), \
                patch.object(pipeline, 'mirror', side_effect=lambda *_: events['mirrored'].append(True)), \
                patch.object(pipeline.batch_notify.nightly_revert, 'send', side_effect=send):
            yield options, state, rows, events


class BatchResume(unittest.TestCase):
    def checkpoint_crash(self, options, state, rows, phase):
        save = pipeline.save
        stopped = False
        def crash(path, current):
            nonlocal stopped
            save(path, current)
            if not stopped and current['phase'] == phase:
                stopped = True
                raise Interrupted()
        with patch.object(pipeline, 'save', side_effect=crash):
            with self.assertRaises(Interrupted):
                pipeline.Pipeline(options, state).run(rows)
        self.assertTrue(stopped)
        return load(options.state / 'batch.json')

    def test_every_persisted_phase_resumes_without_republishing_main(self):
        phases = ('composition', 'staging-push', 'staging-tests', 'batch-push', 'ci',
                  'qualified', 'main-push', 'reconcile', 'mirror', 'notifications', 'complete')
        for phase in phases:
            with self.subTest(phase=phase), batch() as (options, state, rows, events):
                resumed = self.checkpoint_crash(options, state, rows, phase)
                pipeline.Pipeline(options, resumed).run(rows)
                final = load(options.state / 'batch.json')
                self.assertEqual(final['phase'], 'complete')
                self.assertEqual(len(events['pushes']), 1)
                self.assertEqual(events['reconciled'], [11, 12])
                self.assertEqual(len(events['mirrored']), 1)
                self.assertEqual(git(options.repository, 'ls-remote', '--heads', 'github',
                                     'refs/heads/main').split()[0], final['landed'])

    def test_remote_publication_before_the_local_receipt_is_discovered_on_resume(self):
        with batch() as (options, state, rows, events):
            events['crash_after_push'] = True
            with self.assertRaises(Interrupted):
                pipeline.Pipeline(options, state).run(rows)
            resumed = load(options.state / 'batch.json')
            self.assertNotIn('landed', resumed)
            pipeline.Pipeline(options, resumed).run(rows)
            self.assertEqual(len(events['pushes']), 1)
            self.assertEqual(load(options.state / 'batch.json')['phase'], 'complete')

    def test_changed_head_or_review_cannot_publish_the_qualified_batch(self):
        for field in ('head', 'accepted'):
            with self.subTest(field=field), batch() as (options, state, rows, events):
                resumed = self.checkpoint_crash(options, state, rows, 'main-push')
                changed = asdict(rows[0])
                changed[field] = 'f' * 40
                moved = pipeline.github.PullRequest(**changed)
                with patch.object(pipeline.github, 'admission', return_value=moved):
                    with self.assertRaises(ValueError):
                        pipeline.Pipeline(options, resumed).run(rows)
                self.assertEqual(events['pushes'], [])
                self.assertEqual(events['reconciled'], [])

    def test_main_drift_cannot_publish_a_batch_built_on_the_old_base(self):
        with batch() as (options, state, rows, events):
            resumed = self.checkpoint_crash(options, state, rows, 'main-push')
            (options.repository / 'new-main.txt').write_text('Concurrent accepted work\n')
            moved = commit(options.repository)
            git(options.repository, 'push', '-q', 'github', moved + ':refs/heads/main')
            with self.assertRaises(ValueError):
                pipeline.Pipeline(options, resumed).run(rows)
            self.assertEqual(events['pushes'], [])
            self.assertEqual(git(options.repository, 'ls-remote', '--heads', 'github',
                                 'refs/heads/main').split()[0], moved)

    def test_mail_debt_survives_without_repeating_confirmed_publication_or_delivery(self):
        with batch() as (options, state, rows, events):
            events['mail_down'] = True
            pipeline.Pipeline(options, state).run(rows)
            resumed = load(options.state / 'batch.json')
            self.assertEqual(resumed['phase'], 'notifications')
            self.assertTrue(any(set(row['targets']) - set(row['delivered'])
                                for row in resumed['notifications']))
            confirmed = list(events['delivered'])
            events['mail_down'] = False
            pipeline.Pipeline(options, resumed).run(rows)
            self.assertEqual(load(options.state / 'batch.json')['phase'], 'complete')
            self.assertEqual(len(events['pushes']), 1)
            self.assertEqual(events['reconciled'], [11, 12])
            self.assertEqual(len(events['mirrored']), 1)
            for delivery in confirmed:
                self.assertEqual(events['delivered'].count(delivery), 1)


if __name__ == '__main__':
    unittest.main()
