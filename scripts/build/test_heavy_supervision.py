"""Running jobs survive metadata contention without bypassing lease checks."""
import fcntl
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import heavy
from fixture_environment import isolated_environment, hardware_sample
import heavy_sampling
from heavy_queue import Queue, identity


def load_guard(seconds, ceiling=120.0):
    """Scale a hang guard by observed load; the proven boundary still arrives."""
    load = os.getloadavg()[0] / max(1, os.cpu_count() or 1)
    return min(ceiling, seconds * max(1.0, min(12.0, load)))


RUNNER = r'''
import signal,sys
from pathlib import Path
import heavy,heavy_runs
import heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host=hardware_sample
root=Path(sys.argv[1]); target=root/'nvme'; target.mkdir()
heavy.HEAVY_LOCK=root/'heavy'; heavy.QUEUE_PATH=root/'queue'
heavy.nvme_target=heavy_runs.nvme_target=lambda _:target
def interrupted(*args):raise KeyboardInterrupt
signal.signal(signal.SIGTERM,interrupted)
sys.argv=['heavy','--nvme','--owner','supervision-fixture','--wait','15',
          '--max-hold',sys.argv[3],'--',sys.executable,'-c',sys.argv[2]]
try:raise SystemExit(heavy.main())
except heavy.ExclusiveHoldTimeout:raise SystemExit(124)
except TimeoutError as error:
 print(error,file=sys.stderr);raise SystemExit(75)
except KeyboardInterrupt:raise SystemExit(130)
'''


class SupervisionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='secode-supervision-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.queue = Queue(self.root/'queue', self.root/'heavy')
        hardware = patch.object(heavy_sampling, 'host', side_effect=hardware_sample)
        hardware.start()
        self.addCleanup(hardware.stop)

    def start(self, body, max_hold=1800):
        body = ('import heavy_sampling;from fixture_environment import hardware_sample;'
                'heavy_sampling.host=hardware_sample;' + body)
        body = ("import os;from pathlib import Path;Path(" + repr(str(self.root/'child.pid'))
                + ").write_text(str(os.getpid()));" + body)
        process = subprocess.Popen([sys.executable, '-B', '-c', RUNNER, str(self.root), body, str(max_hold)],
            cwd=Path(__file__).parent, env=isolated_environment(),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(self.stop, process)
        with selectors.DefaultSelector() as ready:
            ready.register(process.stdout, selectors.EVENT_READ)
            self.assertTrue(ready.select(5), 'owned child did not start')
            self.assertEqual(process.stdout.readline().strip(), 'started')
        return process

    def wait_for(self, predicate):
        deadline = time.monotonic()+3
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.02)
        self.fail('private supervision boundary was not reached')

    def assert_joined(self):
        self.assertIsNone(identity(int((self.root/'child.pid').read_text())), 'owned child survived')

    def test_stop_group_reaches_a_descendant_after_its_leader_was_reaped(self):
        # The #329 review probe. The wrapper exits as soon as it has spawned its
        # worker, so by the time cleanup runs the leader is collected while the
        # worker still holds the group. Stopping must decide from the recorded
        # group, not from the leader's lifetime.
        worker = "import time; time.sleep(60)"
        # The wrapper exits at once and the worker inherits none of its stdio, so
        # waiting for the leader cannot wait for the worker: the leader is
        # collected while the worker keeps the group alive.
        wrapper = (
            "import subprocess,sys; "
            "child=subprocess.Popen([sys.executable,'-c',sys.argv[1]],"
            "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); "
            "print(child.pid,flush=True); sys.exit(0)"
        )
        # The production path holds an UnreapedProcess, which records the group at
        # spawn; the witness must use the same handle or it exercises the
        # foreign-child branch instead of the one the defect is in.
        leader = heavy.UnreapedProcess(
            [sys.executable, '-c', wrapper, worker],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, start_new_session=True)
        self.addCleanup(self.stop, leader)
        with selectors.DefaultSelector() as ready:
            ready.register(leader.stdout, selectors.EVENT_READ)
            self.assertTrue(ready.select(5), 'wrapper did not report its worker')
            worker_pid = int(os.read(leader.stdout.fileno(), 128))
        self.addCleanup(self._kill_if_alive, worker_pid)
        self.assertEqual(leader.wait(timeout=5), 0, 'wrapper did not exit')
        self.assertEqual(os.getpgid(worker_pid), leader.pid, 'worker left the wrapper group')
        # Reap the leader the way group cleanup does, so ownership is gone while
        # the worker still lives. That is the state #329 reached on a loaded box.
        self.assertEqual(leader.reap(5), 0, 'leader was not reaped')
        assert leader.reaped, 'leader ownership was not cleared'
        heavy.stop_group(leader)
        self.assertIsNone(identity(worker_pid), 'the worker survived its reaped leader')

    def _kill_if_alive(self, pid):
        """Leave no stray worker behind even when the witness itself fails."""
        try:
            if identity(pid) is not None:
                os.kill(pid, signal.SIGKILL)
        except OSError:
            pass

    def stop(self, process):
        if process.poll() is None:
            process.terminate()
        try:
            process.communicate(timeout=8)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate(timeout=5)

    def test_full_gate_wait_has_no_heavy_reservation_and_cancellation_releases_capacity(self):
        import full_capacity
        source = Path(heavy.__file__).parent
        gate_script = self.root/'gate.py'
        gate_script.write_text('''FULL_CAPACITY_PROTOCOL = 1
import sys
from pathlib import Path
import full_capacity,heavy
full_capacity.LOCK=Path(__file__).with_name('full.lock')
assert full_capacity.inherited() is not None
assert heavy.inherited() is not None
print('full and heavy leases retained',flush=True)
''')
        bootstrap = '''import sys,signal
from pathlib import Path
import heavy,full_capacity,heavy_sampling,heavy_runs
from fixture_environment import hardware_sample
root=Path(sys.argv[1]); full_capacity.LOCK=root/'full.lock'
heavy.HEAVY_LOCK=root/'heavy'; heavy.QUEUE_PATH=root/'queue'
heavy_sampling.host=hardware_sample
heavy.nvme_target=heavy_runs.nvme_target=lambda _:root/'nvme'
def stop(*_):raise KeyboardInterrupt
signal.signal(signal.SIGTERM,stop)
sys.argv=['heavy','--nvme','--wait','10','--',sys.executable,str(root/'gate.py'),'full']
try:raise SystemExit(heavy.main())
except KeyboardInterrupt:raise SystemExit(130)
'''
        environment = isolated_environment()
        environment['PYTHONPATH'] = str(source)
        for cancel in (True, False):
            with self.subTest(cancel=cancel), (self.root/'full.lock').open('a') as holder:
                fcntl.flock(holder, fcntl.LOCK_EX)
                child = subprocess.Popen([sys.executable,'-B','-c',bootstrap,str(self.root)],
                    cwd=source, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                self.addCleanup(self.stop,child)
                with selectors.DefaultSelector() as ready:
                    ready.register(child.stdout,selectors.EVENT_READ)
                    self.assertTrue(ready.select(5), 'Full-capacity wait never started')
                    self.assertIn('before build admission',child.stdout.readline())
                # A gate blocked on workspace capacity has not enqueued, taken
                # the heavy flock, or reserved projected memory.
                self.assertFalse(self.queue.snapshot(time.monotonic()+2))
                with self.queue.heavy.open('a') as probe:
                    fcntl.flock(probe,fcntl.LOCK_EX | fcntl.LOCK_NB)
                if cancel:
                    child.terminate()
                    out,err=child.communicate(timeout=8)
                    self.assertEqual(child.returncode,130,out+err)
                else:
                    fcntl.flock(holder,fcntl.LOCK_UN)
                    out,err=child.communicate(timeout=8)
                    self.assertEqual(child.returncode,0,out+err)
                    self.assertIn('full and heavy leases retained',out)
                self.assertFalse(self.queue.snapshot(time.monotonic()+2))
            with (self.root/'full.lock').open('a') as probe:
                fcntl.flock(probe,fcntl.LOCK_EX | fcntl.LOCK_NB)
                # An environment claim without a corroborating kernel lock fails.
                info=os.fstat(probe.fileno())
                value=json.dumps(dict(fd=probe.fileno(),device=info.st_dev,inode=info.st_ino,mode='WRITE'))
                fcntl.flock(probe,fcntl.LOCK_UN)
                with patch.dict(os.environ,{full_capacity.CLAIM:value}), self.assertRaisesRegex(ValueError,'kernel lock'):
                    full_capacity.inherited(self.root/'full.lock')

    def test_running_child_survives_a_metadata_lock_held_beyond_five_seconds(self):
        marker = self.root/'finish'
        body = ("import time;from pathlib import Path;print('started',flush=True);"
                "marker=Path(" + repr(str(marker)) + ")\n"
                "while not marker.exists():time.sleep(.02)\n")
        process = self.start(body)
        with self.queue.locked(time.monotonic()+5):
            time.sleep(6.2)
        marker.touch()
        _, error = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 0, error)
        self.assertNotIn('metadata lock', error)
        self.assertEqual(self.queue.snapshot(time.monotonic()+5), [])
        with self.queue.heavy.open('a') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX|fcntl.LOCK_NB)

    def test_inherited_lease_validation_does_not_wait_for_admission(self):
        trigger, checked, finish = (self.root/name for name in ('trigger', 'checked', 'finish'))
        body = ("import time,heavy;print('started',flush=True);trigger=Path(" + repr(str(trigger)) + ")\n"
                "while not trigger.exists():time.sleep(.02)\n"
                "heavy.inherited();Path(" + repr(str(checked)) + ").touch()\n"
                "while not Path(" + repr(str(finish)) + ").exists():time.sleep(.02)\n")
        process = self.start(body)
        with self.queue.locked(time.monotonic()+5):
            trigger.touch()
            self.wait_for(checked.exists)
            self.assertIsNone(process.poll())
        finish.touch()
        _, error = process.communicate(timeout=8)
        self.assertEqual(process.returncode, 0, error)

    def test_legacy_budget_call_and_mutex_free_observation_agree(self):
        with heavy.slot('gate', 5, heavy_path=self.queue.heavy, queue_path=self.queue.directory,
                        job_owner='budget-compatibility', max_hold=10):
            claim, queue, ticket, _ = heavy.inherited()
            legacy = queue.exclusive_budget(ticket)
            with queue.locked(time.monotonic()+5):
                self.assertEqual(queue.exclusive_budget(ticket, claim['fd']), legacy)
            self.assertEqual(legacy[1], 10)

    def test_lost_live_run_still_cancels_and_joins_the_child(self):
        process = self.start("import time;print('started',flush=True);time.sleep(60)")
        with self.queue.locked(time.monotonic()+5):
            _, runs = self.queue.state()
            runs.active.clear()
            runs.save()
        _, error = process.communicate(timeout=8)
        self.assertNotEqual(process.returncode, 0)
        self.assertIn('Inherited heavy ticket is not live', error)
        self.assert_joined()

    def test_lost_kernel_lock_still_cancels_and_joins_the_child(self):
        trigger = self.root/'unlock'
        body = ("import time,heavy,json,fcntl;print('started',flush=True)\n"
                "while not Path(" + repr(str(trigger)) + ").exists():time.sleep(.02)\n"
                "fcntl.flock(json.loads(os.environ[heavy.CLAIM])['fd'],fcntl.LOCK_UN)\n"
                "time.sleep(600)")
        process = self.start(body)
        trigger.touch()
        # Wait on the wrapper alone, never on its pipes: the guard only bounds
        # how long a loaded host may take to deliver its own exit, because the
        # join is proven below, at that exit. Draining first would let a child
        # that merely outlived the wrapper and exited on its own satisfy the
        # witness. heavy.stop_group allows one SIGTERM grace and one SIGKILL
        # grace before it refuses, so the guard clears both phases.
        process.wait(timeout=load_guard(2*5.0))
        self.assertNotEqual(process.returncode, 0)
        # The wrapper has exited, so its cleanup has already run: the child must
        # be gone now, whatever the wait took.
        self.assert_joined()
        # Draining the pipes is cleanup now that the witness above has passed.
        _, error = process.communicate(timeout=load_guard(2*5.0))
        self.assertIn('Inherited heavy kernel lock is not held', error)

    def test_corrupt_run_metadata_still_cancels_and_joins_the_child(self):
        process = self.start("import time;print('started',flush=True);time.sleep(60)")
        path = self.queue.directory/'runs.json'
        original = path.read_bytes()
        try:
            with self.queue.locked(time.monotonic()+5):
                staging = path.with_suffix('.test-corrupt')
                staging.write_text(json.dumps({'version': 2, 'active': {}, 'recent': []}))
                staging.replace(path)
            _, error = process.communicate(timeout=8)
            self.assertNotEqual(process.returncode, 0)
            self.assertIn('Invalid heavy resource state', error)
            self.assert_joined()
        finally:
            path.write_bytes(original)

    def test_converted_child_expires_even_while_metadata_mutex_is_busy(self):
        body = ("import heavy,heavy_runs,signal,time;heavy.nvme_target=heavy_runs.nvme_target=lambda p:Path(p);"
                "heavy.require_exclusive();signal.signal(signal.SIGALRM,signal.SIG_IGN);"
                "print('started',flush=True);time.sleep(60)")
        process = self.start(body, max_hold=.3)
        with self.queue.locked(time.monotonic()+5):
            self.wait_for(lambda: identity(int((self.root/'child.pid').read_text())) is None)
        _, error = process.communicate(timeout=8)
        self.assertEqual(process.returncode, 124, error)
        self.assert_joined()


if __name__ == '__main__':
    unittest.main()
