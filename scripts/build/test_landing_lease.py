"""Landing-helper SIGKILL must leave the real gate child holding its lease."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import landing
from fixture_environment import isolated_environment


class LandingLeaseTests(unittest.TestCase):
    def test_qualification_and_composition_children_keep_the_heavy_lease_after_helper_crash(self):
        worker = '''import sys
from pathlib import Path
import landing,heavy,full_capacity
import heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host=hardware_sample
root=Path(sys.argv[1])
heavy.HEAVY_LOCK=root/'heavy'
heavy.QUEUE_PATH=root/'queue'
full_capacity.LOCK=root/'full.lock'
# The real compose/run_checks/gate.execute chain runs a private gate script.
# Unrelated Git and receipt policy already has its own real-Git witnesses.
from contextlib import nullcontext
landing.detached_source=lambda root,commit: nullcontext(root)
landing.source=lambda *args: {'commit':'a'*40,'clean':True}
landing.original_gate=lambda *args: 'fixture'
landing.git=lambda root,*args: (('c'*40 if args==('rev-parse','FETCH_HEAD') else 'a'*40)+'\\n').encode()
landing.paths=lambda *args: set()
landing.assert_disjoint=lambda *args: None
landing.raw_diff=lambda *args: b''
landing.commands=lambda *args: [['owned-check']]
landing.heavy.run=lambda *args,**kwargs: None
request={'accepted':'a'*40,'old_base':'b'*40,'branch':'fixture','canonical':'fixture',
         'run_gate':sys.argv[2]=='qualification','full':True,'gate_log':str(root/'original.log'),
         'receipt':str(root/'receipt.json'),'owner':'crash-seat'}
landing.compose(root,request)
'''
        guardian = '''import ctypes,fcntl,os,signal,subprocess,sys,time
from pathlib import Path
from heavy_queue import Queue
libc=ctypes.CDLL(None,use_errno=True)
if libc.prctl(36,1,0,0,0) != 0: raise OSError(ctypes.get_errno(),'subreaper unavailable')
root=Path(sys.argv[1])
helper=subprocess.Popen([sys.executable,'-B','-c',sys.argv[3],str(root),sys.argv[2]])
child=None
try:
    deadline=time.monotonic()+5
    marker=root/'gate.pid'
    while not marker.exists():
        if helper.poll() is not None: raise AssertionError('helper failed before its gate child ran')
        if time.monotonic()>=deadline: raise AssertionError('gate child did not start')
        time.sleep(0.01)
    child=int(marker.read_text())
    helper.kill()
    helper.wait(timeout=5)
    queue=Queue(root/'queue',root/'heavy')
    assert queue.snapshot(time.monotonic()+5) == [], 'dead helper ticket remained'
    with queue.heavy.open('a') as probe:
        try: fcntl.flock(probe,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError: pass
        else: raise AssertionError('helper crash unlocked its still-running gate child')
    os.killpg(child,signal.SIGTERM)
    os.waitpid(child,0)
    child=None
    with queue.heavy.open('a') as probe: fcntl.flock(probe,fcntl.LOCK_EX|fcntl.LOCK_NB)
finally:
    if helper.poll() is None:
        helper.kill()
        helper.wait(timeout=5)
    if child is not None:
        os.killpg(child,signal.SIGKILL)
        os.waitpid(child,0)
'''
        for phase in ['qualification', 'composition']:
            with self.subTest(phase=phase), tempfile.TemporaryDirectory(prefix='secode-landing-lease-') as temporary:
                root = Path(temporary)
                (root/'scripts').mkdir()
                # exec preserves the direct child's PID and inherited lease;
                # the private subreaper joins it even after its parent is killed.
                (root/'scripts/gate.sh').write_text('echo "$$" > gate.pid\nexec sleep 30\n')
                environment = dict(isolated_environment(), PYTHONPATH=str(Path(landing.__file__).parent))
                result = subprocess.run([sys.executable, '-B', '-c', guardian, str(root), phase, worker],
                                        env=environment, capture_output=True, text=True, timeout=15)
                self.assertEqual(result.returncode, 0, result.stdout+result.stderr)


if __name__ == '__main__':
    unittest.main()
