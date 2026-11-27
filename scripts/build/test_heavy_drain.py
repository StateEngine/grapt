"""Private real-process witnesses for bounded exclusive fairness."""
import json
import os
import signal
import re
import shutil
import tempfile
import threading
import fcntl
from pathlib import Path
import subprocess
import sys
import time
import unittest
from unittest.mock import patch

import heavy
from heavy_drain import Drain
from fixture_environment import isolated_environment
import test_heavy_ram as fixtures


class ExclusiveDrainTests(unittest.TestCase):
    # Reuse the process fixtures without inheriting and rerunning their whole suite.
    setUp = fixtures.HeavyRamTests.setUp
    set_available = fixtures.HeavyRamTests.set_available
    cleanup = fixtures.HeavyRamTests.cleanup
    line = fixtures.HeavyRamTests.line
    enqueued = fixtures.HeavyRamTests.enqueued
    blocked = fixtures.HeavyRamTests.blocked
    finish = fixtures.HeavyRamTests.finish

    def child(self, *args, **kwargs):
        if not getattr(self, '_clock_started', False):
            # Simulated one-hour waits must stay positive after a fresh boot.
            # Only the ordering workers share this private clock origin.
            self._clock_started = True
            steady, ticks = time.monotonic, time.monotonic_ns
            self.enterContext(patch.object(time, 'monotonic', lambda: steady()+7200))
            self.enterContext(patch.object(time, 'monotonic_ns', lambda: ticks()+7200_000_000_000))
            prefix = ('import time\n_fixture_s=time.monotonic\n_fixture_ns=time.monotonic_ns\n'
                      'time.monotonic=lambda:_fixture_s()+7200\n'
                      'time.monotonic_ns=lambda:_fixture_ns()+7200_000_000_000\n')
            self.enterContext(patch.object(fixtures, 'WORKER', prefix+fixtures.WORKER))
        return fixtures.HeavyRamTests.child(self, *args, **kwargs)

    def age(self, owner, seconds=301):
        with self.queue.locked(time.monotonic() + 5):
            tickets, _ = self.queue.state()
            row = next(row for row in tickets if row['owner'] == owner)
            row['enqueued_ns'] = time.monotonic_ns() - int(seconds * 1_000_000_000)
            self.queue.save(tickets)
            self.queue.notify(tickets)

    def project(self, owner, seconds):
        with self.queue.locked(time.monotonic() + 5):
            tickets, _ = self.queue.state()
            row = next(row for row in tickets if row['owner'] == owner)
            Drain(self.queue.directory, tickets).declare(row, 1800, seconds)
            self.queue.notify(tickets)

    @staticmethod
    def worker_startup():
        # The worker's own import set, not a bare interpreter: a loaded box
        # starts the next fixture worker late, and a bound covering it must be
        # measured the same way the worker pays it.
        began = time.monotonic()
        subprocess.run([sys.executable, '-B', '-c',
                        'import heavy,heavy_runs,ram_targets,heavy_sampling,external_holders,heavy_queue,heavy_hardware'],
                       check=True, cwd=Path(heavy.__file__).parent)
        return max(.05, time.monotonic() - began)

    def test_aged_exclusive_drains_shared_work_but_class_zero_still_passes(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        self.project('first', 60)
        disk = self.child('exclusive', 'disk')
        self.enqueued('exclusive')
        self.age('exclusive')
        later = self.child('later')
        self.enqueued('later')
        self.blocked(later, 'a late shared job bypassed the aged exclusive')
        landing = self.child('landing', priority=0)
        self.assertEqual(self.line(landing), 'entered ram')
        self.finish(first)
        self.finish(landing)
        self.assertEqual(self.line(disk), 'entered disk')
        self.blocked(later, 'shared work overlapped the exclusive holder')
        self.finish(disk)
        self.assertEqual(self.line(later), 'entered ram')
        self.finish(later)
        events = [json.loads(line) for line in (self.queue.directory / 'exclusive-drain.jsonl').read_text().splitlines()]
        self.assertEqual([row['event'] for row in events], ['drain-start', 'drain-end'])
        self.assertEqual(events[1]['reason'], 'exclusive-admitted')
        self.assertGreater(events[1]['idle_slot_minutes'], 0)

    def test_long_projection_cannot_defer_an_exclusive_after_one_hour(self):
        first = self.child('long', projected=10801)
        self.assertEqual(self.line(first), 'entered ram')
        disk = self.child('exclusive', 'disk')
        self.enqueued('exclusive')
        self.age('exclusive')
        # An aged exclusive at the head admits no new shared holder, even
        # while a long projection still defers its drain.
        early = self.child('early', projected=60)
        self.enqueued('early')
        self.blocked(early, 'shared work passed an aged exclusive behind a long holder')
        with self.queue.locked(time.monotonic()+5):
            tickets, _ = self.queue.state()
            row = next(row for row in tickets if row['owner'] == 'exclusive')
            row['enqueued_ns'] = time.monotonic_ns() - 3601_000_000_000
            self.queue.save(tickets)
            self.queue.notify(tickets)
        later = self.child('later', projected=60)
        self.enqueued('later')
        self.blocked(later, 'a long projection postponed the hard wait bound')
        urgent = self.child('urgent', priority=0)
        self.assertEqual(self.line(urgent), 'entered ram')
        self.assertIsNone(first.poll(), 'forcing a drain killed a running holder')
        self.finish(first)
        self.finish(urgent)
        self.assertEqual(self.line(disk), 'entered disk')
        self.finish(disk)
        self.assertEqual(self.line(early), 'entered ram')
        self.assertEqual(self.line(later), 'entered ram')
        self.finish(early)
        self.finish(later)
        events = [json.loads(line) for line in (self.queue.directory/'exclusive-drain.jsonl').read_text().splitlines()]
        forced = next(row for row in events if row['event'] == 'forced-drain')
        self.assertGreaterEqual(forced['wait_seconds'], 3600)

    def test_young_exclusive_never_drains_before_five_minutes(self):
        first = self.child('first', projected=60)
        self.assertEqual(self.line(first), 'entered ram')
        disk = self.child('exclusive', 'disk')
        self.enqueued('exclusive')
        with self.queue.locked(time.monotonic()+5):
            tickets, _ = self.queue.state()
            row = next(row for row in tickets if row['owner'] == 'exclusive')
            row['enqueued_ns'] = time.monotonic_ns() - 240_000_000_000
            self.queue.save(tickets)
            self.queue.notify(tickets)
        later = self.child('later', projected=60)
        self.assertEqual(self.line(later), 'entered ram')
        self.assertFalse((self.queue.directory/'exclusive-drain.jsonl').exists())
        self.finish(first)
        self.finish(later)
        self.assertEqual(self.line(disk), 'entered disk')
        self.finish(disk)

    def test_aged_exclusive_admits_no_new_shared_work_behind_a_long_holder(self):
        first = self.child('long', projected=10801)
        self.assertEqual(self.line(first), 'entered ram')
        disk = self.child('exclusive', 'disk')
        self.enqueued('exclusive')
        # Before five minutes the exclusive is young: shared work still passes.
        early = self.child('early', projected=60)
        self.assertEqual(self.line(early), 'entered ram')
        self.age('exclusive')
        later = self.child('later', projected=60)
        self.enqueued('later')
        self.blocked(later, 'shared arrivals kept refilling the slot an aged exclusive waits for')
        urgent = self.child('urgent', priority=0)
        self.assertEqual(self.line(urgent), 'entered ram')
        self.finish(urgent)
        self.finish(early)
        self.blocked(later, 'shared work took the slot before the long holder finished')
        self.finish(first)
        self.assertEqual(self.line(disk), 'entered disk')
        self.blocked(later, 'shared work entered beside an exclusive holder')
        self.finish(disk)
        self.assertEqual(self.line(later), 'entered ram')
        self.finish(later)

    def test_cancelling_an_aged_exclusive_resumes_normal_shared_admission(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        self.project('first', 60)
        # A loaded box starts the 'later' fixture worker late; the exclusive's
        # slot deadline must outlive the steps between its enqueue and the
        # cancel, or it times out (exit 1) before the SIGTERM can land (130).
        startup = self.worker_startup()
        disk = self.child('exclusive', 'disk', wait=max(10.0, startup * 2 + 2.0))
        self.enqueued('exclusive')
        self.age('exclusive')
        later = self.child('later')
        self.enqueued('later')
        self.blocked(later, 'the drain did not block class one')
        disk.terminate()
        self.assertEqual(disk.wait(timeout=5), 130)
        self.assertEqual(self.line(later), 'entered ram')
        self.finish(first)
        self.finish(later)
        events = [json.loads(line) for line in (self.queue.directory / 'exclusive-drain.jsonl').read_text().splitlines()]
        self.assertEqual(events[-1]['reason'], 'ticket-removed')

    def test_exclusive_timeout_kills_the_process_group_and_refunds_original_queue_age(self):
        root = self.root
        runner = '''import sys
from pathlib import Path
import heavy
import heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host=hardware_sample
heavy.HEAVY_LOCK=Path(sys.argv[1])/'heavy'
heavy.QUEUE_PATH=Path(sys.argv[1])/'queue'
sys.argv=['heavy','--exclusive','--owner','bounded-work','--max-hold','0.5','--',sys.executable,'-c',sys.argv[2]]
try:raise SystemExit(heavy.main())
except heavy.ExclusiveHoldTimeout:raise SystemExit(124)
'''
        body = "import signal,time,subprocess,sys,heavy; signal.signal(signal.SIGTERM,signal.SIG_IGN); child=subprocess.Popen([sys.executable,'-c','import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(60)'],pass_fds=heavy.inherited_fds()); print('started '+str(child.pid),flush=True); time.sleep(60)"
        process = subprocess.Popen([sys.executable, '-B', '-c', runner, str(root), body],
                                   cwd=Path(heavy.__file__).parent,
                                   env=isolated_environment(), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        output, error = process.communicate(timeout=12)
        self.assertEqual(process.returncode, 124, error.decode())
        self.assertIn(b'started', output)
        grandchild = int(output.decode().split('started ')[1].splitlines()[0])
        self.assertIsNone(heavy.identity(grandchild), 'a timed-out grandchild survived the group kill')
        with self.queue.heavy.open('a') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with self.queue.locked(time.monotonic()+5):
            tickets, _ = self.queue.state()
            self.assertFalse(tickets)
            drain = Drain(self.queue.directory, tickets)
            refund = drain.state['refunds'][0]
        ticket, notification = self.queue.enqueue(1, 'bounded-work', time.monotonic()+5,
                                                 max_hold=1800, refund=refund['ticket'])
        try:
            self.assertEqual(ticket['enqueued_ns'], refund['enqueued_ns'])
        finally:
            notification.close()
            self.queue.remove(ticket)
        with self.queue.locked(time.monotonic()+5):
            self.assertEqual(Drain(self.queue.directory, self.queue.state()[0]).state['refunds'], [])

    def test_parent_enforces_a_child_exclusive_conversion_even_if_the_child_ignores_its_alarm(self):
        runner = r"""import sys
from pathlib import Path
import heavy, heavy_runs
import heavy_sampling
from fixture_environment import hardware_sample
heavy_sampling.host=hardware_sample
root=Path(sys.argv[1]); target=root/'nvme'; target.mkdir()
heavy.HEAVY_LOCK=root/'heavy'; heavy.QUEUE_PATH=root/'queue'
heavy.nvme_target=heavy_runs.nvme_target=lambda _:target
body="import signal,time,heavy,heavy_runs; from pathlib import Path; heavy.nvme_target=heavy_runs.nvme_target=lambda p:Path(p); heavy.require_exclusive(); signal.signal(signal.SIGALRM,signal.SIG_IGN); print('exclusive',flush=True); time.sleep(60)"
body="import heavy_sampling;from fixture_environment import hardware_sample;heavy_sampling.host=hardware_sample;"+body
sys.argv=['heavy','--nvme','--owner','converted-work','--max-hold','0.2','--',sys.executable,'-c',body]
try:raise SystemExit(heavy.main())
except heavy.ExclusiveHoldTimeout:raise SystemExit(124)
"""
        result = subprocess.run([sys.executable, '-B', '-c', runner, str(self.root)],
                                cwd=Path(heavy.__file__).parent, env=isolated_environment(),
                                capture_output=True, timeout=12)
        self.assertEqual(result.returncode, 124, result.stderr.decode())
        self.assertIn(b'exclusive', result.stdout)
        with self.queue.heavy.open('a') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with self.queue.locked(time.monotonic()+5):
            tickets, _ = self.queue.state()
            self.assertEqual(Drain(self.queue.directory, tickets).state['refunds'][0]['owner'], 'converted-work')

    def test_landing_transport_contains_a_self_contained_scheduler(self):
        source = Path(heavy.__file__).parent
        script = (source.parent / 'land.ps1').read_text()
        row = re.search(r"\$helpers = @\(([^\n]+)\)", script)
        self.assertIsNotNone(row)
        names = re.findall(r"'([^']+)'", row[1])
        with tempfile.TemporaryDirectory(prefix='secode-landing-transport-') as temporary:
            target = Path(temporary)
            for name in names:
                origin=source.parent/'release'/name if name=='windows_version_worker.py' else source/name
                shutil.copyfile(origin, target/name)
            result = subprocess.run([sys.executable, '-B', '-c', 'import landing,heavy; assert heavy.Queue'],
                                    cwd=target, env=dict(isolated_environment(),PYTHONPATH=''), capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr.decode())

    def test_drain_expiry_reopens_no_shared_window_ahead_of_an_aged_exclusive(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        self.project('first', 60)
        disk = self.child('exclusive', 'disk')
        self.enqueued('exclusive')
        self.age('exclusive', 3601)
        later = self.child('later')
        self.enqueued('later')
        self.blocked(later, 'drain was not active')
        for index in range(3):
            urgent = self.child('urgent-' + str(index), priority=0)
            self.assertEqual(self.line(urgent), 'entered ram')
            self.finish(urgent)
        urgent = self.child('last-urgent', priority=0)
        self.assertEqual(self.line(urgent), 'entered ram')
        with self.queue.locked(time.monotonic()+5):
            tickets, _ = self.queue.state()
            policy = Drain(self.queue.directory, tickets)
            policy.state['active']['since'] = time.monotonic() - 2701
            policy.save()
            self.queue.notify(tickets)
        # The expired drain's shared window must not let new work refill the slot.
        self.blocked(later, 'drain expiry admitted shared work ahead of the aged exclusive')
        self.finish(first)
        self.finish(urgent)
        self.assertEqual(self.line(disk), 'entered disk')
        self.finish(disk)
        self.assertEqual(self.line(later), 'entered ram')
        self.finish(later)
        events = [json.loads(line) for line in (self.queue.directory / 'exclusive-drain.jsonl').read_text().splitlines()]
        expired = next(row for row in events if row.get('reason') == 'drain-time-limit')
        self.assertLessEqual(expired['seconds'], 2700)
        self.assertLessEqual(expired['idle_slot_minutes'], 2700 * 4 / 60)

    def test_consecutive_aged_exclusives_run_in_queue_order_ahead_of_later_shared_work(self):
        first = self.child('first')
        self.assertEqual(self.line(first), 'entered ram')
        self.project('first', 60)
        one = self.child('exclusive-one', 'disk')
        self.enqueued('exclusive-one')
        self.age('exclusive-one')
        two = self.child('exclusive-two', 'disk')
        self.enqueued('exclusive-two')
        self.age('exclusive-two')
        self.finish(first)
        self.assertEqual(self.line(one), 'entered disk')
        shared = self.child('shared')
        self.enqueued('shared')
        self.finish(one)
        # The older aged exclusive keeps its place ahead of later shared work.
        self.assertEqual(self.line(two), 'entered disk')
        self.blocked(shared, 'later shared work passed an older aged exclusive')
        self.finish(two)
        self.assertEqual(self.line(shared), 'entered ram')
        self.finish(shared)

    def test_gate_and_landing_commands_join_children_before_an_exclusive_timeout_unlocks(self):
        import gate
        for launch in ('gate', 'landing'):
            with self.subTest(launch=launch):
                marker = self.root / (launch + '.pid')
                command = [sys.executable, '-c',
                           "import os,signal,time; from pathlib import Path; Path(" + repr(str(marker)) + ").write_text(str(os.getpid())); signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(60)"]
                with self.assertRaises(heavy.ExclusiveHoldTimeout):
                    with heavy.slot('gate', 5, queue_path=self.queue.directory, heavy_path=self.queue.heavy,
                                    job_owner=launch, max_hold=.5):
                        if launch == 'gate':
                            with (self.root / 'gate-output.log').open('w') as log:
                                gate.execute(command, log, time.monotonic()+30)
                        else:
                            heavy.run(command, check=True, timeout=30)
                pid = int(marker.read_text())
                self.assertIsNone(heavy.identity(pid), 'owned command survived the lease timeout')
                with self.queue.heavy.open('a') as handle:
                    fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_hold_expiry_during_normal_exit_cleanup_joins_descendant_before_unlock(self):
        self.cleanup_crosses_hold_deadline('normal-exit')

    def test_hold_expiry_during_gate_timeout_cleanup_joins_descendant_before_unlock(self):
        self.cleanup_crosses_hold_deadline('gate-timeout')

    @staticmethod
    def pid_file_ready(path, seconds):
        # The marker does not exist, and may exist empty, until the writer's
        # close lands; poll with a bound instead of assuming it is ready.
        deadline = time.monotonic() + seconds
        while True:
            try:
                if path.read_text().strip():
                    return
            except FileNotFoundError:
                pass
            if time.monotonic() >= deadline:
                raise AssertionError('pid marker did not land: ' + str(path))
            time.sleep(.01)

    def interpreter_startup(self):
        # Measure the fixture pair the test actually spawns (leader plus
        # descendant) instead of a bare interpreter: a loaded box starts the
        # pair several times later than 'python -c pass', and the hold must
        # outlive the descendant's boot or its marker never lands before the
        # cleanup KILL. Three samples, worst case.
        probe = self.root / 'startup-probe.pid'
        child = ("import os; "
                 "fd=os.open(" + repr(str(probe)) + ",os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o644); "
                 "os.write(fd,str(os.getpid()).encode()); os.close(fd)")
        leader = ("import os,subprocess,sys; "
                  "subprocess.Popen([sys.executable,'-c'," + repr(child) + "])")
        worst = 0.0
        for _ in range(3):
            probe.unlink(missing_ok=True)
            began = time.monotonic()
            process = subprocess.Popen([sys.executable, '-c', leader], start_new_session=True)
            try:
                self.pid_file_ready(probe, 30.0)
                worst = max(worst, time.monotonic() - began)
            finally:
                heavy.stop_group(process)
        return max(.05, worst)

    def test_slow_startup_sample_keeps_normal_exit_hold_expiry_in_cleanup(self):
        with patch.object(type(self), 'interpreter_startup', return_value=.5, create=True):
            self.cleanup_crosses_hold_deadline('normal-exit')

    def test_slow_startup_sample_keeps_gate_timeout_hold_expiry_in_cleanup(self):
        with patch.object(type(self), 'interpreter_startup', return_value=.5, create=True):
            self.cleanup_crosses_hold_deadline('gate-timeout')

    def cleanup_crosses_hold_deadline(self, mode):
        import gate
        marker = self.root / (mode + '.pid')
        # A loaded box starts each fixture interpreter later; size the hold,
        # grace and run bounds from a measured startup so the descendant's
        # marker lands before the cleanup KILL without weakening the expiry.
        # The gate bound stays below the hold, so the gate times out first and
        # the expiry lands in its cleanup; the run bound stays above it, so the
        # normal exit completes first.
        startup = self.interpreter_startup()
        hold = max(.4, startup * 6)
        grace = hold * 2
        # The descendant writes its marker with os alone, before any further
        # import, so it is findable before the cleanup KILL even when late.
        child = ("import os; "
                 "fd=os.open(" + repr(str(marker)) + ",os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o644); "
                 "os.write(fd,str(os.getpid()).encode()); os.close(fd); "
                 "import signal,time; "
                 "signal.signal(signal.SIGTERM,signal.SIG_IGN); time.sleep(60)")
        leader = ("import os,subprocess,sys,time; "
                  "subprocess.Popen([sys.executable,'-c'," + repr(child) + "])\n"
                  "while not os.path.exists(" + repr(str(marker)) + "): time.sleep(.005)\n" +
                  ("time.sleep(60)" if mode == 'gate-timeout' else ""))
        try:
            with patch.object(heavy, 'STOP_GRACE_SECONDS', grace):
                with self.assertRaises(heavy.ExclusiveHoldTimeout):
                    with heavy.slot('gate', 5, queue_path=self.queue.directory,
                                    heavy_path=self.queue.heavy, job_owner=mode,
                                    max_hold=hold):
                        command = [sys.executable, '-c', leader]
                        if mode == 'gate-timeout':
                            with (self.root / 'timeout.log').open('w') as log:
                                gate.execute(command, log, time.monotonic()+max(.3, startup*5))
                        else:
                            heavy.run(command, timeout=max(5.0, startup * 8))
            self.pid_file_ready(marker, grace)
            pid = int(marker.read_text())
            self.assertIsNone(heavy.identity(pid), 'descendant survived expiry during cleanup')
            with self.queue.heavy.open('a') as handle:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.queue.locked(time.monotonic()+5):
                tickets, _ = self.queue.state()
                self.assertFalse(tickets)
                refunds = Drain(self.queue.directory, tickets).state['refunds']
                self.assertEqual([row['owner'] for row in refunds], [mode])
        finally:
            # A red witness must not leave its deliberately ignoring descendant alive.
            if marker.exists():
                try:
                    pid = int(marker.read_text())
                except ValueError:
                    pid = None
                if pid is not None and heavy.identity(pid) is not None:
                    os.kill(pid, signal.SIGKILL)

    def test_cleanup_waits_for_sigkill_delivery_to_orphaned_descendant(self):
        marker = self.root / 'delayed-kill.pid'
        child = ("import os,signal,time; from pathlib import Path; "
                 "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                 "Path(" + repr(str(marker)) + ").write_text(str(os.getpid())); time.sleep(60)")
        leader = ("import subprocess,sys,time; from pathlib import Path; "
                  "subprocess.Popen([sys.executable,'-c'," + repr(child) + "]); "
                  "marker=Path(" + repr(str(marker)) + ")\n"
                  "while not marker.exists(): time.sleep(.005)\n")
        original = os.killpg
        pending = []
        def delayed(pgid, sig):
            if sig != signal.SIGKILL:
                return original(pgid, sig)
            timer = threading.Timer(.03, original, args=(pgid, sig))
            pending.append(timer)
            timer.start()
        try:
            with patch.object(heavy, 'STOP_GRACE_SECONDS', .2), patch.object(os, 'killpg', delayed):
                heavy.run([sys.executable, '-c', leader], timeout=5)
            self.assertIsNone(heavy.identity(int(marker.read_text())),
                              'cleanup returned before SIGKILL reached the descendant')
        finally:
            for timer in pending:
                timer.join(timeout=1)
            if marker.exists() and heavy.identity(int(marker.read_text())) is not None:
                os.kill(int(marker.read_text()), signal.SIGKILL)

    def test_invalid_declarations_and_foreign_or_live_refunds_are_refused(self):
        with self.queue.locked(time.monotonic()+5):
            drain = Drain(self.queue.directory, [])
            ticket = {'id':'1-1', 'owner':'original', 'class':1, 'enqueued_ns':1}
            for maximum, projected in [(0,60),(1801,60),(1800,float('inf'))]:
                with self.assertRaises(ValueError):drain.declare(ticket, maximum, projected)
            drain.refund(ticket)
            for owner in ['foreign']:
                with self.assertRaises(ValueError):drain.redeem('1-1', {**ticket,'id':'2-2','owner':owner})
            drain.live.add('1-1')
            with self.assertRaises(ValueError):drain.redeem('1-1', {**ticket,'id':'2-2'})
