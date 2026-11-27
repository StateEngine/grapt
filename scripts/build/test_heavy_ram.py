"""Real private flocks and processes at the RAM/disk admission boundary."""
import fcntl
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import heavy
from fixture_environment import isolated_environment
from heavy_queue import Queue
from heavy_runs import Admission
import heavy_hardware
import heavy_sampling


WORKER = r'''
import json,os,shutil,signal,subprocess,sys
from pathlib import Path
import heavy,heavy_runs,ram_targets as ram
root=Path(sys.argv[1])
heavy.HEAVY_LOCK=root/'heavy'
heavy.QUEUE_PATH=root/'queue'
ram.ROOT=root/'ram'
ram.available=lambda:int((root/'available').read_text())
disk_usage=shutil.disk_usage
def fixture_disk_usage(path):
    usage=disk_usage(path)
    return usage._replace(total=ram.TOTAL_BYTES,used=0,free=ram.TOTAL_BYTES) if Path(path)==ram.ROOT else usage
shutil.disk_usage=fixture_disk_usage
try:
    import heavy_sampling,external_holders,time
    external_holders.cpu_ticks=lambda:[0,time.monotonic_ns()//100000]
    from heavy_queue import Queue
    original_shared=Queue.shared_locks
    sampled=False
    def shared(self,*args,**kwargs):
        global sampled
        count=original_shared(self,*args,**kwargs)
        if (root/'underreported-locks').exists():
            if sys.argv[2]=='fifth' and not sampled:
                sampled=True
                print('sampled undercount',flush=True)
            return 0
        return count
    Queue.shared_locks=shared
    heavy_sampling.host=lambda:dict(available=ram.available(), cores=48,
        load=128 if (root/'weighted-cpu').exists() else 0, nvme_free=1024**4,
        weighted_cpu=(root/'weighted-cpu').exists(),
        root_free=int((root/'root-free').read_text()) if (root/'root-free').exists() else 1024**4)
    import heavy_hardware
    original_projection=heavy_hardware.Profiles.projection
    def projection(self,ticket,ram=False):
        row=original_projection(self,ticket,ram)
        if not ram and (root/'peaks.json').exists():
            peaks=json.loads((root/'peaks.json').read_text())
            row['peak_rss']=peaks.get(ticket['owner'],row['peak_rss'])
        return row
    heavy_hardware.Profiles.projection=projection
    register=heavy_hardware.Profiles.register
    def registration(self,ticket,*args,**kwargs):
        if ticket['owner'] != 'unprofiled-fifth':return register(self,ticket,*args,**kwargs)
    heavy_hardware.Profiles.register=registration
    if (root/'allocation-fails').exists():
        def failed_allocation(*args):raise ValueError('injected reservation race')
        ram.allocate=failed_allocation
except ModuleNotFoundError:
    pass  # The original old-client fixture predates resource sampling.
(root/'nvme').mkdir(exist_ok=True)
heavy_runs.nvme_target=lambda path: Path(path).resolve() if Path(path).resolve().is_relative_to(root/'nvme') else None
heavy.nvme_target=heavy_runs.nvme_target
original=ram.subprocess.check_output
ram.subprocess.check_output=lambda args,**kw: 'tmpfs\n' if args[0]=='findmnt' else original(args,**kw)
def remove(path,row):
    assert ram.read_run(path)==row
    shutil.rmtree(path)
ram.remove=remove
def interrupted(*args):raise KeyboardInterrupt
signal.signal(signal.SIGTERM,interrupted)
# Match heavy.main even when a detached gate inherited an ignored interrupt.
signal.signal(signal.SIGINT,interrupted)
try:
    with heavy.slot('landing' if sys.argv[4]=='0' else 'gate',next((float(a[5:]) for a in sys.argv[6:] if a.startswith('wait=')),10),
                    job_owner=sys.argv[2],ram=sys.argv[3] in ('ram','ram-nvme'),
                    **({'nvme':root/'nvme'} if 'nvme' in sys.argv[3] else {}),
                    **({'projected':float(sys.argv[5])} if sys.argv[5] != '10800' else {})):
        mode=heavy.inherited()[3]['slot']
        heavy.note_target(mode)
        print('entered '+mode,flush=True)
        action=sys.stdin.readline()
        if action.strip()=='nested-gate':
            from unittest.mock import patch
            import evict_targets
            with patch.dict(os.environ,{'CARGO_TARGET_DIR':str(root/'nvme'),
                                       'SECODE_TARGET_ROOT':str(root/'nvme')}), \
                    patch.object(evict_targets,'prepare'), \
                    patch.object(ram,'allocate',side_effect=AssertionError('unpriced RAM allocation')), \
                    patch.object(ram,'pressure',return_value='private fixture'):
                assert ram.run_gate(['affected','--log',str(root/'nested.log')],root/'nested.log') is None
            print('nested disk',flush=True)
            sys.stdin.readline()
        if action.strip()=='retain':
            child=subprocess.Popen([sys.executable,'-c','import sys; sys.stdin.buffer.read(1)'],
                                   pass_fds=heavy.inherited_fds(), stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
            print('retained '+str(child.pid),flush=True)
            sys.stdin.readline()
        if action.strip()=='interrupt':
            from unittest.mock import patch
            import evict_targets
            def cancelled(**kwargs):
                if kwargs:return 130
                print('scope started',flush=True)
                sys.stdin.readline()
                raise KeyboardInterrupt
            with patch.dict(os.environ,{'CARGO_TARGET_DIR':str(root/'disk'),
                                       'SECODE_TARGET_ROOT':str(root/'disk')}), \
                    patch.object(evict_targets,'prepare'),patch.object(ram.subprocess,'Popen') as launch:
                launch.return_value.wait.side_effect=cancelled
                try:ram.run_gate(['affected','--log',str(root/'gate.log')],root/'gate.log')
                except KeyboardInterrupt:pass
            print('resumed '+heavy.inherited()[3]['slot'],flush=True)
            sys.stdin.readline()
        if action.strip()=='nvme-fallback':
            ram.remove(*heavy.reservation())
            heavy.require_disk(root/'nvme')
            heavy.note_target('disk')
            print('shared nvme',flush=True)
            sys.stdin.readline()
        if action.strip()=='fallback':
            ram.remove(*heavy.reservation())
            heavy.require_exclusive()
            heavy.note_target('disk')
            print('exclusive disk',flush=True)
            sys.stdin.readline()
except KeyboardInterrupt:
    raise SystemExit(130)
'''

LOCK_CHURN = r'''
import fcntl,select,sys
from pathlib import Path
root=Path(sys.argv[1]);root.mkdir()
streams=[open(root/str(index),'w') for index in range(256)]
try:
    print('ready',flush=True)
    while not select.select([sys.stdin],[],[],0)[0]:
        for stream in streams:fcntl.flock(stream,fcntl.LOCK_SH)
        for stream in streams:fcntl.flock(stream,fcntl.LOCK_UN)
finally:
    for stream in streams:stream.close()
'''


def load_bound(base=5.0, ceiling=60.0):
    # A loaded box admits the same worker later, not never: scale the bound
    # by observed load instead of failing an idle-box deadline.
    load = os.getloadavg()[0] / max(1, os.cpu_count() or 1)
    return min(ceiling, base * max(1.0, min(12.0, load)))


class HeavyRamTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='secode-ram-queue-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.set_available(str(128 * 1024**3))
        self.queue = Queue(self.root / 'queue', self.root / 'heavy')
        heavy_hardware.set_ceiling(self.queue.directory, 3, 'private fixture capacity')
        sample = lambda: dict(available=int((self.root/'available').read_text()),
                              cores=48, load=0, nvme_free=1024**4,
                              root_free=int((self.root/'root-free').read_text())
                              if (self.root/'root-free').exists() else 1024**4)
        hardware = patch.object(heavy_sampling, 'host', side_effect=sample)
        hardware.start()
        self.addCleanup(hardware.stop)

    def set_available(self, text):
        # Workers poll this file with int(read_text()) on every admission
        # sample; write_text truncates before writing, so replace atomically.
        target = self.root / 'available'
        (self.root / 'available.staging').write_text(text)
        os.replace(self.root / 'available.staging', target)

    def test_nvme_wrapper_cannot_allocate_unpriced_nested_ram(self):
        worker = self.child('nested-nvme', 'nvme')
        self.assertEqual(self.line(worker), 'entered disk')
        worker.stdin.write(b'nested-gate\n')
        self.assertTrue(self.line(worker).startswith('Gate target: disk; fallback;'))
        self.assertEqual(self.line(worker), 'nested disk')
        self.finish(worker)
        history = json.loads((self.queue.directory/'runs.json').read_text())['recent']
        self.assertEqual(history[-1]['targets'], ['disk'])

    def test_orphan_ram_growth_is_reserved_before_nvme_admission(self):
        first = self.child('orphan-ram')
        self.assertEqual(self.line(first), 'entered ram')
        first.stdin.write(b'retain\n')
        child_pid = int(self.line(first).removeprefix('retained '))
        child_fd = os.pidfd_open(child_pid)
        try:
            first.kill()
            first.wait(timeout=5)
            self.set_available(str(40*1024**3))
            disk = self.child('new-nvme', 'nvme')
            self.enqueued('new-nvme')
            self.blocked(disk, 'orphan RAM growth was priced as an ordinary disk job')
            signal.pidfd_send_signal(child_fd, signal.SIGKILL)
            self.assertEqual(self.line(disk), 'entered disk')
            self.finish(disk)
        finally:
            try:
                signal.pidfd_send_signal(child_fd, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.close(child_fd)

    def test_hardware_fifth_slot_keeps_legacy_waiter_and_kernel_leases_intact(self):
        import gate_capacity
        gate_capacity.set_limit(self.queue, 4, time.time())
        heavy_hardware.set_ceiling(self.queue.directory, 12, 'hardware fixture')
        workers = [self.child(f'hardware-{i}', 'nvme') for i in range(4)]
        for worker in workers:
            self.assertEqual(self.line(worker), 'entered disk')
        legacy = self.child('older-three-slot-client', 'nvme', legacy='three')
        self.enqueued('older-three-slot-client')
        fifth = self.child('unprofiled-fifth', 'nvme')
        self.assertEqual(self.line(fifth), 'entered disk')
        self.blocked(legacy, 'old client exceeded its original capacity')
        self.assertEqual(len([r for r in self.queue.snapshot(time.monotonic()+5) if r['holder']]), 5)
        self.finish(fifth)
        for worker in workers[:2]:
            self.finish(worker)
        self.assertEqual(self.line(legacy), 'entered disk')
        for worker in workers[2:]+[legacy]:
            self.finish(worker)

    def test_landing_has_one_reserved_slot_above_the_ordinary_ceiling(self):
        workers = [self.child(f'ordinary-{i}', 'nvme') for i in range(3)]
        for worker in workers:
            self.assertEqual(self.line(worker), 'entered disk')
        ordinary = self.child('ordinary-waiter', 'nvme')
        self.enqueued('ordinary-waiter')
        landing = self.child('landing-composition', 'nvme', priority=0)
        self.assertEqual(self.line(landing), 'entered disk')
        self.blocked(ordinary, 'ordinary work consumed the reserved landing slot')
        self.finish(workers[0])
        self.assertEqual(self.line(ordinary), 'entered disk')
        another = self.child('another-landing', 'nvme', priority=0)
        self.enqueued('another-landing')
        self.blocked(another, 'more than one extra landing slot was admitted')
        self.finish(landing)
        self.assertEqual(self.line(another), 'entered disk')
        for worker in [*workers[1:], ordinary, another]:
            self.finish(worker)

    def test_failed_ram_reservation_rechecks_actual_disk_projection(self):
        (self.root/'weighted-cpu').touch()
        self.set_available(str(50*1024**3))
        (self.root/'peaks.json').write_text(json.dumps({'fallback':40*1024**3}))
        (self.root/'allocation-fails').touch()
        worker=self.child('fallback','ram-nvme')
        self.enqueued('fallback')
        self.blocked(worker,'disk fallback violated the projected RAM reserve')
        self.assertEqual(self.occupancy(),0,'a refused fallback retained its kernel lease')
        self.set_available(str(80*1024**3))
        self.assertEqual(self.line(worker),'entered disk')
        self.finish(worker)

    def test_normal_priority_hygiene_can_recover_a_low_root_filesystem(self):
        (self.root/'root-free').write_text('1')
        worker=self.child('disk-hygiene','nvme',priority=1)
        self.assertEqual(self.line(worker),'entered disk')
        self.finish(worker)

    def test_resource_blocked_shared_head_cannot_be_bypassed_by_a_smaller_job(self):
        self.set_available(str(30*1024**3))
        (self.root/'peaks.json').write_text(json.dumps({'older-large':20*1024**3,'later-small':1024**3}))
        older=self.child('older-large','nvme')
        self.enqueued('older-large')
        self.blocked(older,'large head should wait for memory')
        later=self.child('later-small','nvme')
        self.enqueued('later-small')
        self.blocked(later,'small shared job bypassed the resource-blocked FIFO head')
        self.set_available(str(80*1024**3))
        self.assertEqual(self.line(older),'entered disk')
        self.assertEqual(self.line(later),'entered disk')
        self.finish(older)
        self.finish(later)

    def cleanup(self, child):
        if child.poll() is None:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)
        for stream in [child.stdin, child.stdout, child.stderr]:
            stream.close()

    def child(self, name, mode='ram', priority=1, *, legacy=False, projected=10800, wait=None):
        directory = Path(heavy.__file__).parent
        if legacy:
            directory = self.root / 'legacy'
            directory.mkdir(exist_ok=True)
            revision = ('5726e85130fb49d72c15626c2f3886ea9aa2d367' if legacy == 'three'
                        else '38f2fa900d775b4f6ab18b3a18249b2ceea14d48')
            if subprocess.run(['git', 'cat-file', '-e', revision + '^{commit}'],
                              cwd=Path(heavy.__file__).parent,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0:
                self.skipTest('legacy pre-public revision unavailable in repository history')
            for filename in ('heavy.py', 'heavy_queue.py', 'heavy_runs.py', 'ram_targets.py'):
                content = subprocess.check_output(
                    ['git', 'show', revision + ':scripts/build/' + filename],
                    cwd=Path(heavy.__file__).parent)
                (directory / filename).write_bytes(content)
        args = [sys.executable, '-B', '-u', '-c', WORKER,
                str(self.root), name, mode, str(priority), str(projected)]
        if wait is not None:
            # Named, because derived workers already pass their own argv[6].
            args.append(f'wait={wait}')
        process = subprocess.Popen(args,
                                   cwd=directory, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   start_new_session=True, bufsize=0, env=isolated_environment())
        self.addCleanup(self.cleanup, process)
        return process

    def line(self, child):
        data = bytearray()
        deadline = time.monotonic() + load_bound()
        while len(data) < 4096:
            self.assertTrue(select.select([child.stdout], [], [], max(0, deadline-time.monotonic()))[0],
                            'worker never reached its expected admission boundary')
            byte = os.read(child.stdout.fileno(), 1)
            if byte == b'':
                self.fail('worker exited before its boundary: ' + child.stderr.read().decode())
            if byte == b'\n':
                return data.decode()
            data.extend(byte)
        self.fail('worker output exceeded its bound')

    def enqueued(self, name):
        deadline = time.monotonic() + load_bound()
        while time.monotonic() < deadline:
            rows = self.queue.snapshot(deadline)
            if any(row['owner'] == name for row in rows):
                return rows
            time.sleep(0.01)
        self.fail('worker never queued: ' + name)

    def blocked(self, child, reason):
        readable = select.select([child.stdout], [], [], 0.15)[0]
        if readable or child.poll() is not None:
            status = child.poll()
            output = {}
            for name, stream in [('stdout', child.stdout), ('stderr', child.stderr)]:
                data = bytearray()
                while len(data) < 65536 and select.select([stream], [], [], 0)[0]:
                    chunk = os.read(stream.fileno(), min(4096, 65536-len(data)))
                    if not chunk:
                        break
                    data.extend(chunk)
                output[name] = data.decode(errors='replace')
            boundary = 'unexpected admission' if 'entered ' in output['stdout'] else 'worker failure or EOF'
            self.fail(f'{reason}: {boundary}; exit={status}; {output}')

    def finish(self, child):
        child.stdin.write(b'\n')
        self.assertEqual(child.wait(timeout=5), 0, child.stderr.read().decode())

    def occupancy(self):
        import gate_capacity
        deadline = time.monotonic() + load_bound()
        while time.monotonic() < deadline:
            count = gate_capacity.occupancy(self.queue)
            if count is not None:
                return count
            time.sleep(0.01)
        self.fail('Heavy lease descriptors remained unreadable for the capacity witness')

    def start_churn(self):
        process = subprocess.Popen([sys.executable, '-u', '-c', LOCK_CHURN,
            str(self.root / 'churn')], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True, bufsize=0)
        self.addCleanup(self.cleanup, process)
        self.assertEqual(self.line(process), 'ready')
        return process

    def test_blocked_boundary_distinguishes_admission_from_worker_failure(self):
        cases = [('print("entered disk",flush=True)', 'unexpected admission.*entered disk'),
                 ('import sys; print("queue fault",file=sys.stderr); sys.exit(2)',
                  'worker failure or EOF.*exit=2.*queue fault')]
        for script, diagnostic in cases:
            with self.subTest(diagnostic=diagnostic):
                child = subprocess.Popen([sys.executable, '-u', '-c', script],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    start_new_session=True, bufsize=0)
                self.addCleanup(self.cleanup, child)
                child.wait(timeout=5)
                with self.assertRaisesRegex(AssertionError, diagnostic):
                    self.blocked(child, 'expected waiting boundary')

    def test_missing_inherited_descriptors_name_the_requeue_action(self):
        from unittest.mock import patch
        with patch.object(heavy, 'HEAVY_LOCK', self.root/'heavy'), \
                patch.object(heavy, 'QUEUE_PATH', self.root/'queue'):
            with heavy.slot('landing', 5, job_owner='stale-claim') as lease:
                for descriptors in ((), (lease.fileno(),)):
                    with self.subTest(pass_fds=descriptors):
                        result = subprocess.run(
                            [sys.executable, '-B', '-c', 'import heavy; heavy.inherited()'],
                            cwd=Path(heavy.__file__).parent, pass_fds=descriptors,
                            capture_output=True, text=True, timeout=5)
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn('Inherited heavy lease descriptors are unavailable', result.stderr)
                        self.assertIn('requeue from a fresh landing helper', result.stderr)
                        self.assertNotIn('Bad file descriptor', result.stderr)
                        self.assertNotIn('Socket operation on non-socket', result.stderr)

    def test_three_ram_holders_preserve_the_v1_ticket_wire_format(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        second = self.child('second')
        self.assertEqual(self.line(second), 'entered ram')
        rows = self.queue.snapshot(time.monotonic() + 5)
        self.assertEqual(sum(row['holder'] for row in rows), 2)
        physical = json.loads((self.queue.directory / 'tickets.json').read_text())
        self.assertEqual(physical['version'], 1)
        self.assertEqual(sum(row['holder'] for row in physical['tickets']), 1)
        for row in physical['tickets']:
            self.assertEqual(set(row), {'id', 'class', 'enqueued_ns', 'pid', 'start', 'owner', 'holder'})
        with self.queue.heavy.open('a') as probe:
            fcntl.flock(probe, fcntl.LOCK_SH | fcntl.LOCK_NB)
            fcntl.flock(probe, fcntl.LOCK_UN)
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
        third = self.child('third')
        self.assertEqual(self.line(third), 'entered ram')
        fourth = self.child('fourth')
        self.enqueued('fourth')
        self.blocked(fourth, 'a fourth holder exceeded the three-slot bound')
        self.finish(first)
        self.assertEqual(self.line(fourth), 'entered ram')
        self.finish(second)
        self.finish(third)
        self.finish(fourth)
        history = json.loads((self.queue.directory / 'runs.json').read_text())['recent']
        self.assertEqual({row['owner'] for row in history}, {'first', 'second', 'third', 'fourth'})
        self.assertTrue(all(row['concurrent'] and row['hold_seconds'] > 0 for row in history))

    def test_waiting_exclusive_job_does_not_block_available_ram_or_nvme(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        disk = self.child('disk-head', 'disk')
        self.enqueued('disk-head')
        later = self.child('later-composition', priority=0)
        self.assertEqual(self.line(later), 'entered ram')
        nvme = self.child('nvme', 'nvme')
        self.assertEqual(self.line(nvme), 'entered disk')
        self.blocked(disk, 'exclusive work overlapped shared holders')
        self.finish(first)
        self.finish(later)
        self.finish(nvme)
        self.assertEqual(self.line(disk), 'entered disk')
        self.finish(disk)

    def test_legacy_exclusive_waiter_still_enters_after_shared_jobs_finish(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        disk = self.child('legacy-disk', 'disk', legacy=True)
        self.enqueued('legacy-disk')
        later = self.child('new-composition', priority=0)
        self.assertEqual(self.line(later), 'entered ram')
        self.blocked(disk, 'legacy exclusive holder overlapped shared work')
        self.finish(first)
        self.finish(later)
        self.assertEqual(self.line(disk), 'entered disk')
        self.finish(disk)

    def test_cancelled_exclusive_waiter_preserves_landing_priority(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        disk = self.child('cancelled-head', 'disk', legacy=True)
        self.enqueued('cancelled-head')
        later = self.child('later', priority=0)
        self.assertEqual(self.line(later), 'entered ram')
        disk.terminate()
        self.assertEqual(disk.wait(timeout=5), 130)
        wire = json.loads((self.queue.directory / 'tickets.json').read_text())['tickets']
        self.assertEqual(next(t['class'] for t in wire if t['owner'] == 'later'), 0)
        self.finish(first)
        self.finish(later)

    def test_three_nvme_jobs_share_and_fourth_waits(self):
        workers = [self.child(name, 'nvme') for name in ('one','two','three')]
        for worker in workers:
            self.assertEqual(self.line(worker), 'entered disk')
        fourth = self.child('fourth', 'nvme')
        self.enqueued('fourth')
        self.blocked(fourth, 'four NVMe jobs exceeded the host bound')
        self.finish(workers[0])
        self.assertEqual(self.line(fourth), 'entered disk')
        for worker in workers[1:] + [fourth]:
            self.finish(worker)

    def test_ram_capacity_refusal_does_not_block_nvme_and_falls_back_shared(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        # Missing fixture cgroups receive no materialized-page credit. Keep
        # enough RAM for disk projections while refusing a second RAM target.
        self.set_available(str(64*1024**3))
        refused = self.child('refused-ram')
        self.enqueued('refused-ram')
        nvme = self.child('later-nvme', 'nvme')
        self.assertEqual(self.line(nvme), 'entered disk')
        fallback = self.child('fallback', 'ram-nvme')
        self.assertEqual(self.line(fallback), 'entered disk')
        self.blocked(refused, 'RAM exceeded available-memory admission')
        self.set_available(str(47*1024**3))
        self.finish(first)
        self.finish(nvme)
        self.finish(fallback)
        self.assertEqual(self.line(refused), 'entered disk')
        self.finish(refused)

    def test_running_ram_fallback_to_nvme_preserves_peer(self):
        first = self.child('peer')
        self.assertEqual(self.line(first), 'entered ram')
        second = self.child('fallback', 'ram-nvme')
        self.assertEqual(self.line(second), 'entered ram')
        second.stdin.write(b'nvme-fallback\n')
        self.assertEqual(self.line(second), 'shared nvme')
        self.assertIsNone(first.poll())
        self.finish(first)
        self.finish(second)

    def test_orphan_child_and_exclusive_waiter_allow_two_nvme_peers_but_not_a_fourth(self):
        first = self.child('orphan', 'nvme')
        self.assertEqual(self.line(first), 'entered disk')
        first.stdin.write(b'retain\n')
        child_pid = int(self.line(first).removeprefix('retained '))
        try:
            first.kill()
            first.wait(timeout=5)
            exclusive = self.child('exclusive-waiter', 'disk')
            self.enqueued('exclusive-waiter')
            peers = [self.child(name, 'nvme') for name in ('peer-a','peer-b')]
            for peer in peers:
                self.assertEqual(self.line(peer), 'entered disk')
            fourth = self.child('fourth', 'nvme')
            self.enqueued('fourth')
            self.blocked(fourth, 'a reaped wrapper hid its live child slot')
            os.kill(child_pid, signal.SIGKILL)
            child_pid = None
            self.assertEqual(self.line(fourth), 'entered disk')
            for peer in peers + [fourth]:
                self.finish(peer)
            self.assertEqual(self.line(exclusive), 'entered disk')
            self.finish(exclusive)
        finally:
            if child_pid is not None:
                try: os.kill(child_pid, signal.SIGKILL)
                except ProcessLookupError: pass

    def test_admission_intent_persists_without_rewriting_unchanged_waits(self):
        state = Admission(self.queue.directory)
        state.requests['1-1'] = True
        state.save()
        path = self.queue.directory / 'admission.json'
        before = path.stat().st_mtime_ns
        restored = Admission(self.queue.directory)
        self.assertEqual(restored.requests, {'1-1': True})
        restored.save()
        self.assertEqual(path.stat().st_mtime_ns, before)
        path.write_text('{"version":1,"requests":{"1-1":"ram"},"head":null}')
        with self.assertRaisesRegex(ValueError, 'Invalid heavy admission'):
            Admission(self.queue.directory)
        path.write_bytes(b' ' * (256 * 1024 + 1))
        with self.assertRaisesRegex(ValueError, 'exceeds its bound'):
            Admission(self.queue.directory)

    def test_second_ram_job_cannot_bypass_the_available_memory_check(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        self.set_available(str(71 * 1024**3))
        second = self.child('second')
        self.enqueued('second')
        self.blocked(second, 'second RAM job bypassed the 24 GiB memory floor')
        rows = self.queue.snapshot(time.monotonic() + 5)
        self.assertEqual([row['owner'] for row in rows if row['holder']], ['first'])
        self.set_available(str(128 * 1024**3))
        self.assertEqual(self.line(second), 'entered ram')
        self.finish(first)
        self.finish(second)

    def test_solo_ram_refusal_runs_disk_exclusively(self):
        self.set_available(str(47 * 1024**3))
        first = self.child('disk-fallback')
        self.assertEqual(self.line(first), 'entered disk')
        self.set_available(str(128 * 1024**3))
        second = self.child('ram-peer')
        self.enqueued('ram-peer')
        self.blocked(second, 'RAM job entered alongside the disk fallback')
        with self.queue.heavy.open('a') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_SH | fcntl.LOCK_NB)
        self.finish(first)
        self.assertEqual(self.line(second), 'entered ram')
        self.finish(second)

    def test_failed_ram_attempt_waits_for_exclusive_before_disk(self):
        first = self.child('peer')
        self.assertEqual(self.line(first), 'entered ram')
        second = self.child('fallback')
        self.assertEqual(self.line(second), 'entered ram')
        second.stdin.write(b'fallback\n')
        self.blocked(second, 'disk fallback ran before the RAM peer finished')
        self.finish(first)
        self.assertEqual(self.line(second), 'exclusive disk')
        with self.queue.heavy.open('a') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_SH | fcntl.LOCK_NB)
        self.finish(second)
        history = json.loads((self.queue.directory / 'runs.json').read_text())['recent']
        run = next(row for row in history if row['owner'] == 'fallback')
        self.assertEqual(run['targets'], ['ram', 'disk'])
        self.assertTrue(run['concurrent'])

    def test_nested_gate_retains_its_opaque_callers_exclusive_disk_lease(self):
        opaque = self.child('opaque', 'disk')
        self.assertEqual(self.line(opaque), 'entered disk')
        opaque.stdin.write(b'nested-gate\n')
        self.assertIn('fallback', self.line(opaque))
        self.assertEqual(self.line(opaque), 'nested disk')
        landing = self.child('landing', priority=0)
        self.enqueued('landing')
        self.blocked(landing, 'opaque nested gate unexpectedly shared its lease')
        with self.queue.heavy.open('a') as probe:
            with self.assertRaises(BlockingIOError):
                fcntl.flock(probe, fcntl.LOCK_SH | fcntl.LOCK_NB)
        self.finish(opaque)
        self.assertEqual(self.line(landing), 'entered ram')
        self.finish(landing)

    def test_killed_ram_wrapper_cannot_release_a_live_childs_shared_flock(self):
        first = self.child('killed-wrapper')
        self.assertEqual(self.line(first), 'entered ram')
        first.stdin.write(b'retain\n')
        child_pid = int(self.line(first).removeprefix('retained '))
        try:
            first.kill()
            first.wait(timeout=5)
            disk = self.child('exclusive', 'disk')
            self.enqueued('exclusive')
            self.blocked(disk, 'SIGKILL released a live RAM child\'s kernel lock')
        finally:
            try:
                os.kill(child_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.assertEqual(self.line(disk), 'entered disk')
        self.finish(disk)


    def test_four_nvme_jobs_share_and_fifth_waits(self):
        import gate_capacity
        gate_capacity.set_limit(self.queue, 4, time.time())
        heavy_hardware.set_ceiling(self.queue.directory, 4, 'private fixture capacity')
        workers = [self.child(name, 'nvme') for name in ('one','two','three','four')]
        for worker in workers:
            self.assertEqual(self.line(worker), 'entered disk')
        fifth = self.child('fifth', 'nvme')
        self.enqueued('fifth')
        self.blocked(fifth, 'five NVMe jobs exceeded the host bound')
        self.finish(workers[0])
        self.assertEqual(self.line(fifth), 'entered disk')
        for worker in workers[1:] + [fifth]:
            self.finish(worker)


    def test_orphan_child_and_exclusive_waiter_allow_three_nvme_peers_but_not_a_fifth(self):
        import gate_capacity
        gate_capacity.set_limit(self.queue, 4, time.time())
        heavy_hardware.set_ceiling(self.queue.directory, 4, 'private fixture capacity')
        first = self.child('orphan', 'nvme')
        self.assertEqual(self.line(first), 'entered disk')
        first.stdin.write(b'retain\n')
        child_pid = int(self.line(first).removeprefix('retained '))
        try:
            first.kill()
            first.wait(timeout=5)
            exclusive = self.child('exclusive-waiter', 'disk')
            self.enqueued('exclusive-waiter')
            peers = [self.child(name, 'nvme') for name in ('peer-a','peer-b','peer-c')]
            for peer in peers:
                self.assertEqual(self.line(peer), 'entered disk')
            self.assertEqual(self.occupancy(), 4)
            with self.queue.heavy.open('a') as independent:
                fcntl.flock(independent, fcntl.LOCK_SH)
                self.assertGreaterEqual(self.occupancy(), 5)
            # A raw table undercount must still combine current holders with
            # the corroborated orphan observed outside the metadata mutex.
            (self.root/'underreported-locks').touch()
            fifth = self.child('fifth', 'nvme')
            self.enqueued('fifth')
            self.assertEqual(self.line(fifth), 'sampled undercount')
            self.blocked(fifth, 'a reaped wrapper hid its live child slot')
            os.kill(child_pid, signal.SIGKILL)
            child_pid = None
            self.assertEqual(self.line(fifth), 'entered disk')
            for peer in peers + [fifth]:
                self.finish(peer)
            self.assertEqual(self.line(exclusive), 'entered disk')
            self.finish(exclusive)
        finally:
            if child_pid is not None:
                try: os.kill(child_pid, signal.SIGKILL)
                except ProcessLookupError: pass


    def test_four_slot_limit_keeps_the_three_ram_reservation_ceiling(self):
        import gate_capacity
        gate_capacity.set_limit(self.queue, 4, time.time())
        heavy_hardware.set_ceiling(self.queue.directory, 4, 'private fixture capacity')
        self.test_three_ram_holders_preserve_the_v1_ticket_wire_format()

    def test_unrelated_kernel_lock_churn_cannot_hide_the_retained_child(self):
        child, blocked = self.child, self.blocked
        churner = None

        def start(name, *args, **kwargs):
            nonlocal churner
            if name == 'fifth':
                churner = self.start_churn()
            return child(name, *args, **kwargs)

        def boundary(process, reason):
            try:
                blocked(process, reason)
            finally:
                self.finish(churner)

        with patch.object(self, 'child', side_effect=start), \
                patch.object(self, 'blocked', side_effect=boundary):
            self.test_orphan_child_and_exclusive_waiter_allow_three_nvme_peers_but_not_a_fifth()

    def test_available_capacity_progresses_while_unrelated_kernel_locks_churn(self):
        import gate_capacity
        gate_capacity.set_limit(self.queue, 4, time.time())
        heavy_hardware.set_ceiling(self.queue.directory, 4, 'private fixture capacity')
        churner = self.start_churn()
        workers = []
        for name in ['one', 'two', 'three', 'four']:
            worker = self.child(name, 'nvme')
            workers.append(worker)
            self.assertEqual(self.line(worker), 'entered disk')
        fifth = self.child('fifth', 'nvme')
        self.enqueued('fifth')
        self.blocked(fifth, 'continuous unrelated lock churn broke the four-slot bound')
        self.finish(workers[0])
        self.assertEqual(self.line(fifth), 'entered disk')
        self.assertIsNone(churner.poll(), 'churn must continue throughout the liveness witness')
        for worker in workers[1:] + [fifth, churner]:
            self.finish(worker)

    def test_waiting_three_slot_client_does_not_leave_the_fourth_slot_idle(self):
        import gate_capacity
        gate_capacity.set_limit(self.queue, 4, time.time())
        heavy_hardware.set_ceiling(self.queue.directory, 4, 'private fixture capacity')
        workers = [self.child(name, 'nvme') for name in ('one', 'two', 'three')]
        for worker in workers:
            self.assertEqual(self.line(worker), 'entered disk')
        legacy = self.child('older-three-slot-client', 'nvme', legacy='three')
        self.enqueued('older-three-slot-client')
        fourth = self.child('fourth', 'nvme')
        self.assertEqual(self.line(fourth), 'entered disk')
        self.finish(fourth)
        self.blocked(legacy, 'legacy client exceeded its own three-slot bound')
        self.finish(workers[0])
        self.assertEqual(self.line(legacy), 'entered disk')
        for worker in workers[1:] + [legacy]:
            self.finish(worker)



if __name__ == '__main__':
    unittest.main()
