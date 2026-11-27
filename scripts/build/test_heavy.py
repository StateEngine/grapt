"""Real private-lock and process witnesses for the shared heavy-job queue."""
import contextlib
import errno
import fcntl
import io
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import threading
import unittest
from unittest.mock import patch

import heavy
from fixture_environment import isolated_environment, hardware_sample
import heavy_sampling
from evict_targets import FREE_SPACE_FLOOR_BYTES
from heavy_queue import Queue, identity, order


def load_guard(seconds, ceiling=120.0):
    """Scale a hang guard by observed load; the proven boundary still arrives.

    This is only how long a loaded host may take to reach a boundary the test
    already proves will come. It never stands in for the condition itself.
    """
    load = os.getloadavg()[0] / max(1, os.cpu_count() or 1)
    return min(ceiling, seconds * max(1.0, min(12.0, load)))


class HeavyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='secode-heavy-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.queue = Queue(self.root/'queue', self.root/'heavy')
        hardware = patch.object(heavy_sampling, 'host', side_effect=hardware_sample)
        hardware.start()
        self.addCleanup(hardware.stop)

    def test_a_queued_job_makes_its_target_in_use_for_the_evictor(self):
        # A bare `cargo` job used to hold only the queue lease, so an evictor
        # that scans /proc could conclude the target was idle while a build was
        # running in it. The job now takes the evictor's own shared lease, so
        # the exclusive side refuses for as long as the job runs.
        import evict_targets
        target = self.root/'target'
        target.mkdir()
        with heavy.cache_lease(target):
            with self.assertRaises(BlockingIOError):
                evict_targets.lease(target)
        # Released with the job: maintenance may take it afterwards.
        evict_targets.lease(target).close()

    def test_a_job_refuses_to_enter_a_target_the_evictor_holds(self):
        # The shared side of this lease cannot fail for contention among
        # builds, so a refusal means an EXCLUSIVE holder - the evictor deleting
        # this target. Entering anyway would race the deletion the lease exists
        # to order, so the job must fail with the contention named rather than
        # run unleased.
        import evict_targets
        target = self.root/'evicting'
        target.mkdir()
        with evict_targets.lease(target):
            with self.assertRaises(RuntimeError) as refused:
                with heavy.cache_lease(target):
                    self.fail('build entered a target during eviction')
            self.assertIn('leased exclusively', str(refused.exception))
        # Once released, the same job is admitted and holds the real lease.
        with heavy.cache_lease(target):
            with self.assertRaises(BlockingIOError):
                evict_targets.lease(target)

    def test_a_job_with_no_target_still_runs(self):
        with heavy.cache_lease(None):
            pass

    def test_installed_launcher_uses_its_worker_with_the_selected_checkout_policy(self):
        import build_environment
        import durable_jobs
        import full_capacity
        checkout = self.root/'checkout'; (checkout/'scripts/build').mkdir(parents=True)
        controller = self.root/'controller'; controller.mkdir()
        marker = self.root/'worker'
        (checkout/'policy').write_text('candidate coverage')
        source_gate = checkout/'scripts/build/gate.py'
        source_gate.write_text("FULL_CAPACITY_PROTOCOL = 1\nfrom pathlib import Path\n"
                               f"Path({str(marker)!r}).write_text('old worker')\n")
        shell = checkout/'scripts/gate.sh'
        shell.write_text(f'exec {sys.executable} {source_gate} "$@"\n')
        current_gate = controller/'gate.py'
        current_gate.write_text("FULL_CAPACITY_PROTOCOL = 1\nimport argparse\nfrom pathlib import Path\n"
            "p=argparse.ArgumentParser();p.add_argument('mode');p.add_argument('--source-policy',type=Path,required=True)\n"
            "a=p.parse_args()\n"+f"Path({str(marker)!r}).write_text(a.mode+':'+(a.source_policy/'policy').read_text())\n")
        explicit = self.root/'gate.py'
        explicit.write_text("from pathlib import Path\n"+f"Path({str(marker)!r}).write_text('explicit worker')\n")
        previous = Path.cwd();os.chdir(checkout);self.addCleanup(os.chdir,previous)
        # This fixture judges which WORKER a launcher selects, not the machine it
        # runs on: the admission loop censuses /proc/locks and /proc every
        # iteration, so on a loaded host it can refuse for the whole wait. Pin
        # both reads here only, so the census witnesses keep the real ones.
        def no_shared_locks(self, handle, held=()):
            return 0
        for mode, selected, default, expected in [
                ('affected',source_gate,True,'affected:candidate coverage'),
                ('full',source_gate,True,'full:candidate coverage'),
                ('affected',shell,True,'affected:candidate coverage'),
                ('affected',explicit,True,'explicit worker'),
                ('affected',source_gate,False,'old worker')]:
            with self.subTest(mode=mode,default=default,selected=selected), \
                    patch.dict(os.environ,isolated_environment(),clear=True), \
                    patch.object(heavy,'__file__',str(controller/'heavy.py')), \
                    patch.object(heavy,'HEAVY_LOCK',self.queue.heavy), \
                    patch.object(heavy,'QUEUE_PATH',self.queue.directory), \
                    patch.object(heavy,'DEFAULT_QUEUE_PATH',self.queue.directory if default else self.root/'other'), \
                    patch.object(full_capacity,'LOCK',self.root/'full.lock'), \
                    patch.object(durable_jobs,'active',return_value=True), \
                    patch.object(durable_jobs,'fenced',side_effect=lambda command:command), \
                    patch.object(build_environment,'for_checkout',return_value=None), \
                    patch.object(heavy_sampling,'processes',return_value={}), \
                    patch.object(Queue,'shared_locks',no_shared_locks), \
                    patch.object(sys,'argv',['heavy','--exclusive','--owner','coherent-fixture','--wait','3',
                                             '--','bash' if selected.suffix=='.sh' else sys.executable,str(selected),mode]):
                self.assertEqual(heavy.main(),0)
                self.assertEqual(marker.read_text(),expected)
                self.assert_empty()

    def test_light_offline_job_runs_while_the_build_lock_is_exclusive(self):
        marker = self.root/'measured'
        command = [sys.executable, '-c', 'import pathlib,sys;pathlib.Path(sys.argv[1]).write_text("done")', str(marker)]
        with self.queue.heavy.open('a') as held, \
                patch.object(heavy, 'LIGHT_LOCK', self.root/'light'), \
                patch.object(heavy, 'QUEUE_PATH', self.queue.directory):
            fcntl.flock(held, fcntl.LOCK_EX)
            self.assertEqual(heavy.run_light(command), 0)
            self.assertEqual(marker.read_text(), 'done')
            self.assertEqual(self.queue.snapshot(time.monotonic()+5), [])
            with (self.root/'light').open('a') as light:
                fcntl.flock(light, fcntl.LOCK_EX)
                with self.assertRaisesRegex(TimeoutError, 'Another light'):
                    heavy.run_light(command)
            with self.assertRaisesRegex(ValueError, 'non-Cargo'):
                heavy.run_light(['cargo', 'test'])
            worker = '''import sys
from pathlib import Path
import heavy,heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host=hardware_sample
heavy.LIGHT_LOCK=Path(sys.argv[1])/'light'
heavy.QUEUE_PATH=Path(sys.argv[1])/'queue'
heavy.LIGHT_WALL_SECONDS=2
sys.exit(heavy.run_light(sys.argv[2:]))
'''
            diagnostic = '''import os,signal,subprocess,sys,time
signal.signal(signal.SIGTERM,signal.SIG_IGN)
subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])
print(os.getpgrp(),flush=True)
time.sleep(30)
'''
            launcher = subprocess.Popen([sys.executable, '-B', '-c', worker, str(self.root),
                                         sys.executable, '-c', diagnostic],
                                        cwd=Path(heavy.__file__).parent, env=isolated_environment(),
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            group = None
            try:
                group = int(self.line(launcher))
                launcher.kill()
                launcher.wait(timeout=5)
                self.assertTrue(heavy.live_group(group))
                with self.assertRaisesRegex(TimeoutError, 'Another light'):
                    heavy.run_light(command)
                deadline = time.monotonic()+8
                while heavy.live_group(group) and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertFalse(heavy.live_group(group), 'Orphan diagnostic exceeded its wall bound')
                with (self.root/'light').open('a') as released:
                    while True:
                        try:
                            fcntl.flock(released, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            break
                        except BlockingIOError:
                            self.assertLess(time.monotonic(), deadline, 'Supervisor retained the light lock')
                            time.sleep(0.02)
                self.assertEqual(heavy.run_light(command), 0)
                self.assertEqual(self.queue.snapshot(time.monotonic()+5), [])
            finally:
                if group is not None and heavy.live_group(group):
                    os.killpg(group, signal.SIGKILL)
                self.cleanup(launcher)

    def test_slow_descriptor_census_does_not_block_running_job_metadata(self):
        started, release = threading.Event(), threading.Event()
        failures = []
        ticket, notification = self.queue.enqueue(1, 'census-probe', time.monotonic() + 5)
        def census(*args):
            started.set()
            if not release.wait(3):
                raise TimeoutError('Private census fixture was not released')
            return 0
        def admission():
            try:
                with self.queue.heavy.open('a') as handle:
                    self.queue.acquire(ticket, notification, handle, time.monotonic() + 5, 5)
            except BaseException as error:
                failures.append(error)
        with patch.object(self.queue, 'shared_locks', side_effect=census):
            thread = threading.Thread(target=admission)
            thread.start()
            try:
                self.assertTrue(started.wait(2), 'Census never started')
                with self.queue.locked(time.monotonic() + 0.5):
                    self.queue.state()
            finally:
                release.set()
                thread.join(timeout=5)
                notification.close()
        self.assertFalse(thread.is_alive(), 'Private admission did not finish')
        self.assertEqual(failures, [])
        self.queue.remove(ticket)

    def test_group_census_distinguishes_live_members_from_zombies_and_other_groups(self):
        proc = self.root / 'proc'
        proc.mkdir()
        def member(pid, state, group):
            directory = proc / str(pid)
            directory.mkdir(exist_ok=True)
            (directory / 'stat').write_text(f'{pid} (name with ) spaces) {state} 1 {group} 1')
        member(10, 'Z', 42)
        member(11, 'S', 99)
        (proc / '11/stat').write_bytes(b'11 (foreign-\xff) S 1 99 1')
        self.assertFalse(heavy.live_group(42, proc))
        member(12, 'S', 42)
        self.assertTrue(heavy.live_group(42, proc))
        member(12, 'X', 42)
        self.assertFalse(heavy.live_group(42, proc))
        (proc / '13').mkdir()
        self.assertFalse(heavy.live_group(42, proc))
        (proc / '13/stat').write_text('x' * 8193)
        with self.assertRaisesRegex(ValueError, 'exceeds its bound'):
            heavy.live_group(42, proc)

    def raw_lock(self):
        handle = self.queue.heavy.open('a')
        self.addCleanup(handle.close)
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return handle

    def test_inherited_descriptor_census_deduplicates_one_lease_across_children(self):
        proc = self.root / 'proc'
        proc.mkdir()
        key = '00:00:123'
        for pid, ticket in [(100, '1'), (101, '1'), (102, '2')]:
            process = proc / str(pid)
            process.mkdir()
            claim = {'queue': str(self.queue.directory.resolve()),
                     'heavy': str(self.queue.heavy.resolve()), 'ticket': ticket, 'fd': 7}
            (process / 'environ').write_bytes(b'SECODE_HEAVY_LEASE=' + json.dumps(claim).encode() + b'\0')
            (process / 'fdinfo').mkdir()
            (process / 'fdinfo/7').write_text(f'lock: 1: FLOCK ADVISORY READ 999 {key} 0 EOF\n')
        self.assertEqual(self.queue.inherited_shared_leases(key, proc), {'1', '2'})
        (proc / '102/fdinfo/7').unlink()
        self.assertEqual(self.queue.inherited_shared_leases(key, proc), {'1'})
        (proc / '101/fdinfo/7').write_text('pos: 0\n')
        self.assertEqual(self.queue.inherited_shared_leases(key, proc), {'1'})

    def test_inherited_descriptors_corroborate_a_paginated_global_undercount(self):
        with self.queue.heavy.open('a') as handle:
            with patch.object(Path, 'open', return_value=io.StringIO('')), \
                    patch.object(self.queue, 'inherited_shared_leases', return_value={'4'}):
                self.assertEqual(self.queue.shared_locks(handle, {'1', '2', '3'}), 4)

    def test_nested_shared_lock_counts_once_for_its_verified_governed_lease(self):
        script = '''import fcntl,json,os,subprocess,sys
from pathlib import Path
from heavy_queue import identity
root=Path(sys.argv[1])
with (root/'heavy').open('a') as held:
    fcntl.flock(held,fcntl.LOCK_SH)
    claim=dict(queue=str((root/'queue').resolve()),heavy=str((root/'heavy').resolve()),
               ticket='1',pid=os.getpid(),start=identity(os.getpid()),fd=held.fileno())
    environment=dict(os.environ,SECODE_HEAVY_LEASE=json.dumps(claim))
    child="import fcntl,sys; held=open(sys.argv[1],'a'); fcntl.flock(held,fcntl.LOCK_SH); print('entered',flush=True); sys.stdin.readline()"
    result=subprocess.run([sys.executable,'-u','-c',child,str(root/'heavy')],
                          env=environment,pass_fds=(held.fileno(),))
    sys.exit(result.returncode)
'''
        process = subprocess.Popen([sys.executable, '-u', '-c', script, str(self.root)],
                                   cwd=Path(heavy.__file__).parent, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                   start_new_session=True, env=isolated_environment())
        self.addCleanup(self.cleanup, process)
        self.assertEqual(self.line(process), 'entered')
        with self.queue.heavy.open('a') as handle:
            count = self.queue.shared_locks(handle)
            fcntl.flock(handle, fcntl.LOCK_SH)
            independent = self.queue.shared_locks(handle)
            original_open = Path.open
            for error in (FileNotFoundError('exited child'), PermissionError('unavailable child')):
                def unavailable(path, *args, **kwargs):
                    if path.name == 'stat' and str(path).startswith('/proc/'):
                        raise error
                    return original_open(path, *args, **kwargs)
                with self.subTest(error=type(error).__name__), patch.object(Path, 'open', unavailable):
                    observed = self.queue.shared_locks(handle)
                    self.assertEqual(
                        observed, 3,
                        'expected 3 with /proc/*/stat raising {}; counter saw {}'.format(
                            type(error).__name__,
                            self.queue.inventory_for(observed)))
        self.finish(process)
        self.assertEqual(count, 1, f'initial count; counter saw '
                                   f'{self.queue.inventory_for(count)}')
        self.assertEqual(independent, 2, f'count with an added raw flock; counter saw '
                                         f'{self.queue.inventory_for(independent)}')

    def test_registered_holder_with_a_replaced_child_descriptor_is_reserved_once(self):
        script = """import fcntl,json,os,subprocess,sys,time
from pathlib import Path
from heavy_queue import Queue,identity
root=Path(sys.argv[1]); queue=Queue(root/'queue',root/'heavy')
ticket,notice=queue.enqueue(1,'sandbox-holder',time.monotonic()+5)
notice.close()
with queue.locked(time.monotonic()+5):
    rows=queue.load();rows[0]['holder']=True;queue.save(rows)
with (root/'heavy').open('a') as held:
    fcntl.flock(held,fcntl.LOCK_SH)
    claim=dict(queue=str(queue.directory.resolve()),heavy=str(queue.heavy.resolve()),
               ticket=ticket['id'],pid=os.getpid(),start=identity(os.getpid()),fd=held.fileno())
    grandchild="import fcntl,sys;held=open(sys.argv[1],'a');fcntl.flock(held,fcntl.LOCK_SH);print('entered',flush=True);sys.stdin.readline()"
    child="import os,subprocess,sys;os.close(int(sys.argv[1]));sys.exit(subprocess.run([sys.executable,'-u','-c',sys.argv[2],sys.argv[3]]).returncode)"
    result=subprocess.run([sys.executable,'-u','-c',child,str(held.fileno()),grandchild,str(root/'heavy')],
                          env=dict(os.environ,SECODE_HEAVY_LEASE=json.dumps(claim)),pass_fds=(held.fileno(),))
queue.remove(ticket)
sys.exit(result.returncode)
"""
        process = subprocess.Popen([sys.executable, '-u', '-c', script, str(self.root)],
                                   cwd=Path(heavy.__file__).parent, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                   start_new_session=True, env=isolated_environment())
        self.addCleanup(self.cleanup, process)
        self.assertEqual(self.line(process), 'entered')
        ticket, notice = self.queue.enqueue(1, 'next-shared-job', time.monotonic()+5)
        try:
            with patch('heavy_runs.Placements.shared', return_value=True), \
                    patch.object(heavy_sampling, 'host', return_value=dict(hardware_sample(), available=40*1024**3)), \
                    self.queue.heavy.open('a') as handle:
                self.assertEqual(self.queue.acquire(ticket, notice, handle, time.monotonic()+0.5, 0.5), 'disk')
        finally:
            notice.close()
            self.queue.remove(ticket)
            self.finish(process)

    def cleanup(self, process):
        if process.poll() is None:
            process.terminate()
        try:
            process.communicate(timeout=8)
        except subprocess.TimeoutExpired:
            from gate import stop_group
            stop_group(process)
            process.communicate(timeout=5)

    def child(self, owner, priority=1, wait=10, command=None):
        script = '''import signal,sys
from pathlib import Path
import heavy
import heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host=hardware_sample
heavy.HEAVY_LOCK=Path(sys.argv[1])/'heavy'
heavy.QUEUE_PATH=Path(sys.argv[1])/'queue'
def interrupted(*args): raise KeyboardInterrupt
signal.signal(signal.SIGTERM,interrupted)
try:
    if sys.argv[5] == 'slot':
        with heavy.slot('landing' if sys.argv[3]=='0' else 'gate',float(sys.argv[4]),job_owner=sys.argv[2]):
            print('entered',flush=True)
            sys.stdin.readline()
    else:
        sys.argv=['heavy','--exclusive','--owner',sys.argv[2],'--class',sys.argv[3],'--wait',sys.argv[4],'--',*sys.argv[6:]]
        sys.exit(heavy.main())
except TimeoutError as error:
    print(error,file=sys.stderr)
    sys.exit(75)
except KeyboardInterrupt:
    sys.exit(130)
'''
        process = subprocess.Popen([sys.executable, '-u', '-c', script, str(self.root),
                                    owner, str(priority), str(wait), 'command' if command else 'slot',
                                    *(command or [])], cwd=Path(heavy.__file__).parent,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True, env=isolated_environment())
        self.addCleanup(self.cleanup, process)
        return process

    def line(self, process):
        line = bytearray()
        with selectors.DefaultSelector() as ready:
            ready.register(process.stdout, selectors.EVENT_READ)
            while len(line) < 4096:
                self.assertTrue(ready.select(5), 'heavy worker did not reach the expected boundary')
                value = os.read(process.stdout.fileno(), 1)
                if value in (b'', b'\n'):
                    return line.decode()
                line.extend(value)
        self.fail('Heavy worker exceeded the output bound')

    def enqueued(self, owner):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            rows = self.queue.snapshot(deadline)
            if any(row['owner'] == owner for row in rows):
                return rows
            time.sleep(0.01)
        self.fail(f'{owner} never enqueued')

    def finish(self, process):
        process.stdin.write('\n')
        process.stdin.flush()
        self.assertEqual(process.wait(timeout=5), 0, process.stderr.read())

    def assert_empty(self):
        self.assertEqual(self.queue.snapshot(time.monotonic() + 5), [])

    def test_hygiene_always_enqueues_at_maintenance_priority(self):
        enqueue = Queue.enqueue
        for free, root_free, priority in [(FREE_SPACE_FLOOR_BYTES - 1, FREE_SPACE_FLOOR_BYTES, 0),
                                          (FREE_SPACE_FLOOR_BYTES, FREE_SPACE_FLOOR_BYTES - 1, 0),
                                          (FREE_SPACE_FLOOR_BYTES, FREE_SPACE_FLOOR_BYTES, 0),
                                          (FREE_SPACE_FLOOR_BYTES + 1, FREE_SPACE_FLOOR_BYTES + 1, 0)]:
            with self.subTest(free=free), \
                    patch.object(heavy, 'HEAVY_LOCK', self.queue.heavy), \
                    patch.object(heavy, 'QUEUE_PATH', self.queue.directory), \
                    patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.root)}), \
                    patch.object(heavy_sampling, 'host', return_value=dict(hardware_sample(),
                        root_free=root_free, nvme_free=free)), \
                    patch.object(Queue, 'enqueue', autospec=True, side_effect=enqueue) as observed, \
                    patch.object(sys, 'argv', ['heavy', '--exclusive', '--hygiene', '--owner', 'disk-hygiene',
                                              '--wait', str(load_guard(30.0)), '--', sys.executable, '-c', 'pass']):
                self.assertEqual(heavy.main(), 0)
                self.assertEqual(observed.call_args.args[1:3], (priority, 'disk-hygiene'))
                self.assert_empty()

    def test_hygiene_priority_does_not_depend_on_a_free_space_probe(self):
        enqueue = Queue.enqueue
        with patch.object(heavy, 'HEAVY_LOCK', self.queue.heavy), \
                patch.object(heavy, 'QUEUE_PATH', self.queue.directory), \
                patch.object(heavy_sampling, 'host', side_effect=OSError('unreadable cache')), \
                patch.object(Queue, 'enqueue', autospec=True, side_effect=enqueue) as observed, \
                patch.object(sys, 'argv', ['heavy', '--exclusive', '--hygiene', '--owner', 'disk-hygiene',
                                          '--', sys.executable, '-c', 'pass']):
            with self.assertRaisesRegex(OSError, 'unreadable cache'):
                heavy.main()
            self.assertEqual(observed.call_args.args[1:3], (0, 'disk-hygiene'))
            self.assert_empty()

    def test_hygiene_flag_requires_its_named_owner(self):
        with patch.object(shutil, 'disk_usage') as probe, \
                patch.object(Queue, 'enqueue') as enqueue, \
                contextlib.redirect_stderr(io.StringIO()), \
                patch.object(sys, 'argv', ['heavy', '--exclusive', '--hygiene', '--owner', 'other-seat',
                                          '--', sys.executable, '-c', 'pass']):
            with self.assertRaises(SystemExit) as refused:
                heavy.main()
            self.assertEqual(refused.exception.code, 2)
            probe.assert_not_called()
            enqueue.assert_not_called()

    def test_env_job_without_a_program_is_refused_before_any_execution(self):
        import durable_jobs
        for command in (
                ['env'],
                ['env', '-i'],
                ['env', '-0'],
                ['env', 'FOO=1'],
                ['env', '-i', 'FOO=1', 'BAR=2'],
                ['env', '-u', 'FOO'],
                ['env', '--unset', 'FOO'],
                ['env', '-u'],
                ['env', '-C', str(self.root)],
                ['env', '-S', 'FOO=1'],
                ['env', '-S', '   '],
                ['env', '-S', ''],
                ['env', '-S', "'abc"],
                ['env', '-S', r'abc\ def'],
                ['env', '-iS/usr/bin/touch m'],
                ['env', '-h'],
                ['env', '-V'],
                ['env', '--help'],
                ['env', '--version'],
                ['env', '-i', '--help', 'true'],
                ['env', '-ih'],
                ['env', '-u=FOO', 'true'],
                ['env', '-S=/usr/bin/true'],
                ['env', '--unknown', 'true'],
                ['env', '-'],
                ['env', '--'],
                ['env', '--', 'FOO=1'],
                ['/usr/bin/env', 'FOO=1'],
                ['/usr/bin/env', '-i']):
            with self.subTest(command=command), \
                    patch.object(Queue, 'enqueue') as enqueue, \
                    patch.object(heavy, 'run_light') as light, \
                    patch.object(durable_jobs, 'run') as durable, \
                    contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(sys, 'argv', ['heavy', '--light', '--', *command]):
                with self.assertRaises(SystemExit) as refused:
                    heavy.main()
                self.assertEqual(refused.exception.code, 2)
            enqueue.assert_not_called()
            light.assert_not_called()
            durable.assert_not_called()

    def test_env_job_with_a_program_passes_the_guard(self):
        for command in (
                ['env', 'true'],
                ['env', '-i', 'true'],
                ['env', 'FOO=1', 'sh', '-c', 'exit 3'],
                ['env', '-u', 'FOO', 'true'],
                ['env', '--unset=FOO', 'true'],
                ['env', '--', 'FOO=1', 'true'],
                ['env', '-', 'true'],
                ['/usr/bin/env', 'FOO=1', 'true'],
                # The forms the review found the guard lost: attached short
                # values and split strings that carry the program.
                ['env', '-i', '-uEXAMPLE_NAME', 'true'],
                ['env', '-i', '-C/x', 'true'],
                ['env', '-i', '-S', '/usr/bin/touch m'],
                ['env', '-i', '-S', 'FOO=1 /usr/bin/touch m'],
                ['env', '-S', 'FOO=1', 'true'],
                ['env', '-iuEXAMPLE_NAME', 'true'],
                ['env', '--split-string=/usr/bin/touch m'],
                ['env', '-C=/x', 'true'],
                ['env', '-i', '--ignore-signal=TERM', 'true'],
                ['env', 'FOO=1', '--help', 'true']):
            with self.subTest(command=command), \
                    patch.object(heavy, 'run_light', return_value=0) as light, \
                    contextlib.redirect_stderr(io.StringIO()), \
                    patch.object(sys, 'argv', ['heavy', '--light', '--', *command]):
                self.assertEqual(heavy.main(), 0)
            light.assert_called_once_with(command)

    def test_env_program_names_the_word_env_would_exec(self):
        # Pinned against the installed uutils env 0.10.0, not the GNU docs:
        # attached short values and split strings are honored, signal
        # options take no separate-word operand, help and version are
        # terminal, and options end at the first assignment or program word.
        cases = [
            # Basic options, assignments, markers, terminals.
            (['true'], 'true'),
            (['FOO=1', 'sh', '-c', 'echo'], 'sh'),
            (['-i', 'FOO=1', 'true'], 'true'),
            (['-u', 'FOO', 'true'], 'true'),
            (['--unset=FOO', 'true'], 'true'),
            (['-C', '/x', 'FOO=1', 'true'], 'true'),
            (['-S', 'FOO=1 true', 'sh'], 'true'),
            (['-a', 'name', 'true'], 'true'),
            (['-ui', 'true'], 'true'),
            (['-uv', 'FOO'], 'FOO'),
            (['-iu', 'FOO', 'true'], 'true'),
            (['-vu', 'FOO'], None),
            (['--ignore-signal=TERM', 'true'], 'true'),
            (['--ignore-signal', 'TERM', 'true'], 'TERM'),
            (['--default-signal', 'TERM', 'true'], 'TERM'),
            (['--block-signal=TERM', 'true'], 'true'),
            (['--', 'FOO=1', 'true'], 'true'),
            (['-', 'FOO=1', 'true'], 'true'),
            (['FOO=1', '--help', 'true'], '--help'),
            (['FOO=1', '-x', 'true'], '-x'),
            (['-h'], None),
            (['-V'], None),
            (['--help'], None),
            (['--version'], None),
            (['-i', '--help', 'true'], None),
            (['-ih'], None),
            (['-hX'], None),
            # Attached short values: the forms the review found the
            # guard lost. '='-attached shorts: one leading '=' is
            # clap's separator, then env checks the value as usual.
            (['-i', '-uEXAMPLE_NAME', 'true'], 'true'),
            (['-i', '-C/tmp', 'true'], 'true'),
            (['-i', '-f/etc/x', 'true'], 'true'),
            (['-iuEXAMPLE_NAME', 'true'], 'true'),
            (['-C=/x', 'true'], 'true'),
            (['--split-string=/usr/bin/touch m'], '/usr/bin/touch'),
            (['-u=FOO', 'true'], None),
            (['-iu=FOO', 'true'], 'true'),
            (['-iu=', 'true'], None),
            (['-iu=FOO=1', 'true'], None),
            (['-iuFOO', 'true'], 'true'),
            (['-iuFOO=1', 'true'], None),
            (['-uFOO=1', 'true'], None),
            (['-a=', 'true'], 'true'),
            (['-aFOO=1', 'true'], 'true'),
            (['-a-', 'true'], 'true'),
            (['-uu', 'true'], 'true'),
            # Empty or '='-bearing -u values never unset anything.
            (['-u', 'FOO=1', 'true'], None),
            (['-u', '', 'true'], None),
            (['-u', '-', 'true'], 'true'),
            (['--unset=', 'true'], None),
            (['--unset', 'x=1', 'true'], None),
            # -C/-f values: non-empty passes (the path may exist);
            # empty is guaranteed dead on env's side.
            (['-C=', 'true'], None),
            (['-C', '', 'true'], None),
            (['-C', '-', 'true'], 'true'),
            (['-f=', 'true'], None),
            (['-f', '', 'true'], None),
            (['--chdir=', 'true'], None),
            (['--file=', 'true'], None),
            (['-i', '-Cdir=x', 'true'], 'true'),
            (['-f', '/nonexistent-x', 'true'], 'true'),
            # Clap refuses a second use of each Set argument.
            (['-C', '/tmp', '-C', '/tmp', 'true'], None),
            (['--chdir', '/tmp', '-C', '/tmp', 'true'], None),
            (['-iSi', '-iSj', 'true'], None),
            (['-i', '-i', 'true'], None),
            (['--ignore-environment', '-i', 'true'], None),
            (['-0', '--null', 'true'], None),
            (['-u', 'A', '-u', 'B', 'true'], 'true'),
            (['-a', 'x', '-a', 'y', 'true'], 'true'),
            # -S splices its split tokens back into the argument list.
            (['-i', '-S', '/usr/bin/touch m'], '/usr/bin/touch'),
            (['-i', '-S', 'FOO=1 /usr/bin/touch m'], '/usr/bin/touch'),
            (['-S', 'FOO=1', 'true'], 'true'),
            (['-S', 'FOO=1'], None),
            (['-S', '   '], None),
            (['-S', ''], None),
            (['-S', 'true', 'arg'], 'true'),
            (['-S', '-u FOO /usr/bin/touch m'], '/usr/bin/touch'),
            (['-S', '-x', 'true'], None),
            (['-S', '-', 'true'], 'true'),
            (['-S-', 'true'], 'true'),
            # Malformed -S strings are env's own errors.
            (['-S', "'abc"], None),
            (['-S', r'a\b'], None),
            # Attached -S forms: the rest of the word is the string
            # (one leading '=' stripped only for --split-string).
            (['-SFOO=1', 'true'], 'true'),
            (['-i', '-SFOO=1', 'true'], 'true'),
            (['-S=FOO=1', 'true'], 'true'),
            (['-S=', 'true'], 'true'),
            (['-S=/usr/bin/touch m'], 'm'),
            (['-S=a', 'b', 'c'], 'b'),
            (['-vS=a', 'b', 'c'], 'b'),
            (['-vSFOO=1', 'true'], 'true'),
            (['-vvSFOO=1', 'true'], 'true'),
            (['--split-string=', 'true'], 'true'),
            (['--split-string', 'FOO=1', 'true'], 'true'),
            # An S buried in a cluster is clap's swallowed -S: it takes
            # a value it never splits; only words beginning -S splice.
            (['-Su', 'true'], 'u'),
            (['-vSu', 'true'], 'u'),
            (['-SuF=1', 'true'], 'true'),
            (['-SuS', 'true'], 'uS'),
            (['-uS', 'true'], 'true'),
            (['-uSu', 'true'], 'true'),
            (['-iSu', 'true'], 'true'),
            (['-iSu', 'u2', 'true'], 'u2'),
            (['-ivSu', 'true'], 'true'),
            (['-iSuF=1', 'true'], 'true'),
            (['-iS=', 'true'], 'true'),
            (['-iSi', 'true'], 'true'),
            (['-0S', 'true'], None),
            (['-0Su', 'true'], None),
            (['-iS', 'true', 'arg'], 'arg'),
            (['-iS', '/usr/bin/touch m'], None),
            (['-viS', '/usr/bin/touch m'], None),
            (['-iS/usr/bin/touch m'], None),
            # The pre-scan stops at the first plain word or --, from
            # the first word on: after it, words are data, full stop.
            (['FOO=1', '-i', 'true'], '-i'),
            (['FOO=1', '--', 'true'], '--'),
            (['FOO=1', '--', '--', 'true'], '--'),
            (['FOO=1', '-', 'true'], 'true'),
            (['FOO=1', '-', '-', 'true'], 'true'),
            (['FOO=1', '-uFOO=2', 'true'], None),
            (['FOO=1', '-SFOO=2', 'true'], 'true'),
            (['FOO=1', '-Cdir=x', 'true'], 'true'),
            (['--', '-uFOO=1', 'true'], 'true'),
            (['--', '-S"m x"', 'true'], '-S"m x"'),
            (['--', '-S', 'a', 'b', 'c'], '-S'),
            (['--', '-Su', 'true'], '-Su'),
            (['--', '-SFOO=1', 'true'], 'true'),
            (['-', '--', 'true'], '--'),
            (['-', '-', '-', 'true'], 'true'),
            (['-', 'true'], 'true'),
            (['-', '-i', 'true'], '-i'),
            (['-', '-uFOO=1', 'true'], None),
            (['-i', '-', '-uFOO=1', 'true'], None),
            (['--argv0', 'x', '-uFOO=1', 'true'], None),
            (['--chdir', '/tmp', '-uFOO=1', 'true'], None),
            (['true', '-uX=1'], 'true'),
            (['--', '-i', 'true'], '-i'),
            # Null: -0 plus any program word is env's own error.
            (['-0', 'true'], None),
            (['-i', '-0', 'true'], None),
            (['--null', 'true'], None),
            (['-0', '-', 'true'], None),
            # Signal options take no separate-word operand; the value
            # must name a real signal (1..64 on this box), and
            # ignore/default refuse KILL and STOP.
            (['--ignore-signal=', 'true'], 'true'),
            (['--ignore-signal=SIGTERM', 'true'], 'true'),
            (['--ignore-signal=term', 'true'], 'true'),
            (['--ignore-signal=KILL', 'true'], None),
            (['--ignore-signal=STOP', 'true'], None),
            (['--default-signal=19', 'true'], None),
            (['--block-signal=KILL', 'true'], 'true'),
            (['--block-signal=STOP', 'true'], 'true'),
            (['--block-signal=19', 'true'], 'true'),
            (['--ignore-signal=0', 'true'], None),
            (['--ignore-signal=64', 'true'], 'true'),
            (['--ignore-signal=65', 'true'], None),
            (['--ignore-signal=FOO', 'true'], None),
            (['--list-signal-handling', 'true'], 'true'),
            (['--list-signal-handling', '--list-signal-handling', 'true'], None),
            # Empty names and empty words.
            (['=FOO', 'true'], 'true'),
            (['=', 'true'], 'true'),
            (['', 'true'], ''),
            (['-i', 'true', 'FOO=1'], 'true'),
            (['-i', 'true', '-SFOO=1'], 'true'),
            # No program left to run.
            (['-C', '/tmp'], None),
            (['-u'], None),
            (['FOO=1'], None),
            (['-i'], None),
            ([], None),
            # Unknown options and hyphen-led option values.
            (['-x', 'true'], None),
            (['-x-', 'true'], None),
            (['--unknown', 'true'], None),
            (['--hel', 'true'], None),
            (['-u', '-x', 'true'], None),
            (['-C', '-x', 'true'], None),
            (['-a', '-x', 'true'], None),
            (['-C', '--', 'true'], None),
            (['-u', '--', 'true'], None),
            (['-a', '--', 'true'], None),
            (['--argv0', '--', 'true'], None),
            # Plain flags and counts.
            (['-v', 'true'], 'true'),
            (['-vv', 'true'], 'true'),
            (['--debug', 'true'], 'true'),
            (['--', 'true'], 'true'),
            (['-i', '--', 'true'], 'true'),
            (['--split-string', 'x', 'true'], 'x'),
            (['-S', 'x', 'y', 'true'], 'x'),
        ]
        for arguments, program in cases:
            with self.subTest(arguments=arguments):
                self.assertEqual(heavy.env_program(arguments), program)

    def test_env_guard_lets_the_legitimate_forms_run_the_real_env(self):
        # The parser tables and a mocked consumer can both drift from the
        # env this box actually ships (uutils 0.10.0 here). For the forms
        # the review named, run the REAL env under heavy's own light
        # supervisor chain (bootstrap -> _supervise_light ->
        # nice/ionice/prlimit -> env: the execution core of --light) and
        # require the marker to appear. The guard may refuse vacuous env
        # jobs; it may never lose these.
        marker = self.root / 'env-legit'
        eqdir = self.root / 'a=b'
        eqdir.mkdir()
        eqmarker = eqdir / 'env-legit-eq'
        for command, target in (
                (['env', '-i', '-S', f'/usr/bin/touch {marker}'], marker),
                (['env', '-i', '-uEXAMPLE_NAME', '/usr/bin/touch', str(marker)], marker),
                (['env', '-i', '-C' + str(self.root), '/usr/bin/touch', marker.name], marker),
                (['env', '-iuEXAMPLE_NAME', '/usr/bin/touch', str(marker)], marker),
                (['env', '-i', '-S', f'FOO=1 /usr/bin/touch {marker}'], marker),
                (['env', '-i', '-SFOO=1', '/usr/bin/touch', str(marker)], marker),
                (['env', '-i', '-C' + str(eqdir), '/usr/bin/touch', str(eqmarker)], eqmarker),
                (['env', '-i', '--ignore-signal=TERM', '/usr/bin/touch', str(marker)], marker)):
            with self.subTest(command=command):
                target.unlink(missing_ok=True)
                bootstrap = ('import sys;sys.path.insert(0,sys.argv.pop(1));'
                             'import heavy;sys.exit(heavy._supervise_light('
                             'sys.argv[2:],float(sys.argv[1])))')
                with subprocess.Popen(
                        [sys.executable, '-B', '-c', bootstrap,
                         str(Path(heavy.__file__).parent), '30', *command],
                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE) as process:
                    _stdout, stderr = process.communicate(timeout=60)
                self.assertEqual(
                    process.returncode, 0,
                    f'real env refused a legitimate form: {stderr.decode()}')
                self.assertTrue(
                    target.exists(), f'real env never ran the wrapped program: {command}')
                target.unlink(missing_ok=True)


    def test_priority_then_fifo_with_legacy_holder_and_late_arrivals(self):
        held = self.raw_lock()
        sequence = [('gate-a', 1), ('gate-b', 1), ('land-a', 0), ('land-b', 0), ('gate-c', 1)]
        children = {}
        for owner, priority in sequence:
            children[owner] = self.child(owner, priority)
            self.enqueued(owner)
        fcntl.flock(held, fcntl.LOCK_UN)
        for owner in ['land-a', 'land-b', 'gate-a', 'gate-b', 'gate-c']:
            self.assertEqual(self.line(children[owner]), 'entered')
            with self.assertRaises(BlockingIOError):
                fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with selectors.DefaultSelector() as ready:
                for name, child in children.items():
                    if name != owner and child.poll() is None:
                        ready.register(child.stdout, selectors.EVENT_READ)
                self.assertEqual(ready.select(0), [], 'another job overlapped or bypassed the queue')
            self.finish(children[owner])
        self.assert_empty()

    def test_landing_does_not_preempt_active_holder(self):
        current = self.child('current')
        self.assertEqual(self.line(current), 'entered')
        landing = self.child('landing', 0)
        self.enqueued('landing')
        with selectors.DefaultSelector() as ready:
            ready.register(landing.stdout, selectors.EVENT_READ)
            self.assertEqual(ready.select(0), [])
        self.finish(current)
        self.assertEqual(self.line(landing), 'entered')
        self.finish(landing)

    def test_status_shows_holder_and_ordered_owners(self):
        current = self.child('current-seat')
        self.assertEqual(self.line(current), 'entered')
        gate = self.child('older-seat')
        self.enqueued('older-seat')
        landing = self.child('landing-seat', 0)
        rows = self.enqueued('landing-seat')
        self.assertTrue(all(row['start'] and row['enqueued_ns'] > 0 for row in rows))
        output = io.StringIO()
        with patch.object(heavy, 'QUEUE_PATH', self.queue.directory), \
                patch.object(heavy, 'HEAVY_LOCK', self.queue.heavy), contextlib.redirect_stdout(output):
            heavy.status()
        lines = output.getvalue().splitlines()
        self.assertIn(f'holder: current-seat pid {current.pid}', lines[0])
        self.assertIn('1: class 0 landing-seat', lines[2])
        self.assertIn('2: class 1 older-seat', lines[3])
        self.finish(current)
        self.assertEqual(self.line(landing), 'entered')
        self.finish(landing)
        self.assertEqual(self.line(gate), 'entered')
        self.finish(gate)

    def test_timeout_names_heavy_stage_position_and_holder_without_running(self):
        current = self.child('holder-seat')
        self.assertEqual(self.line(current), 'entered')
        first = self.child('first-seat', 0)
        self.enqueued('first-seat')
        marker = self.root/'should-not-exist'
        blocked = self.child('timeout-seat', wait=0.5,
                             command=[sys.executable, '-c', f'open({str(marker)!r},"w").close()'])
        original = next(row for row in self.enqueued('timeout-seat') if row['owner']=='timeout-seat')
        with (self.queue.directory/'mutex').open('a') as mutex:
            fcntl.flock(mutex, fcntl.LOCK_EX)
            time.sleep(0.7)  # A brief metadata update crosses the overall wait deadline.
        stdout, stderr = blocked.communicate(timeout=5)
        self.assertEqual(blocked.returncode, 75)
        self.assertEqual(stdout, '')
        self.assertIn(f'heavy lock not acquired after 0.5 s, position 2 behind holder-seat pid {current.pid}', stderr)
        self.assertFalse(marker.exists())
        self.assertFalse(any(row['owner'] == 'timeout-seat' for row in self.queue.snapshot(time.monotonic()+5)))
        self.assertIn('--refund '+original['id'], stderr)
        retry, notification = self.queue.enqueue(1, 'timeout-seat', time.monotonic()+5,
                                                 refund=original['id'])
        try:
            self.assertEqual(retry['enqueued_ns'], original['enqueued_ns'])
            self.assertEqual(retry['class'], original['class'])
        finally:
            notification.close()
            self.queue.remove(retry)
        self.finish(current)
        self.assertEqual(self.line(first), 'entered')
        self.finish(first)

    def test_legacy_timeout_names_legacy_holder(self):
        self.raw_lock()
        blocked = self.child('timeout-seat', wait=0.05)
        _, stderr = blocked.communicate(timeout=5)
        self.assertEqual(blocked.returncode, 75)
        self.assertIn('position 1 behind legacy flock', stderr)
        self.assert_empty()

    def test_cancel_waiter_removes_ticket_and_preserves_fifo(self):
        held = self.raw_lock()
        canceled = self.child('canceled', 0)
        self.enqueued('canceled')
        next_job = self.child('next')
        self.enqueued('next')
        canceled.terminate()
        canceled.communicate(timeout=5)
        self.assertEqual(canceled.returncode, 130)
        fcntl.flock(held, fcntl.LOCK_UN)
        self.assertEqual(self.line(next_job), 'entered')
        self.finish(next_job)
        self.assert_empty()

    def test_sigkill_waiter_reaped_even_before_parent_reaps_zombie(self):
        held = self.raw_lock()
        killed = self.child('killed', 0)
        self.enqueued('killed')
        killed.kill()
        deadline = time.monotonic()+5
        while identity(killed.pid) is not None and time.monotonic() < deadline:
            time.sleep(0.01)
        rows = self.queue.snapshot(deadline)
        self.assertFalse(any(row['owner'] == 'killed' for row in rows))
        killed.communicate(timeout=5)
        next_job = self.child('survivor')
        self.enqueued('survivor')
        fcntl.flock(held, fcntl.LOCK_UN)
        self.assertEqual(self.line(next_job), 'entered')
        self.finish(next_job)
        self.assert_empty()

    def test_reused_pid_start_identity_does_not_keep_stale_landing(self):
        fake = dict(id='1-1', **{'class': 0}, enqueued_ns=1, pid=os.getpid(),
                    start='previous-boot:1', owner='dead-seat', holder=True)
        with self.queue.locked(time.monotonic()+5):
            self.queue.save([fake])
        with heavy.slot('gate', 0.2, heavy_path=self.queue.heavy, queue_path=self.queue.directory):
            rows = self.queue.snapshot(time.monotonic()+5)
            self.assertEqual(len(rows), 1)
            self.assertNotEqual(rows[0]['owner'], 'dead-seat')
        self.assert_empty()

    def test_process_disappearing_during_stat_read_is_absent_but_other_errors_propagate(self):
        stream = io.StringIO('')
        with patch.object(Path, 'open', return_value=stream), \
                patch.object(stream, 'read', side_effect=ProcessLookupError(errno.ESRCH, 'exited')):
            self.assertIsNone(identity(os.getpid()))
        with patch.object(Path, 'open', side_effect=PermissionError(errno.EACCES, 'unreadable')):
            with self.assertRaises(PermissionError):
                identity(os.getpid())
        with patch.object(Path, 'open', return_value=io.StringIO('corrupt stat')):
            with self.assertRaises((ValueError, IndexError)):
                identity(os.getpid())

    def test_corrupt_queue_fails_closed_without_acquiring_heavy(self):
        (self.queue.directory/'tickets.json').write_text('{"version":1,"tickets":[{}]}')
        with self.assertRaisesRegex(ValueError, 'Invalid heavy queue ticket'):
            with heavy.slot('gate', heavy_path=self.queue.heavy, queue_path=self.queue.directory):
                self.fail('Corrupt queue admitted work')
        self.raw_lock()

    def test_fifo_metadata_refuses_without_blocking_or_running(self):
        os.mkfifo(self.queue.directory/'tickets.json')
        refused = self.child('fifo-seat')
        stdout, stderr = refused.communicate(timeout=5)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(stdout, '')
        self.assertIn('metadata must be a regular file', stderr)
        self.raw_lock()

    def test_symlink_staging_refuses_and_preserves_the_target(self):
        victim = self.root/'private-work'
        victim.write_text('preserve this work')
        (self.queue.directory/'tickets.next').symlink_to(victim)
        refused = self.child('symlink-seat')
        stdout, _ = refused.communicate(timeout=5)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(stdout, '')
        self.assertEqual(victim.read_text(), 'preserve this work')
        self.raw_lock()

    def test_ticket_bound_refuses_without_losing_existing_jobs(self):
        fake = dict(id='1-1', **{'class': 1}, enqueued_ns=1, pid=os.getpid(),
                    start=identity(os.getpid()), owner='live-seat', holder=False)
        tickets = [dict(fake, id=f'1-{i+1}', enqueued_ns=i+1) for i in range(1024)]
        with self.queue.locked(time.monotonic()+5):
            self.queue.save(tickets)
        with self.assertRaisesRegex(RuntimeError, 'queue is full'):
            self.queue.enqueue(1, 'new-seat', time.monotonic()+5)
        self.assertEqual(len(self.queue.snapshot(time.monotonic()+5)), 1024)

    def test_cli_preserves_exit_code_and_joins_canceled_child_before_unlock(self):
        finished = self.child('exit-seat', command=[sys.executable, '-c', 'raise SystemExit(7)'])
        finished.communicate(timeout=load_guard(5.0))
        self.assertEqual(finished.returncode, 7)
        self.assert_empty()
        running = self.child('running-seat', command=[sys.executable, '-u', '-c',
                             'import os,time;print(os.getpid(),flush=True);time.sleep(300)'])
        child_pid = int(self.line(running))
        with self.assertRaises(BlockingIOError):
            self.raw_lock()
        running.terminate()
        # Wait on the wrapper alone, never on its pipes: the guard only bounds
        # how long a loaded host may take to deliver its own exit, because the
        # join is proven below, at that exit. Draining first would let a child
        # that merely outlived the wrapper and exited on its own satisfy the
        # witness. heavy.stop_group allows one SIGTERM grace and one SIGKILL
        # grace before it refuses, so the guard clears both phases.
        running.wait(timeout=load_guard(2*5.0))
        self.assertEqual(running.returncode, 130)
        # The wrapper has exited, so its cleanup has already run: the child must
        # be gone now, whatever the wait took.
        self.assertIsNone(identity(child_pid), 'owned child still runs after wrapper cancellation')
        self.assertFalse(heavy.live_group(child_pid), 'owned child group survived the wrapper')
        self.assert_empty()
        # Draining the pipes is cleanup now that the witness above has passed.
        running.communicate(timeout=load_guard(2*5.0))
        self.raw_lock()

    def test_slot_caps_cargo_jobs_and_restores_the_callers_environment(self):
        for initial, expected in [('48','3'),('8','3'),('1','1'),(None,'3')]:
            with self.subTest(initial=initial), patch.dict(os.environ, {}, clear=False):
                if initial is None: os.environ.pop('CARGO_BUILD_JOBS', None)
                else: os.environ['CARGO_BUILD_JOBS'] = initial
                with heavy.slot('gate', 5, heavy_path=self.queue.heavy,
                                queue_path=self.queue.directory):
                    self.assertEqual(os.environ['CARGO_BUILD_JOBS'], expected)
                self.assertEqual(os.environ.get('CARGO_BUILD_JOBS'), initial)

    def test_nvme_detection_refuses_unmounted_root_and_other_filesystem(self):
        import heavy_runs
        with patch.object(heavy_runs.os.path, 'ismount', return_value=False):
            self.assertIsNone(heavy_runs.nvme_target(self.root))
        with patch.object(heavy_runs.os.path, 'ismount', return_value=True), \
                patch.object(heavy_runs, 'NVME_ROOT', self.root):
            self.assertEqual(heavy_runs.nvme_target(self.root/'future'/'target'),
                             self.root/'future'/'target')
        with patch.object(heavy_runs, 'NVME_ROOT', Path('/proc')):
            self.assertIsNone(heavy_runs.nvme_target(self.root))

    def test_enqueue_time_and_class_are_the_only_scheduling_keys(self):
        held = self.raw_lock()
        older = self.child('z-owner')
        rows = self.enqueued('z-owner')
        newer = self.child('a-owner')
        rows = self.enqueued('a-owner')
        pending = sorted(rows, key=order)
        self.assertEqual([r['owner'] for r in pending], ['z-owner', 'a-owner'])
        self.assertLess(pending[0]['enqueued_ns'], pending[1]['enqueued_ns'])
        fcntl.flock(held, fcntl.LOCK_UN)
        self.assertEqual(self.line(older), 'entered')
        self.finish(older)
        self.assertEqual(self.line(newer), 'entered')
        self.finish(newer)

    def test_sigkill_wrapper_keeps_child_lease_until_child_is_joined(self):
        # A private subreaper owns both descendants, so the crash witness leaves
        # no live orphan or unreaped child in the workstation's process tree.
        worker = '''import sys
from pathlib import Path
import heavy
import heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host=hardware_sample
heavy.HEAVY_LOCK=Path(sys.argv[1])/'heavy'
heavy.QUEUE_PATH=Path(sys.argv[1])/'queue'
sys.argv=['heavy','--exclusive','--owner','crash-seat','--',sys.executable,'-u','-c',
          'import os,time;print(os.getpid(),flush=True);time.sleep(30)']
heavy.main()
'''
        guardian = '''import ctypes,fcntl,os,signal,subprocess,sys
from pathlib import Path
from heavy_queue import Queue
libc=ctypes.CDLL(None,use_errno=True)
if libc.prctl(36,1,0,0,0) != 0: raise OSError(ctypes.get_errno(),'subreaper unavailable')
wrapper=subprocess.Popen([sys.executable,'-u','-c',sys.argv[2],sys.argv[1]],stdout=subprocess.PIPE,text=True)
child=None
try:
    child=int(wrapper.stdout.readline())
    wrapper.kill()
    wrapper.wait(timeout=5)
    queue=Queue(Path(sys.argv[1])/'queue',Path(sys.argv[1])/'heavy')
    import time
    assert queue.snapshot(time.monotonic()+5) == [], 'dead wrapper ticket was not reaped'
    with queue.heavy.open('a') as probe:
        try: fcntl.flock(probe,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError: pass
        else: raise AssertionError('killed wrapper released its live child lease')
    os.killpg(child,signal.SIGTERM)
    os.waitpid(child,0)
    child=None
    with queue.heavy.open('a') as probe: fcntl.flock(probe,fcntl.LOCK_EX|fcntl.LOCK_NB)
finally:
    if wrapper.poll() is None:
        wrapper.kill()
        wrapper.wait(timeout=5)
    if child is not None:
        os.killpg(child,signal.SIGKILL)
        os.waitpid(child,0)
    wrapper.stdout.close()
'''
        result = subprocess.run([sys.executable, '-B', '-c', guardian, str(self.root), worker],
                                cwd=Path(heavy.__file__).parent, env=isolated_environment(),
                                capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
        self.assert_empty()


if __name__ == '__main__':
    unittest.main()
