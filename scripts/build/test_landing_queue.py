"""A canonical lease must not sit behind the ordinary heavy queue."""
import fcntl
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import tempfile
import time
import unittest

import heavy
from heavy_queue import Queue
from fixture_environment import isolated_environment


WORKER = '''import fcntl, signal, sys
from pathlib import Path
import heavy, landing, gate, heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host = hardware_sample
root = Path(sys.argv[1])
mode = sys.argv[2]
heavy.HEAVY_LOCK = root/'heavy'
heavy.QUEUE_PATH = root/'queue'
def interrupted(*args): raise KeyboardInterrupt
signal.signal(signal.SIGTERM, interrupted)
try:
    if mode.startswith('ordinary'):
        with heavy.slot('gate', wait=15, job_owner=mode):
            print('ordinary-entered', flush=True)
            sys.stdin.readline()
    else:
        print('waiting-for-canonical', flush=True)
        with (root/'canonical.lock').open('a') as canonical:
            fcntl.flock(canonical, fcntl.LOCK_EX)
            print('canonical-acquired', flush=True)
            landing.source = lambda *args: None
            landing.apply_policy = lambda root, request: request
            if mode == 'first':
                gate.execute = lambda *args, **kwargs: print('qualified', flush=True)
                landing.compose_locked = lambda *args, **kwargs: print('composed', flush=True)
                request = dict(accepted='a'*40, old_base='b'*40, branch='candidate',
                               run_gate=True, full=False, owner='first',
                               receipt=str(root/'first.json'), gate_log=str(root/'gate.log'))
                landing.compose(root, request)
            else:
                # A reusable qualification does not need another gate reservation.
                landing.original_gate = lambda *args: None
                landing.qualify(root, dict(accepted='c'*40, branch='ready'))
                print('ready-landing-qualified', flush=True)
except KeyboardInterrupt:
    sys.exit(130)
'''


class LandingQueueTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='secode-landing-queue-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def stop(self, process):
        if process.poll() is None:
            process.send_signal(signal.SIGTERM)
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            heavy.stop_group(process)
            process.communicate(timeout=5)

    def child(self, root, mode):
        environment = isolated_environment()
        environment['SECODE_CACHE_DIR'] = str(root/'cache')
        process = subprocess.Popen(
            [sys.executable, '-u', '-c', WORKER, str(root), mode],
            cwd=Path(heavy.__file__).parent, env=environment,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True)
        self.addCleanup(self.stop, process)
        return process

    def line(self, process):
        result = bytearray()
        with selectors.DefaultSelector() as ready:
            ready.register(process.stdout, selectors.EVENT_READ)
            while len(result) < 4096:
                self.assertTrue(ready.select(5), 'worker did not reach the expected boundary')
                value = os.read(process.stdout.fileno(), 1)
                if value in (b'', b'\n'):
                    return result.decode()
                result.extend(value)
        self.fail('worker output exceeded its bound')

    def queued(self, queue, owner):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            rows = queue.snapshot(deadline)
            if any(row['owner'] == owner for row in rows):
                return rows
            time.sleep(0.01)
        self.fail(f'{owner} never enqueued')

    def test_lease_holding_qualification_does_not_block_ready_landing_behind_ordinary_queue(self):
        root = self.root
        queue = Queue(root/'queue', root/'heavy')
        with queue.heavy.open('a') as holder:
            fcntl.flock(holder, fcntl.LOCK_EX)
            ordinary = []
            for index in range(2):
                ordinary.append(self.child(root, 'ordinary-'+str(index)))
                self.queued(queue, 'ordinary-'+str(index))
            first = self.child(root, 'first')
            self.assertEqual(self.line(first), 'waiting-for-canonical')
            self.assertEqual(self.line(first), 'canonical-acquired')
            rows = self.queued(queue, 'first')
            self.assertEqual(next(row['class'] for row in rows if row['owner'] == 'first'), 0)
            second = self.child(root, 'ready')
            self.assertEqual(self.line(second), 'waiting-for-canonical')
            with selectors.DefaultSelector() as ready:
                ready.register(second.stdout, selectors.EVENT_READ)
                self.assertEqual(ready.select(0), [])
            fcntl.flock(holder, fcntl.LOCK_UN)
            self.assertEqual(self.line(first), 'qualified')
            self.assertEqual(self.line(first), 'composed')
            self.assertEqual(first.wait(timeout=5), 0, first.stderr.read().decode())
            self.assertEqual(self.line(second), 'canonical-acquired')
            self.assertEqual(self.line(second), 'ready-landing-qualified')
            self.assertEqual(second.wait(timeout=5), 0, second.stderr.read().decode())
            # Neither ordinary gate has been released. The second landing
            # acquired the canonical lease without waiting for that queue.
            self.assertTrue(all(process.poll() is None for process in ordinary))
            for process in ordinary:
                self.assertEqual(self.line(process), 'ordinary-entered')
                process.stdin.write(b'\n')
                process.stdin.flush()
                self.assertEqual(process.wait(timeout=5), 0, process.stderr.read().decode())
            self.assertEqual(queue.snapshot(time.monotonic() + 5), [])


if __name__ == '__main__':
    unittest.main()
