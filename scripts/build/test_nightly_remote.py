"""Exercise the actual publication transport with a disposable SSH executable."""
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

import nightly_revert
from heavy_queue import identity


SSH = r"""
import base64,json,os,signal,subprocess,sys,time
from pathlib import Path
root=Path.cwd()
script=base64.b64decode(sys.argv[-1]).decode('utf-16le')
(root/'invocation.json').write_text(json.dumps(dict(argv=sys.argv[1:],script=script)))
mode=os.environ['FIXTURE_MODE']
if mode=='return':
 print('fixture output');print('fixture diagnostic',file=sys.stderr);raise SystemExit(0)
if mode=='refuse':
 print('fixture refusal',file=sys.stderr);raise SystemExit(7)
if mode=='unconfirmed':
 print('publication transport ended without a receipt');raise SystemExit(0)
signal.signal(signal.SIGTERM,signal.SIG_IGN)
child=subprocess.Popen([sys.executable,'-c',
 "import signal,time;from pathlib import Path;"
 "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
 "Path('descendant-ready').touch();time.sleep(30)"],
 stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
while not (root/'descendant-ready').exists():time.sleep(.01)
(root/'children.json').write_text(json.dumps([os.getpid(),child.pid]))
time.sleep(30)
"""

# Only qualification prerequisites are substituted. Publication calls the real
# nightly_revert.remote through the actual batch entrypoint and signal handlers.
CALLER = r"""
import json,sys,time,subprocess
from contextlib import nullcontext
from pathlib import Path
import nightly_revert
root=Path.cwd();mode=sys.argv[1]
if mode=='direct':
 try:
  print(nightly_revert.remote("Write-Output 'synthetic request'",timeout=float(sys.argv[2])),end='')
 except (ValueError,RuntimeError,subprocess.TimeoutExpired) as error:
  print(type(error).__name__+': '+str(error),file=sys.stderr);raise SystemExit(1)
else:
 import heavy,merge_batch,merge_batch_driver as driver
 repository=root/'repository';(repository/'.git').mkdir(parents=True)
 (root/'worktrees').mkdir()
 owned_run=heavy.run
 def run(command,**options):
  if command[0]=='git':return None
  return owned_run(command,**options)
 heavy.run=run
 lease=(root/'lease').open('w')
 heavy.slot=lambda *a,**k:nullcontext(lease)
 driver.landing.git=lambda *a:('b'*40).encode()+b'\n'
 driver.landing.source=lambda *a:None
 driver.landing.paths=lambda *a:{'owned.rs'}
 driver.gate_policy.required_mode=lambda *a,**k:'affected'
 def compose(repository,checkout,*a):
  checkout.mkdir();return 'a'*40
 merge_batch.compose=compose
 driver.gate.execute=lambda *a,**k:None
 driver.landing.gate_invocation=lambda *a:(['fixture-qualification'],30)
 driver.landing.qualification_proof=lambda *a:None
 if mode=='timeout':
  original_init=driver.Runner.__init__
  def initialize(self,*a):
   original_init(self,*a);self.deadline=time.monotonic()+1
  driver.Runner.__init__=initialize
 queue=root/'accepted.json'
 queue.write_text(json.dumps([dict(root=str(repository),branch='slice',commit='a'*40,
  base='b'*40,owner='agt-author',reviewer='agt-reviewer',acceptance='msg-accepted',tier='2')]))
 sys.argv=['merge-batch',str(queue),'--repository',str(repository),'--owner','agt-author',
  '--state',str(root/'state'),'--worktrees',str(root/'worktrees'),
  '--controller','C:/fixtures/land.ps1']
 raise SystemExit(driver.main())
"""


class RemoteTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='secode-remote-test-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        binary = self.root/'bin/ssh'
        binary.parent.mkdir()
        binary.write_text('#!'+sys.executable+'\n'+SSH)
        binary.chmod(0o700)
        git = shutil.which('git')
        self.assertIsNotNone(git, 'Fixture import requires the installed Git executable')
        (binary.parent/'git').symlink_to(git)
        self.descriptors = []
        self.addCleanup(self.stop_children)

    def stop_children(self):
        for descriptor in self.descriptors:
            try:
                signal.pidfd_send_signal(descriptor, signal.SIGKILL)
            except ProcessLookupError:
                pass
            finally:
                os.close(descriptor)

    def start(self, mode, caller='direct', timeout=5):
        # No inherited account configuration, durable job, controller or lease.
        environment = dict(PATH=str(self.root/'bin'), HOME=str(self.root),
                           PYTHONPATH=str(Path(nightly_revert.__file__).parent),
                           SECODE_WINDOWS_HOST='fixture.invalid', FIXTURE_MODE=mode)
        process = subprocess.Popen([sys.executable, '-B', '-c', CALLER, caller, str(timeout)],
                                   cwd=self.root, env=environment, text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(self.stop_caller, process)
        return process

    def stop_caller(self, process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)

    def children(self, process):
        marker = self.root/'children.json'
        deadline = time.monotonic()+5
        while not marker.exists() and process.poll() is None and time.monotonic()<deadline:
            time.sleep(.01)
        if process.poll() is not None:
            out, error = process.communicate(timeout=5)
            self.fail('Disposable SSH exited before readiness: '+out+error)
        self.assertTrue(marker.exists(), 'Disposable SSH did not reach its ready boundary')
        children = json.loads(marker.read_text())
        for pid in children:
            self.descriptors.append(os.pidfd_open(pid))
        return children

    def assert_joined(self, children):
        self.assertTrue(all(identity(pid) is None for pid in children),
                        'Local SSH group member survived the consumer return')
        self.assertEqual(len(select.select(self.descriptors, [], [], 0)[0]), len(children))

    def assert_pending(self, status):
        pending = self.root/'state/pending-publication.json'
        value = json.loads(pending.read_text())
        self.assertEqual((value['commit'], value['base']), ('a'*40, 'b'*40))
        self.assertTrue(Path(value['checkout']).is_dir())
        self.assertEqual(value['branch'], Path(value['checkout']).name)
        rows = [json.loads(line) for line in (self.root/'state/results.jsonl').read_text().splitlines()]
        self.assertEqual([row['status'] for row in rows], [status])
        self.assertFalse((self.root/'state/last-result').exists())
        return rows[0]['detail']

    def test_return_and_nonzero_refusal_use_the_real_ssh_contract(self):
        for mode in ('return', 'refuse'):
            with self.subTest(mode=mode):
                process = self.start(mode)
                out, error = process.communicate(timeout=5)
                invocation = json.loads((self.root/'invocation.json').read_text())
                self.assertEqual(invocation['argv'][:-1], ['-n', 'fixture.invalid', 'powershell.exe',
                                 '-NoProfile', '-NonInteractive', '-EncodedCommand'])
                self.assertIn("Write-Output 'synthetic request'", invocation['script'])
                self.assertEqual(process.returncode, 0 if mode=='return' else 1, error)
                if mode=='return':
                    self.assertEqual(out, 'fixture output\n')
                else:
                    self.assertIn('Nightly controller refused: fixture refusal', error)

    def test_timeout_joins_term_resistant_local_group(self):
        process = self.start('hang', timeout=1)
        children = self.children(process)
        _, error = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 1, error)
        self.assertIn('TimeoutExpired', error)
        self.assert_joined(children)

    def test_nonfinite_timeout_refuses_before_ssh_starts(self):
        for timeout in (0, -1, float('nan'), float('inf')):
            with self.subTest(timeout=timeout):
                process = self.start('return', timeout=timeout)
                _, error = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 1, error)
                self.assertIn('finite timeout', error)
                self.assertFalse((self.root/'invocation.json').exists())

    def cancel_batch(self, number):
        process = self.start('hang', caller='batch')
        children = self.children(process)
        process.send_signal(number)
        out, error = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 130, out+error)
        self.assert_joined(children)
        self.assertIn('KeyboardInterrupt', self.assert_pending('cancelled'))

    def test_sigterm_joins_local_group_and_retains_remote_uncertainty(self):
        self.cancel_batch(signal.SIGTERM)

    def test_sigint_joins_local_group_and_retains_remote_uncertainty(self):
        self.cancel_batch(signal.SIGINT)

    def test_batch_timeout_preserves_pending_publication_without_retry(self):
        process = self.start('hang', caller='timeout')
        children = self.children(process)
        out, error = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 1, out+error)
        self.assert_joined(children)
        self.assertIn('TimeoutExpired', self.assert_pending('blocked'))

    def test_batch_refusal_preserves_pending_publication(self):
        process = self.start('refuse', caller='batch')
        out, error = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 1, out+error)
        self.assertIn('Nightly controller refused', self.assert_pending('blocked'))

    def test_batch_missing_receipt_preserves_pending_publication(self):
        process = self.start('unconfirmed', caller='batch')
        out, error = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 1, out+error)
        self.assertIn('Publication is unconfirmed', self.assert_pending('blocked'))


if __name__ == '__main__':
    unittest.main()
