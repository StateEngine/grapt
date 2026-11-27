"""Remote acknowledgements make closure and detached handoff safe to resume."""
from contextlib import contextmanager
from copy import deepcopy
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import batch_github as github
import batch_land
import batch_notify


HEAD, LANDED = 'a' * 40, 'b' * 40
ROW = github.PullRequest(11, HEAD, HEAD)


@contextmanager
def remote(*, merged=False, crash=None, move_after_comment=False):
    pull = dict(state='closed' if merged else 'open', merged=merged, head=dict(sha=HEAD))
    comments, writes = [], []
    interrupted = False
    def api(_root, _project, suffix):
        return deepcopy(comments if '/comments?' in suffix else pull)
    def write(_root, _project, suffix, method, value):
        nonlocal interrupted
        writes.append((method, suffix))
        if method == 'POST':
            comments.append(dict(body=value['body']))
            if move_after_comment:
                pull['head']['sha'] = 'c' * 40
        elif method == 'PATCH':
            pull['state'] = value['state']
        if method == crash and not interrupted:
            interrupted = True
            raise ConnectionError('remote committed before acknowledgement was lost')
        return {}
    with patch.object(github, 'api', side_effect=api), patch.object(github, 'write', side_effect=write):
        yield pull, comments, writes


class BatchReconciliation(unittest.TestCase):
    def test_an_already_merged_pull_request_needs_no_comment_or_close(self):
        with remote(merged=True) as (_, comments, writes):
            self.assertEqual(github.reconcile(Path.cwd(), 'example/project', ROW, LANDED), 'merged')
            self.assertEqual(comments, [])
            self.assertEqual(writes, [])

    def test_a_still_open_squash_landed_pull_request_is_closed_once(self):
        with remote() as (pull, comments, writes):
            for _ in range(2):
                self.assertEqual(github.reconcile(Path.cwd(), 'example/project', ROW, LANDED), 'closed')
            self.assertEqual(pull['state'], 'closed')
            self.assertEqual(len(comments), 1)
            self.assertIn(LANDED, comments[0]['body'])
            self.assertEqual([method for method, _ in writes], ['POST', 'PATCH'])

    def test_lost_comment_or_close_ack_is_read_back_before_retrying(self):
        for method in ('POST', 'PATCH'):
            with self.subTest(method=method), remote(crash=method) as (pull, comments, writes):
                with self.assertRaises(ConnectionError):
                    github.reconcile(Path.cwd(), 'example/project', ROW, LANDED)
                self.assertEqual(github.reconcile(Path.cwd(), 'example/project', ROW, LANDED), 'closed')
                self.assertEqual(pull['state'], 'closed')
                self.assertEqual(len(comments), 1)
                self.assertEqual([kind for kind, _ in writes], ['POST', 'PATCH'])

    def test_an_unlanded_new_head_is_never_closed(self):
        for changed_late in (False, True):
            with self.subTest(changed_late=changed_late), \
                    remote(move_after_comment=changed_late) as (pull, _, writes):
                if not changed_late:
                    pull['head']['sha'] = 'c' * 40
                with self.assertRaises(ValueError):
                    github.reconcile(Path.cwd(), 'example/project', ROW, LANDED)
                self.assertEqual(pull['state'], 'open')
                self.assertNotIn('PATCH', [method for method, _ in writes])


class BatchDetachedHandoff(unittest.TestCase):
    def test_execute_and_resume_hand_the_controller_to_an_independent_job(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            repository, worktrees = root / 'repository', root / 'worktrees'
            repository.mkdir()
            worktrees.mkdir()
            common = ['fixture', '--project', 'example/project', '--repository', str(repository),
                      '--worktrees', str(worktrees), '--state', str(root / 'state'),
                      '--mirror-host', 'fixture-mirror', '--mirror-path', 'C:/fixture/repository']
            for mode in (['--execute', '--pr', '11', '--actor', 'agt-fixture-controller'], ['--resume']):
                with self.subTest(mode=mode), \
                        patch.object(batch_land.heavy, 'inherited', return_value=None), \
                        patch.object(batch_land.durable_jobs, 'active', return_value=False), \
                        patch.object(batch_land.durable_jobs, 'run', return_value=0) as detach, \
                        patch.object(batch_land, 'state_for') as foreground:
                    self.assertEqual(batch_land.main([*common, *mode]), 0)
                    detach.assert_called_once()
                    self.assertIs(detach.call_args.kwargs.get('detach'), True)
                    self.assertEqual(detach.call_args.args[0][-len(mode):], mode)
                    foreground.assert_not_called()


class BatchNotificationAcknowledgements(unittest.TestCase):
    def test_lost_delivery_ack_reuses_one_remote_identity_per_recipient(self):
        state = dict(name='fixture', requested=[11], mail={
            'actor': 'agt-fixture-controller', 'lead': 'agt-fixture-coordinator',
            'authors': {'11': 'agt-fixture-author'}})
        batch_notify.enqueue(state, '1/gate/receipt', 'gate', 'Qualified', [11])
        remote_deliveries, keys, checkpoints = [], [], []
        interrupted = False

        def send(actor, recipient, body, **options):
            nonlocal interrupted
            key = options.get('idempotency_key')
            keys.append((recipient, key))
            identity = (actor, recipient, key)
            if key is None or identity not in remote_deliveries:
                remote_deliveries.append(identity)
            if not interrupted:
                interrupted = True
                raise ConnectionError('delivery committed before acknowledgement was lost')

        def persist():
            checkpoints.append(deepcopy(state))

        with patch.object(batch_notify.nightly_revert, 'send', side_effect=send):
            self.assertFalse(batch_notify.flush(state, persist))
            self.assertTrue(batch_notify.flush(state, persist))
            self.assertTrue(batch_notify.flush(state, persist))
        self.assertTrue(checkpoints)
        self.assertEqual(len(remote_deliveries), 2,
                         'a resumed notification must not duplicate accepted remote mail')
        self.assertEqual({recipient for recipient, _ in keys},
                         {'agt-fixture-author', 'agt-fixture-coordinator'})
        self.assertTrue(all(isinstance(key, str) and key for _, key in keys),
                        'the remote adapter needs an explicit stable idempotency identity')
        self.assertEqual(keys[0], keys[1], 'retry changed the recipient delivery identity')
        self.assertNotEqual(keys[1][1], keys[2][1], 'distinct recipients share one delivery identity')


if __name__ == '__main__':
    unittest.main()
