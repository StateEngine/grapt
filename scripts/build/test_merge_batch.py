"""Reviewed source composition, split isolation and uncertain publication boundaries."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

import merge_batch
import merge_batch_driver
from fixture_environment import isolated_environment
from heavy_queue import identity
from test_landing import command, commit


class MergeBatchTests(unittest.TestCase):
    def test_cancellation_joins_qualification_and_preserves_uncertain_publication(self):
        bootstrap = r'''import json,sys,subprocess
from pathlib import Path
import merge_batch_driver as driver,merge_batch,heavy,heavy_runs,heavy_sampling
from fixture_environment import hardware_sample
root=Path(sys.argv[1]); phase=sys.argv[2]; marker=root/'children.json'
repository=root/'repository'; (repository/'.git').mkdir(parents=True)
(root/'nvme').mkdir(); (root/'worktrees').mkdir()
heavy.HEAVY_LOCK=root/'heavy'; heavy.QUEUE_PATH=root/'queue'
heavy_sampling.host=hardware_sample
heavy.nvme_target=heavy_runs.nvme_target=lambda _:root/'nvme'
owned_run=heavy.run
heavy.run=lambda *a,**k: None
driver.landing.git=lambda *a: ('b'*40).encode()+bytes([10])
driver.landing.source=lambda *a: None
driver.landing.paths=lambda *a: {'owned.rs'}
driver.gate_policy.required_mode=lambda *a,**k:'affected'
def compose(repository,checkout,*a):
 checkout.mkdir(); return 'a'*40
merge_batch.compose=compose
child="import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(60)"
payload=("import os,sys,signal,time,json,subprocess;from pathlib import Path;"
 "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
 "child=subprocess.Popen([sys.executable,'-c',"+repr(child)+"]);"
 "Path("+repr(str(marker))+").write_text(json.dumps([os.getpid(),child.pid]));time.sleep(60)")
driver.landing.gate_invocation=lambda *a: ([sys.executable,'-c',payload],30)
driver.landing.qualification_proof=lambda *a: None
if phase=='publication':
 driver.gate.execute=lambda *a,**k: None
 def remote(*a,**k):
  payload="import os,time,json;from pathlib import Path;Path("+repr(str(marker))+").write_text(json.dumps([os.getpid()]));time.sleep(60)"
  return owned_run([sys.executable,'-c',payload],capture_output=True,text=True,timeout=30).stdout
 driver.remote=remote
queue=root/'accepted.json'; queue.write_text(json.dumps([dict(root=str(repository),branch='slice',
 commit='a'*40,base='b'*40,owner='agt-author',reviewer='agt-reviewer',acceptance='msg-accepted',tier='2')]))
sys.argv=['merge-batch',str(queue),'--repository',str(repository),'--owner','agt-author',
 '--state',str(root/'state'),'--worktrees',str(root/'worktrees'),'--controller','C:/fixtures/land.ps1']
raise SystemExit(driver.main())
'''
        for phase in ('qualification','publication'):
            with self.subTest(phase=phase), tempfile.TemporaryDirectory(prefix='secode-batch-cancel-') as directory:
                root=Path(directory)
                env=dict(isolated_environment(),PYTHONPATH=str(Path(merge_batch.__file__).parent))
                process=subprocess.Popen([sys.executable,'-B','-c',bootstrap,str(root),phase],env=env,
                                         stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
                descriptors=[]
                try:
                    marker=root/'children.json'; deadline=time.monotonic()+8
                    while not marker.exists() and process.poll() is None and time.monotonic()<deadline:
                        time.sleep(.01)
                    self.assertTrue(marker.exists(),'Batch did not reach the requested lifecycle boundary')
                    children=json.loads(marker.read_text())
                    for pid in children:
                        descriptors.append(os.pidfd_open(pid))
                    process.terminate()
                    out,err=process.communicate(timeout=15)
                    self.assertEqual(process.returncode,130,out+err)
                    self.assertTrue(all(identity(pid) is None for pid in children),'An owned local process survived cancellation')
                    pending=root/'state/pending-publication.json'
                    self.assertEqual(pending.exists(),phase=='publication')
                    rows=[json.loads(line) for line in (root/'state/results.jsonl').read_text().splitlines()]
                    self.assertEqual([row['status'] for row in rows],['cancelled'])
                finally:
                    if process.poll() is None:
                        process.kill(); process.communicate(timeout=5)
                    for descriptor in descriptors:
                        try:signal.pidfd_send_signal(descriptor,signal.SIGKILL)
                        except ProcessLookupError:pass
                        finally:os.close(descriptor)

    def test_combined_source_preserves_accepted_ancestry_and_rejects_overlap(self):
        with tempfile.TemporaryDirectory(prefix='secode-batch-test-') as directory:
            outer = Path(directory)
            repo = outer/'controller'; repo.mkdir()
            command(repo, 'git', 'init', '-q', '-b', 'main')
            command(repo, 'git', 'config', 'user.name', 'Fixture Author')
            command(repo, 'git', 'config', 'user.email', 'fixture@users.noreply.github.com')
            (repo/'base.txt').write_text('base\n')
            base = commit(repo, ['base.txt'])
            rows = []
            for index in range(3):
                root = outer/f'owner-{index}'
                command(outer, 'git', 'clone', '-q', str(repo), str(root))
                command(root, 'git', 'checkout', '-q', '-b', f'slice-{index}')
                name = f'{index}.txt'
                (root/name).write_text(f'{index}\n')
                accepted = commit(root, [name])
                rows.append(merge_batch.Candidate(root, f'slice-{index}', accepted, base,
                            f'agt-owner-{index}', 'agt-reviewer', f'msg-review-{index}', '2'))
            states = [(row.commit, command(row.root, 'git', 'status', '--porcelain')) for row in rows]
            combined = merge_batch.compose(repo, outer/'combined', 'batch', base, rows)
            for row in rows:
                command(repo, 'git', 'merge-base', '--is-ancestor', row.commit, combined)
            self.assertEqual(states, [(command(row.root, 'git', 'rev-parse', 'HEAD'),
                                      command(row.root, 'git', 'status', '--porcelain')) for row in rows])
            # An advance touching one source cannot be smuggled through a clean merge.
            (repo/'0.txt').write_text('overlap\n')
            advanced = commit(repo, ['0.txt'])
            with self.assertRaisesRegex(ValueError, 'overlap'):
                merge_batch.compose(repo, outer/'refused', 'refused', advanced, rows)
            self.assertFalse((outer/'refused').exists())

    def test_named_red_splits_but_infrastructure_and_uncertain_publication_stop(self):
        rows = [merge_batch.Candidate(Path(f'/owner-{i}'), f'slice-{i}', str(i)*40, 'f'*40,
                f'agt-owner-{i}', 'agt-reviewer', f'msg-review-{i}', '2') for i in range(4)]
        seen = []; events = []
        def attempt(group):
            seen.append([row.commit for row in group])
            return merge_batch.Outcome('red' if rows[1] in group else 'landed', 'named test failure')
        result = merge_batch.drain(rows, attempt, events.append)
        self.assertEqual([len(group) for group in seen], [4, 2, 1, 1, 2])
        self.assertEqual([event['commits'] for event in result if event['status'] == 'landed'],
                         [[rows[0].commit], [rows[2].commit, rows[3].commit]])
        for reason in ('disk admission', 'remote publication uncertain'):
            events = []
            result = merge_batch.drain(rows, lambda _: merge_batch.Outcome('blocked', reason), events.append)
            self.assertEqual(result, [dict(commits=[row.commit for row in rows], status='blocked', detail=reason)])

    def test_queue_requires_independent_accept_and_failure_identity_before_bisect(self):
        with tempfile.TemporaryDirectory() as directory:
            outer = Path(directory)
            row = dict(root=str(outer), branch='slice', commit='a'*40, base='b'*40,
                       owner='agt-author', reviewer='agt-reviewer', acceptance='msg-accepted', tier='2')
            queue = outer/'queue.json'; queue.write_text(json.dumps([row]))
            self.assertEqual(merge_batch_driver.read_queue(queue)[0].commit, row['commit'])
            row['reviewer'] = row['owner']; queue.write_text(json.dumps([row]))
            with self.assertRaisesRegex(ValueError, 'independent reviewer'):
                merge_batch_driver.read_queue(queue)
            log = outer/'red.log'
            for text, expected in [('FAIL full: disk capacity unavailable\n', False),
                                   ('test module::case ... FAILED\n', True),
                                   ('error[E0308]: mismatched types\n', True),
                                   ('error: denied lint\n --> crates/example.rs:2:1\n', True)]:
                log.write_text(text)
                self.assertEqual(bool(merge_batch_driver.named_failure(log)), expected)
            log.unlink(); log.symlink_to(queue)
            with self.assertRaises(ValueError):
                merge_batch_driver.named_failure(log)
