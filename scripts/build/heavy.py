#!/usr/bin/env python3
"""Run a heavy job in the host's fair queue, or show its holder and waiters."""
import argparse
from contextlib import contextmanager, nullcontext
import errno
import fcntl
import json
import math
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import time

from git_environment import apply as apply_git_environment
apply_git_environment()

from heavy_queue import Queue, identity
from heavy_runs import Placements, nvme_target
import full_capacity

HEAVY_LOCK = Path.home()/'secode-heavy.lock'
DEFAULT_QUEUE_PATH = Path.home()/'.local/state/secode/heavy'
QUEUE_PATH = DEFAULT_QUEUE_PATH
LOCK_TIMEOUT = 75
CLAIM = 'SECODE_HEAVY_LEASE'


class ExclusiveHoldTimeout(RuntimeError):
    pass


_owned_depth = 0
_hold_pending = False


def hold_expired(_signal, _frame):
    global _hold_pending
    if _owned_depth:
        # Owned waits poll this flag. Never unwind between spawning a child and
        # installing its cleanup, or while stopping its descendants.
        _hold_pending = True
        return
    raise ExclusiveHoldTimeout('Exclusive heavy hold expired; retry credit was preserved')


def check_hold_timeout():
    global _hold_pending
    if _hold_pending:
        _hold_pending = False
        raise ExclusiveHoldTimeout('Exclusive heavy hold expired; retry credit was preserved')


def arm_exclusive(queue, ticket):
    budget = queue.exclusive_budget(ticket, json.loads(os.environ[CLAIM])['fd'])
    if budget is not None:
        since, maximum = budget
        signal.signal(signal.SIGALRM, hold_expired)
        signal.setitimer(signal.ITIMER_REAL, max(0.001, maximum - (time.monotonic() - since)))


STOP_GRACE_SECONDS = 5


class UnreapedProcess(subprocess.Popen):
    """Observe command status while retaining its PID until group cleanup."""
    def __init__(self, *args, **kwargs):
        self.group_pid = None
        self.reaped = False
        self._retain_exit = False
        super().__init__(*args, **kwargs)
        self.group_pid = self.pid
        self._retain_exit = True

    def poll(self):
        if not self._retain_exit:
            return super().poll()
        with self._waitpid_lock:
            if self.returncode is None:
                try:
                    result = os.waitid(os.P_PID, self.group_pid or self.pid,
                                       os.WEXITED | os.WNOHANG | os.WNOWAIT)
                except ChildProcessError as error:
                    self.reaped = True
                    raise RuntimeError('Owned child was reaped outside group cleanup') from error
                if result is not None:
                    self.returncode = (result.si_status if result.si_code == os.CLD_EXITED
                                       else -result.si_status)
            return self.returncode

    def _wait(self, timeout):
        # Popen.wait and communicate's interrupt path both use this hook.
        if not self._retain_exit:
            return super()._wait(timeout)
        deadline = None if timeout is None else time.monotonic() + timeout
        while self.poll() is None:
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0:
                raise subprocess.TimeoutExpired(self.args, timeout)
            time.sleep(.01 if remaining is None else min(.01, remaining))
        return self.returncode

    def reap(self, timeout):
        self.wait(timeout=timeout)
        with self._waitpid_lock:
            if not self.reaped:
                pid, status = os.waitpid(self.group_pid, os.WNOHANG)
                if pid != self.group_pid:
                    raise RuntimeError('Observed child exit was unavailable for reaping')
                self.returncode = os.waitstatus_to_exitcode(status)
                self.reaped = True
        return self.returncode


@contextmanager
def cleanup_signals():
    """Deliver pending termination only after bounded stop-and-join completes."""
    previous = signal.pthread_sigmask(signal.SIG_BLOCK,
                                      {signal.SIGALRM, signal.SIGTERM, signal.SIGINT})
    try:
        yield
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)


def stop_group(process):
    """Stop the group recorded at spawn, even after its leader is reaped.

    Stopping is decided from the group id, never from whether the leader is
    still unreaped: reaping the leader clears its ownership record while a
    descendant can still be running, and returning early there left that
    descendant alive holding the heavy lease.

    The group id is reserved by the kernel only while one of its members lives.
    Once the group is empty the number can be reused, and a late `killpg` would
    then signal an unrelated group. Two facts bound that window: this runs right
    after the caller's wait, never deferred, and `_stop_group` treats ESRCH as
    "already gone", so an empty group ends the stop rather than signalling a
    stranger. That residual window is the one systemd avoids with cgroups;
    `durable_jobs` runs its jobs under `KillMode=control-group`, so the cgroup is
    the backstop there and this is the fallback for a process the units do not own.
    """
    with cleanup_signals():
        owned = isinstance(process, UnreapedProcess)
        # Without a spawn-time group id the only ownership evidence is the
        # unreaped handle, so an already-reaped foreign child cannot be stopped.
        if not owned and process.returncode is not None:
            if live_group(process.pid):
                raise RuntimeError('Reaped process group has no retained ownership')
            return
        try:
            _stop_group(process.group_pid if owned else process.pid)
        finally:
            if owned:
                process.reap(STOP_GRACE_SECONDS)
            else:
                process.wait(timeout=STOP_GRACE_SECONDS)


def live_group(pgid, proc=Path('/proc')):
    """Zombies cannot execute or hold leases; a live descendant still can."""
    for index, entry in enumerate(proc.iterdir()):
        if index >= 65536:
            raise ValueError('Process group inventory exceeds its bound')
        if not entry.name.isdigit():
            continue
        try:
            with (entry / 'stat').open('rb') as stream:
                raw = stream.read(8193)
        except (FileNotFoundError, ProcessLookupError):
            continue
        if len(raw) > 8192:
            raise ValueError('Process group identity exceeds its bound')
        fields = raw.rsplit(b')', 1)[1].split()
        if int(fields[2]) == pgid and fields[0] not in (b'Z', b'X'):
            return True
    return False


def _stop_group(group_pid):
    try:
        os.killpg(group_pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + STOP_GRACE_SECONDS
    while live_group(group_pid):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            try:
                os.killpg(group_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
        time.sleep(min(0.05, remaining))
    # SIGKILL delivery is asynchronous, and waiting for the leader does not join
    # an orphaned descendant. Keep the lease until no group member can execute.
    deadline = time.monotonic() + STOP_GRACE_SECONDS
    while live_group(group_pid):
        if time.monotonic() >= deadline:
            raise RuntimeError('Owned process group did not stop after SIGKILL')
        time.sleep(0.01)


@contextmanager
def spawn_signals():
    """Defer cancellation until the caller has recorded the new process handle."""
    previous = {}
    pending = []

    def deferred(number, frame):
        if not pending:
            pending.append((number, frame))

    try:
        for number in (signal.SIGTERM, signal.SIGINT):
            handler = signal.getsignal(number)
            if handler != signal.SIG_IGN:
                previous[number] = handler
                signal.signal(number, deferred)
        yield
    finally:
        # Block only the handoff, never the spawn: a signal arriving between
        # two restorations would otherwise leave the other on `deferred`.
        mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
        try:
            for number, handler in previous.items():
                signal.signal(number, handler)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, mask)
        if pending:
            number, frame = pending[0]
            handler = previous[number]
            if callable(handler):
                handler(number, frame)
            else:
                raise KeyboardInterrupt


@contextmanager
def owned_process(command, **options):
    """Keep hold expiry deferred from spawn through the final group join."""
    global _owned_depth
    process = None
    entered = False
    try:
        if _owned_depth >= 16:
            raise RuntimeError('Owned process nesting exceeds its bound')
        _owned_depth += 1
        entered = True
        if QUEUE_PATH == DEFAULT_QUEUE_PATH and Path(command[0]).name != 'git':
            import build_environment
            environment = build_environment.for_checkout(options.get('cwd') or Path.cwd(),
                                                          options.get('env', os.environ))
            if environment is not None:
                options['env'] = environment
        # Defer in the handler rather than masking spawn: exec children must not
        # inherit blocked termination signals.
        import durable_jobs
        invocation = durable_jobs.fenced(command) if QUEUE_PATH == DEFAULT_QUEUE_PATH else command
        with spawn_signals():
            process = UnreapedProcess(invocation, start_new_session=True, **options)
        check_hold_timeout()
        yield process
    finally:
        try:
            if process is not None:
                stop_group(process)
        finally:
            if entered:
                _owned_depth -= 1
        check_hold_timeout()


def wait_owned(process, timeout):
    deadline = time.monotonic() + timeout
    while True:
        check_hold_timeout()
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(process.args, timeout)
        try:
            return process.wait(timeout=min(1, remaining))
        except subprocess.TimeoutExpired:
            pass


def communicate_owned(process, input, timeout):
    deadline = time.monotonic() + timeout
    while True:
        check_hold_timeout()
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(process.args, timeout)
        try:
            return process.communicate(input, timeout=min(1, remaining))
        except subprocess.TimeoutExpired:
            # communicate retains partial input/output across timeout retries.
            input = None


def run(command, *, timeout, input=None, capture_output=False, check=False, **options):
    """Run one owned group, joining it before any exception returns its lease."""
    if type(timeout) not in (int, float) or not math.isfinite(timeout) or not 0 < timeout <= 43200:
        raise ValueError('Owned commands require a finite timeout in (0, 43200]')
    if input is not None:
        options['stdin'] = subprocess.PIPE
    if capture_output:
        options.update(stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    options.setdefault('pass_fds', inherited_fds())
    with owned_process(command, **options) as process:
        stdout, stderr = communicate_owned(process, input, timeout)
        result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
        if check:
            result.check_returncode()
        return result


def check_output(command, **options):
    return run(command, stdout=subprocess.PIPE, check=True, **options).stdout


def owner():
    return os.environ.get('STATEENGINE_AGENT_ID') or f'uid-{os.getuid()}'


def ram_compatible(root):
    try:
        with (root / 'scripts/build/ram_targets.py').open('rb') as source:
            raw = source.read(32769)
        return len(raw) <= 32768 and b'HEAVY_RAM_PROTOCOL = 1\n' in raw
    except FileNotFoundError:
        return False


@contextmanager
def slot(lane, wait=10800, *, heavy_path=None, queue_path=None, job_owner=None, ram=False, nvme=None, outcome=None, max_hold=1800, projected=None, refund=None, gate_log=None):
    if lane not in ('gate', 'landing') or not math.isfinite(wait) or not 0 < wait <= 43200:
        raise ValueError('Use gate or landing and a wait in (0, 43200] seconds')
    deadline = time.monotonic() + wait
    queue = Queue(queue_path or QUEUE_PATH, heavy_path or HEAVY_LOCK)
    jobs = min(int(os.environ.get('CARGO_BUILD_JOBS', '3')), 3)
    if jobs <= 0:
        raise ValueError('CARGO_BUILD_JOBS must be positive')
    ticket, notification = queue.enqueue(0 if lane == 'landing' else 1, job_owner or owner(), deadline, ram=ram, nvme=nvme, max_hold=max_hold, projected=projected, refund=refund, jobs=jobs, gate_log=gate_log)
    reservation = None
    previous_alarm = signal.getsignal(signal.SIGALRM)
    previous_timer = signal.getitimer(signal.ITIMER_REAL)
    timer_saved = time.monotonic()
    previous = os.environ.get(CLAIM)
    previous_jobs = os.environ.get('CARGO_BUILD_JOBS')
    result = outcome if outcome is not None else {}
    admitted = False
    waiting_timeout = False

    def reserve():
        nonlocal reservation
        import ram_targets
        reservation = ram_targets.allocate(Path.cwd())
        return reservation

    try:
        jobs = int(previous_jobs or '3')
        if jobs <= 0:
            raise ValueError('CARGO_BUILD_JOBS must be positive')
        os.environ['CARGO_BUILD_JOBS'] = str(min(jobs, 3))
        with queue.heavy.open('a') as handle:
            try:
                mode = queue.acquire(ticket, notification, handle, deadline, wait, reserve if ram else None)
                admitted = True
                os.environ[CLAIM] = json.dumps({
                    'queue': str(queue.directory.resolve()), 'heavy': str(queue.heavy.resolve()),
                    'ticket': ticket['id'], 'pid': ticket['pid'], 'start': ticket['start'],
                    'fd': handle.fileno(), 'notify': notification.fileno(), 'wait': wait,
                    'ram': str(reservation[0]) if reservation else None,
                    'ram_row': reservation[1] if reservation else None,
                    'force_disk': ram and mode == 'disk'})
                arm_exclusive(queue, ticket)
                yield handle
                result.setdefault('exit', 0)
            except BaseException as error:
                waiting_timeout = not admitted and isinstance(error, TimeoutError)
                if isinstance(error, ExclusiveHoldTimeout):
                    signal.setitimer(signal.ITIMER_REAL, 0)
                    queue.refund(ticket)
                    print(f'Exclusive hold timed out; refund ticket {ticket["id"]}', file=sys.stderr)
                    result['exit'] = 124
                result.setdefault('exit', 130 if isinstance(error, KeyboardInterrupt) else 1)
                raise
            finally:
                cleanup_failed = False
                try:
                    cleanup = reservation
                    if cleanup is not None:
                        import ram_targets
                        try:
                            cleanup[0].lstat()
                        except FileNotFoundError:
                            pass  # A governed RAM failure already joined and removed its scope.
                        else:
                            if (cleanup[0].parent != ram_targets.ROOT or cleanup[0].is_symlink()
                                    or ram_targets.read_run(cleanup[0]) != cleanup[1]):
                                raise ValueError('Scheduler RAM cleanup owner changed; preserve target')
                            ram_targets.remove(*cleanup)
                except BaseException:
                    cleanup_failed = True
                    result['exit'] = 1
                    raise
                finally:
                    if not cleanup_failed:
                        fcntl.flock(handle, fcntl.LOCK_UN)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous_alarm)
        if previous_timer[0]:
            signal.setitimer(signal.ITIMER_REAL, max(0.001, previous_timer[0] - (time.monotonic() - timer_saved)), previous_timer[1])
        if previous is None:
            os.environ.pop(CLAIM, None)
        else:
            os.environ[CLAIM] = previous
        if previous_jobs is None:
            os.environ.pop('CARGO_BUILD_JOBS', None)
        else:
            os.environ['CARGO_BUILD_JOBS'] = previous_jobs
        notification.close()
        try:
            if waiting_timeout:
                if queue.remove(ticket, result.get('exit'), refund_pending=True):
                    print(f'Wait expired without admission; retry with --owner {ticket["owner"]} '
                          f'--refund {ticket["id"]} to retain queue age', file=sys.stderr)
            else:
                queue.remove(ticket, result.get('exit'))
            result['queue_cleanup'] = 'completed'
        except TimeoutError as error:
            if not admitted:
                raise
            result['queue_cleanup'] = 'deferred'
            print(f'Heavy queue finalization deferred for {ticket["id"]}: {error}; '
                  f'child exit={result.get("exit")}; dead ticket will be reaped after wrapper exit',
                  file=sys.stderr, flush=True)


def inherited():
    raw = os.environ.get(CLAIM)
    if raw is None:
        return None
    if len(raw) > 8192:
        raise ValueError('Heavy lease claim exceeds its bound')
    claim = json.loads(raw)
    if (not isinstance(claim, dict)
            or set(claim) != {'queue', 'heavy', 'ticket', 'pid', 'start', 'fd', 'notify', 'wait',
                              'ram', 'ram_row', 'force_disk'}
            or any(type(claim[k]) is not int or claim[k] < 0 for k in ('fd', 'notify', 'pid'))
            or type(claim['wait']) not in (int, float) or not math.isfinite(claim['wait'])
            or not 0 < claim['wait'] <= 43200 or type(claim['force_disk']) is not bool
            or any(not isinstance(claim[k], str) or not Path(claim[k]).is_absolute()
                   for k in ('queue', 'heavy'))):
        raise ValueError('Invalid inherited heavy lease')
    try:
        descriptor = os.fstat(claim['fd'])
        path = Path(claim['heavy']).stat()
        if ((descriptor.st_dev, descriptor.st_ino) != (path.st_dev, path.st_ino)
                or identity(claim['pid']) != claim['start']):
            raise ValueError('Inherited heavy lease owner or descriptor changed')
        duplicate = os.dup(claim['notify'])
        try:
            notification = socket.socket(fileno=duplicate)
        except BaseException:
            os.close(duplicate)
            raise
        with notification:
            address = '\0secode-heavy-' + claim['ticket']
            if notification.getsockname() not in (address, address.encode()):
                raise ValueError('Inherited heavy notification changed')
    except OSError as error:
        if error.errno not in (errno.EBADF, errno.ENOTSOCK):
            raise
        raise ValueError('Inherited heavy lease descriptors are unavailable; '
                         'requeue from a fresh landing helper') from None
    queue = Queue(claim['queue'], claim['heavy'])
    ticket, allocation = queue.lease(dict(id=claim['ticket'], pid=claim['pid'], start=claim['start']))
    return claim, queue, ticket, allocation


def inherited_fds():
    lease = inherited()
    return ((lease[0]['fd'], lease[0]['notify']) if lease else ()) + full_capacity.inherited_fds()


def reservation():
    lease = inherited()
    if lease is None or lease[3]['slot'] != 'ram' or lease[0]['ram'] is None:
        return None
    claim = lease[0]
    import ram_targets
    path = Path(claim['ram'])
    if (path.parent != ram_targets.ROOT or path.is_symlink()
            or ram_targets.read_run(path) != claim['ram_row']
            or ram_targets.identity(claim['pid']) != claim['ram_row']['owner']
            or lease[3]['path'] != str(path) or lease[3]['budget'] != claim['ram_row']['budget']):
        raise ValueError('Inherited RAM reservation changed')
    return path, claim['ram_row']


def shared():
    lease = inherited()
    return bool(lease and (lease[3]['slot'] == 'ram' or
                Placements(lease[1].directory, [lease[2]]).shared(lease[2])))


def disk_only():
    lease = inherited()
    # Nested gates inherit the placement that admission actually priced. RAM
    # requires the scheduler's reservation; an NVMe lease is not a RAM budget.
    return bool(lease and (lease[0]['force_disk'] or lease[3]['slot'] == 'disk'))


def require_disk(target):
    lease = inherited()
    if lease is None or lease[3]['slot'] != 'ram':
        return
    claim, queue, ticket, _ = lease
    with socket.socket(fileno=os.dup(claim['notify'])) as notification, \
            os.fdopen(os.dup(claim['fd']), 'a') as handle:
        queue.disk(ticket, notification, handle, claim['wait'], nvme=nvme_target(target))
    arm_exclusive(queue, ticket)


def require_exclusive():
    lease = inherited()
    if lease is None or not shared():
        return
    claim, queue, ticket, _ = lease
    with socket.socket(fileno=os.dup(claim['notify'])) as notification, \
            os.fdopen(os.dup(claim['fd']), 'a') as handle:
        queue.disk(ticket, notification, handle, claim['wait'])
    arm_exclusive(queue, ticket)


def note_target(mode):
    lease = inherited()
    if lease is not None:
        lease[1].target(lease[2], mode)


def status():
    import disk_stall
    queue = Queue(QUEUE_PATH, HEAVY_LOCK)
    tickets = queue.snapshot(time.monotonic() + 5, detailed=True)
    print(f'holder: {queue.holder(tickets)}')
    pending = sorted((t for t in tickets if not t['holder']), key=lambda t: t['position'])
    print(f'queue: {len(pending)}')
    from heavy_hardware import read, SANITY_LIMIT
    policy = read(queue.directory/'hardware-ceiling-v2.json',
                  {'limit': SANITY_LIMIT, 'reason': 'hardware-derived admission'})
    for position, ticket in enumerate(pending, 1):
        seconds = max(0, (time.monotonic_ns() - ticket['enqueued_ns']) / 1e9)
        print(f"{position}: class {ticket['class']} {ticket['owner']} pid {ticket['pid']} "
              f"queued {seconds:.1f} s (enqueue {ticket['enqueued_ns']}); "
              f"resources: {ticket['resource_refusal'] or 'fit'}; placement: {ticket['placement']}")
    print(f"hardware safety ceiling: {policy['limit']} ordinary + 1 reserved landing")
    print(f"configured ceiling note (recorded): {policy['reason']}")
    if pending:
        from heavy_hardware import PRESSURE_LIMITS, GIB
        hardware = pending[0]['hardware']
        pressure = hardware.get('pressure')
        if pressure is not None:
            print('live PSI avg10: ' + ', '.join(
                f'{key} {pressure[key]:.2f}% (limit {limit}%)' for key, limit in PRESSURE_LIMITS))
        print(f"live available RAM {hardware['available']/GIB:.2f} GiB; "
              f"NVMe free {hardware['nvme_free']/GIB:.2f} GiB; root free {hardware['root_free']/GIB:.2f} GiB")
    now = time.monotonic()
    stalled = disk_stall.current(QUEUE_PATH, now)
    if stalled is not None and now-stalled['updated'] <= 120 and now-stalled['since'] >= disk_stall.ALERT_SECONDS:
        print(disk_stall.message(stalled, now))
    for ticket in tickets:
        if ticket['holder']:
            print(f"running: {ticket['owner']} mode={ticket.get('mode', 'legacy-exclusive')} "
                  f"reservation={ticket.get('budget', 0)} hold={ticket.get('hold_seconds', 0):.1f}s "
                  f"concurrent={ticket.get('concurrent', False)}")


def history():
    queue = Queue(QUEUE_PATH, HEAVY_LOCK)
    with queue.locked(time.monotonic() + 5):
        _, runs = queue.state()
        for run in runs.recent[-16:]:
            print(f"{run['owner']} hold={run['hold_seconds']:.1f}s "
                  f"targets={'+'.join(run['targets']) or 'disk'} "
                  f"concurrent={run['concurrent']} exit={run['exit']}")


LIGHT_LOCK = Path.home()/'secode-light.lock'
LIGHT_WALL_SECONDS = 300


def _supervise_light(command, wall_seconds):
    """Retain the inherited lock and timer even if the submitting seat dies."""
    global STOP_GRACE_SECONDS
    # Finish descendant cleanup before the launcher's own five-second grace.
    STOP_GRACE_SECONDS = 1
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        with owned_process(['nice', '-n', '15', 'ionice', '-c', '3', 'prlimit',
                            '--as=1073741824', '--cpu=300', '--', *command]) as process:
            return wait_owned(process, wall_seconds)
    except subprocess.TimeoutExpired:
        return 124
    except KeyboardInterrupt:
        return 130


def run_light(command):
    """Bounded offline diagnostics bypass the build queue at idle priority."""
    if (Path(command[0]).name in {'cargo', 'rustc', 'rustdoc'} or
            any(Path(word).name in {'gate.sh', 'gate.py'} for word in command)):
        raise ValueError('--light is only for non-Cargo offline diagnostics')
    import heavy_sampling
    host = heavy_sampling.host()
    if host['available'] < 17*1024**3:
        raise TimeoutError('Light diagnostics require the memory reserve plus one GiB')
    with LIGHT_LOCK.open('a') as held:
        try:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise TimeoutError('Another light diagnostic is running; no build ticket was queued') from None
        # A separate process owns both the lock and bounded descendant cleanup.
        bootstrap = ('import sys;sys.path.insert(0,sys.argv.pop(1));import heavy;'
                     'sys.exit(heavy._supervise_light(sys.argv[2:],float(sys.argv[1])))')
        with owned_process([sys.executable, '-B', '-c', bootstrap,
                            str(Path(__file__).resolve().parent), str(LIGHT_WALL_SECONDS), *command],
                           pass_fds=(held.fileno(),)) as process:
            try:
                code = wait_owned(process, LIGHT_WALL_SECONDS+STOP_GRACE_SECONDS)
            except subprocess.TimeoutExpired:
                raise TimeoutError('Light diagnostics exceeded five minutes') from None
            if code == 124:
                raise TimeoutError('Light diagnostics exceeded five minutes')
            return code


@contextmanager
def cache_lease(target):
    """Hold the shared cache lease for this job's target while it runs.

    A gate takes this lease itself for its whole run, so the evictor already
    treats a gate's target as in use. A bare `cargo` job under this queue held
    only the queue lease, so the evictor had to infer liveness from /proc
    environments - the check that missed builds and let an in-use target look
    idle. Taking the same lease here makes "in use" a kernel fact for every
    queued job, whatever it runs.

    Only an *exclusive* holder refuses entry. A build or a survivor holding
    this lease is shared, so sharing it again always succeeds; a refused shared
    lease therefore means the evictor is deleting this target right now, and
    starting a build inside it would race the deletion the lease exists to
    order. That refusal fails the job with the contention named, and the queue
    can retry it. A target outside the managed cache, or a filesystem without
    working locks, is a different case: the other guards (recent use, ownership
    marker, /proc scan) still apply, so the job runs unleased rather than
    refusing over a capability this host never had.
    """
    if target is None:
        yield
        return
    try:
        import evict_targets
    except ImportError:
        yield
        return
    try:
        held = evict_targets.lease(Path(target), shared=True)
    except BlockingIOError:
        # An OSError subclass, so it must be named before the general fallback
        # below: this is contention, not a missing lock capability.
        raise RuntimeError(
            f'target {target} is leased exclusively by the evictor or another '
            'holder; refusing to start a build inside it') from None
    except (OSError, RuntimeError):
        yield
        return
    try:
        yield
    finally:
        held.close()


# The fleet's env (uutils coreutils 0.10.0, as installed on this box)
# runs in three stages, pinned here against the installed binary rather
# than the GNU docs; the guard mirrors each:
#
# 1. Pre-scan, before option parsing. While active, a word beginning
#    -vvS, -vS, -S or --split-string is spliced: the rest of the word
#    is its string (--split-string strips one leading '=', the short
#    forms keep it, so '-S=FOO' feeds the splitter the literal string
#    '=FOO', which only warns), or the next word for the bare forms.
#    The string is split on whitespace, single- and double-quote
#    aware; an unquoted backslash or an unterminated quote is env's
#    own error. The tokens replace the word in the list the options
#    phase sees, but the pre-scan never re-examines them; it keeps
#    scanning the original words that follow. A word beginning -u
#    (but not --) that contains '=' anywhere is refused before
#    anything runs, even past assignments and lone - markers. The
#    pre-scan stops at the first plain word (no leading dash, no '=')
#    or at --, unless that word is the separate value of a preceding
#    -a/-C/-f/-u or their long forms; everything after the stop passes
#    through untouched: no splice, no -u check.
# 2. Option parsing, leading options only. Flags: -i, -0, -v and
#    their longs, plus --list-signal-handling. -h/-V/--help/--version
#    print and exit 0 without running anything. --ignore-signal /
#    --default-signal / --block-signal consume exactly their own word
#    (bare, or =SIG): the following word is the program, and a =SIG
#    value must name a real signal (case-insensitive, optional SIG
#    prefix, or a number 1..64 on this box); ignore and default are
#    env errors for SIGKILL(9) and SIGSTOP(19), block is not. -C/-f/
#    -u/-a and their longs take an attached value (one leading '=' is
#    clap's separator) or a separate word that may not start with a
#    hyphen, then env checks the value: -C and -f need non-empty, -u
#    needs non-empty and no '=', -a accepts anything. Unknown options
#    are refused, and so is any abbreviated long option: the guard is
#    deliberately conservative where env's prefix inference is
#    permissive. Clap also refuses a second use of each Set argument
#    (the -i and -0 flags, --list-signal-handling, -C/--chdir, and the
#    cluster -S); -a overrides itself, and -u, -f and the signal
#    options may repeat. A lone - or -- ends the options, as does the
#    first assignment or program word. An S buried in a bigger cluster
#    (-iS, -viS) is an ordinary value-taking option: it consumes its
#    attached rest or the next word and never splits it; only a word
#    beginning -S was spliced in stage 1.
# 3. Data. Of the remaining words, a lone - anywhere before the
#    program only means "ignore the environment"; a word containing
#    '=' is an assignment, even when it looks like an option (stage 1
#    has stopped, so '-uFOO=2' after an assignment is an assignment,
#    not an unset); the first other word is the program. -0/--null
#    plus a program is env's own error; with no program env prints
#    the environment and exits 0.
#
# The guard therefore refuses what env is guaranteed to refuse before
# exec: stage 1/2 errors, help and version, the null-with-command
# rule, and the vacuous no-program jobs. Everything that may reach
# exec is a program word and a job: a program name the guard cannot
# see may be on the PATH, and a -C/-f path may exist; their failure
# modes are the job's own outcome.
_ENV_TERMINAL = ('-h', '-V', '--help', '--version')
_ENV_FLAGS = ('-i', '-0', '-v', '--ignore-environment', '--null',
              '--debug', '--list-signal-handling')
_ENV_SIGNALS = {'--ignore-signal': 'ignore',
                '--default-signal': 'default',
                '--block-signal': 'block'}
_ENV_LONG_VALUE = {'--chdir': 'C', '--file': 'f', '--unset': 'u',
                   '--argv0': 'a'}
_S_FAMILY = ('-vvS', '-vS', '-S', '--split-string')
_SIGNAL_BY_NAME = {}
for _signal_name in dir(signal.Signals):
    if _signal_name.startswith('SIG') and _signal_name[3:4].isupper():
        _signal_number = int(signal.Signals[_signal_name].value)
        _SIGNAL_BY_NAME[_signal_name] = _signal_number
        _signal_bare = _signal_name[3:]
        if _signal_bare and '_' not in _signal_bare:
            _SIGNAL_BY_NAME[_signal_bare] = _signal_number
del _signal_name, _signal_number, _signal_bare


def split_env_string(text):
    """The fleet env's -S split, or None when env would refuse the string.

    Whitespace-separated, single- and double-quote aware. A backslash
    escapes the next character inside double quotes; anywhere unquoted it
    is env's own error, as is an unterminated quote, so both refuse.
    """
    tokens, current = [], []
    quote, escaped = None, False
    for char in text:
        if escaped:
            current.append(char)
            escaped = False
        elif char == '\\' and quote == '"':
            escaped = True
        elif char == '\\':
            return None
        elif quote:
            if char == quote:
                quote = None
            else:
                current.append(char)
        elif char in ('"', "'"):
            quote = char
        elif char.isspace():
            if current:
                tokens.append(''.join(current))
                current = []
        else:
            current.append(char)
    if escaped or quote is not None:
        return None
    if current:
        tokens.append(''.join(current))
    return tokens


def _s_family_string(word):
    """The -S family's string, or False when the word is not one.

    None marks the bare form, whose string is the next word. The long
    form strips one leading '=' (clap's separator); the short forms
    keep it, so '-S=FOO' is the literal string '=FOO'.
    """
    for prefix in _S_FAMILY:
        if word == prefix:
            return None
        if word.startswith(prefix):
            rest = word[len(prefix):]
            if prefix == '--split-string' and rest.startswith('='):
                rest = rest[1:]
            return rest
    return False


def _attached(rest):
    """Clap strips one leading '=' from an attached C/f/u/a value."""
    return rest[1:] if rest.startswith('=') else rest


def _value_ok(letter, value):
    """env's post-parse check on a -C/-f/-u/-a value.

    -C and -f need non-empty (env fails to chdir or open); -u needs
    non-empty and no '=' (env's unset refuses both); -a accepts
    anything, empty included.
    """
    if letter == 'a':
        return True
    if letter == 'u':
        return value != '' and '=' not in value
    return value != ''


def _signal_value_ok(action, value):
    """env's check on a --ignore/default/block-signal=SIG value.

    Empty is the bare form's default. Names are case-insensitive with
    an optional SIG prefix; numbers run 1..64 (this box's real-time
    ceiling). ignore and default are env errors for SIGKILL and
    SIGSTOP; block is not.
    """
    if not value:
        return True
    if value.isdigit():
        number = int(value)
        if not 1 <= number <= 64:
            return False
    else:
        name = value.upper()
        number = _SIGNAL_BY_NAME.get(name)
        if number is None:
            number = _SIGNAL_BY_NAME.get('SIG' + name)
        if number is None:
            return False
    if action in ('ignore', 'default') and number in (9, 19):
        return False
    return True


def _separate_value(words, index, length):
    """The next word as an option value, or None when env would refuse.

    Clap refuses a value starting with a hyphen; a lone - is allowed
    and env then fails on its own terms, which is the job's outcome
    rather than the guard's.
    """
    if index + 1 >= length:
        return None, None
    value = words[index + 1]
    if value.startswith('-') and len(value) > 1:
        return None, None
    return index + 2, value


def _data_phase(words, null_seen):
    """Stage 3: markers, assignments, then the first program word."""
    for word in words:
        if word == '-' or '=' in word:
            continue
        return None if null_seen else word
    return None


def _option_phase(words):
    """Stages 2 and 3 over the pre-scan's output list.

    Mirrors clap as env drives it: leading options only; a lone -, --,
    or the first assignment or program word starts the data phase.
    Clap also refuses a second use of each Set argument (the -i and
    -0 flags, --list-signal-handling, -C/--chdir, and the cluster
    -S); -a overrides itself, and -u, -f and the signal options may
    repeat.
    """
    null_seen = False
    flags_seen = set()
    values_seen = set()

    def flag_seen(arg):
        if arg in flags_seen:
            return False
        flags_seen.add(arg)
        return True

    def value_seen(arg):
        if arg in values_seen:
            return False
        values_seen.add(arg)
        return True

    index = 0
    length = len(words)
    while index < length:
        word = words[index]
        if word == '--':
            return _data_phase(words[index + 1:], null_seen)
        if word == '-' or not (word.startswith('-') and len(word) > 1):
            return _data_phase(words[index:], null_seen)
        base, sep, attached = word.partition('=')
        if base in _ENV_TERMINAL:
            return None
        if base in _ENV_SIGNALS:
            if not _signal_value_ok(_ENV_SIGNALS[base], attached if sep else ''):
                return None
            index += 1
            continue
        if base in _ENV_LONG_VALUE:
            if base == '--chdir' and not value_seen('C'):
                return None
            if sep:
                value = attached
                index += 1
            else:
                index, value = _separate_value(words, index, length)
                if value is None:
                    return None
            if not _value_ok(_ENV_LONG_VALUE[base], value):
                return None
            continue
        if base in _ENV_FLAGS:
            if sep:
                return None
            argid = {'-i': 'i', '--ignore-environment': 'i',
                     '-0': '0', '--null': '0',
                     '--list-signal-handling': 'L'}.get(word)
            if argid is not None and not flag_seen(argid):
                return None
            if word in ('-0', '--null'):
                null_seen = True
            index += 1
            continue
        if len(word) == 2:
            letter = word[1]
            if letter not in 'aCfuS':
                return None
            if letter in 'CS' and not value_seen(letter):
                return None
            index, value = _separate_value(words, index, length)
            if value is None:
                return None
            if letter != 'S' and not _value_ok(letter, value):
                return None
            continue
        # A multi-letter short cluster, walked left to right.
        advance = 1
        letters = word[1:]
        position = 0
        while position < len(letters):
            letter = letters[position]
            rest = letters[position + 1:]
            if letter in 'iv':
                if letter == 'i' and not flag_seen('i'):
                    return None
                position += 1
            elif letter == '0':
                if not flag_seen('0'):
                    return None
                null_seen = True
                position += 1
            elif letter in 'hV':
                return None
            elif letter in 'aCfuS':
                if letter in 'CS' and not value_seen(letter):
                    return None
                if rest:
                    # An attached value ends the cluster. An S here is
                    # clap's swallowed -S (stage 1 spliced the real
                    # ones): the rest is consumed, never split.
                    if letter != 'S' and not _value_ok(letter, _attached(rest)):
                        return None
                else:
                    index, value = _separate_value(words, index, length)
                    if value is None:
                        return None
                    if letter != 'S' and not _value_ok(letter, value):
                        return None
                    # _separate_value already stepped past both words.
                    advance = 0
                break
            else:
                return None
        index += advance
    return _data_phase([], null_seen)


def _preparse(command):
    """Stage 1: the -S splices and the '-u' + '=' refusal.

    Returns the word list the options phase sees, or None when env
    would refuse before parsing options.
    """
    out = []
    expecting_value = False
    index = 0
    while index < len(command):
        word = command[index]
        if not expecting_value and (word == '--'
                                    or (not word.startswith('-')
                                        and '=' not in word)):
            out.extend(command[index:])
            return out
        expecting_value = False
        string = _s_family_string(word)
        if string is not False:
            if string is None:
                if index + 1 >= len(command):
                    out.append(word)
                    index += 1
                    continue
                string = command[index + 1]
                index += 2
            else:
                index += 1
            tokens = split_env_string(string)
            if tokens is None:
                return None
            out.extend(tokens)
            continue
        if word.startswith('-u') and not word.startswith('--') \
                and '=' in word:
            return None
        if word.startswith('--'):
            expecting_value = word[2:] in _ENV_LONG_VALUE
        elif len(word) > 1:
            expecting_value = word[-1] in 'aCfu'
        out.append(word)
        index += 1
    return out


def env_program(command):
    """The program word env would exec, or None when it would not.

    Mirrors the fleet's env in its three stages (the pinned grammar
    is in the comment above): a pre-scan that splices the -S family
    and refuses '-u' words carrying '=', an options phase of leading
    options only, and a data phase where a lone - means "ignore the
    environment", words with '=' are assignments, and the first other
    word is the program. None means env would print the environment
    or help and exit 0, or refuse before exec; a word means env would
    exec one, whether or not it exists.
    """
    pre = _preparse(command)
    if pre is None:
        return None
    return _option_phase(pre)

def main():
    if len(sys.argv) == 3 and sys.argv[1] == 'archive':
        import durable_jobs
        print(durable_jobs.archive(sys.argv[2]))
        return 0
    if sys.argv[1:] == ['status']:
        status()
        return 0
    if sys.argv[1:] == ['history']:
        history()
        return 0
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--wait', type=float, default=10800)
    parser.add_argument('--owner', default=owner())
    parser.add_argument('--max-hold', type=float, default=1800,
                        help='exclusive execution ceiling in seconds, at most 1800; timeout preserves retry credit')
    parser.add_argument('--projected-seconds', type=float,
                        help='shared duration estimate; default is the measured owner/kind or gate median')
    parser.add_argument('--refund', help='redeem a timed-out attempt ticket, preserving its owner, priority and enqueue age')
    parser.add_argument('--class', dest='priority', type=int, choices=(0, 1), default=1,
                        help='0 for bounded landing composition; 1 for all other work')
    placement = parser.add_mutually_exclusive_group(required=True)
    placement.add_argument('--light', action='store_true', help='offline non-Cargo diagnostics: idle I/O/CPU, one job, five minutes')
    placement.add_argument('--nvme', action='store_true', help='share verified NVMe targets within the monitored host limit')
    placement.add_argument('--ram', action='store_true', help='admit a governed scripts/gate.sh command to a RAM slot')
    placement.add_argument('--exclusive', action='store_true', help='explicitly drain shared work before exclusive execution')
    parser.add_argument('--hygiene', action='store_true',
                        help='run legacy disk-hygiene callers at maintenance priority')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('Supply a command after --')
    if Path(command[0]).name == 'env' and env_program(command[1:]) is None:
        parser.error('env names no program for this job: it would print the environment (or help or version) and exit 0, or refuse the form')
    if args.light:
        if args.nvme or args.ram or args.hygiene or args.refund or args.priority != 1:
            parser.error('--light does not take build placement, priority or refund options')
        return run_light(command)
    if args.hygiene:
        if args.owner != 'disk-hygiene':
            parser.error('--hygiene requires --owner disk-hygiene')
        args.priority = 0
    import durable_jobs
    if (QUEUE_PATH == DEFAULT_QUEUE_PATH and not durable_jobs.active()
            and inherited() is None and full_capacity.inherited() is None
            and not os.environ.get('SECODE_RAM_TARGET_RUN')):
        return durable_jobs.run([sys.executable, '-B', str(Path(__file__).resolve()), *sys.argv[1:]], args.owner)
    if args.ram:
        root = Path.cwd()
        if (len(command) < 2 or Path(command[0]).name != 'bash'
                or Path(command[1]).resolve() != root / 'scripts/gate.sh'
                or not ram_compatible(root)):
            parser.error('--ram requires a checkout with governed bash scripts/gate.sh RAM admission')
    target = None
    if args.nvme or args.ram:
        cache = Path(os.environ.get('SECODE_CACHE_DIR', str(Path.home()/'.cache/secode')))
        target = nvme_target(os.environ.get('CARGO_TARGET_DIR', str(cache/'target')))
        if args.nvme and target is None:
            parser.error('--nvme requires a target on the configured build filesystem')
    outcome = {}
    gate_log = None
    if '--log' in command and any(Path(word).name in ('gate.sh', 'gate.py') for word in command):
        index = command.index('--log')
        if index+1 < len(command):
            gate_log = Path(command[index+1]).resolve()
    gate_index = next((i for i, word in enumerate(command[:3])
                       if Path(word).name in ('gate.sh', 'gate.py')), None)
    if gate_index is not None and QUEUE_PATH == DEFAULT_QUEUE_PATH:
        selected = Path(command[gate_index]).resolve()
        if selected.name == 'gate.sh':
            selected = selected.parent/'build/gate.py'
        runner = Path(__file__).resolve().with_name('gate.py')
        # Candidate coverage stays pinned; installed controllers own execution
        # and child cleanup even when an older candidate knows the full lock.
        if selected == Path.cwd().resolve()/'scripts/build/gate.py' and selected != runner:
            command = [sys.executable, '-B', str(runner), *command[gate_index+1:],
                       '--source-policy', str(Path.cwd())]
            gate_index = 2
    full = (gate_index is not None and command[gate_index+1:gate_index+2] == ['full']
            and not any(flag in command for flag in ('--list','--help','-h')))
    capacity = nullcontext()
    if full:
        if inherited() is not None and full_capacity.inherited() is None:
            raise ValueError('Relaunch the full gate outside its old heavy lease')
        # Old checkout scripts cannot borrow this lock. Use this coherent
        # supervisor while retaining that checkout's exact command policy.
        runner = Path(__file__).with_name('gate.py')
        source_runner = Path(command[gate_index]).resolve()
        if source_runner.name == 'gate.sh':
            source_runner = source_runner.parent/'build/gate.py'
        with source_runner.open() as stream:
            source = stream.read(65537)
        if len(source) > 65536:
            raise ValueError('Gate runner exceeds its source bound')
        if 'FULL_CAPACITY_PROTOCOL = 1' not in source:
            command = [sys.executable, '-B', str(runner), *command[gate_index+1:],
                       '--source-policy', str(Path.cwd())]
        print(f'Waiting for full gate capacity before build admission (limit {args.wait}s).', flush=True)
        capacity = full_capacity.reserve(args.wait, shared=not '--exclusive-full' in command and target is not None)
    with capacity, slot('landing' if args.priority == 0 else 'gate', args.wait,
              job_owner=args.owner, ram=args.ram, nvme=target, outcome=outcome,
              max_hold=args.max_hold, projected=args.projected_seconds, refund=args.refund, gate_log=gate_log), \
            cache_lease(target):
        with owned_process(['ionice', '-c2', '-n7', 'nice', '-n', '10', *command],
                           pass_fds=inherited_fds()) as process:
            while True:
                check_hold_timeout()
                # Supervise a child that switches from shared work to exclusive.
                lease = inherited()
                if lease is not None:
                    lease[1].observe()
                budget = lease[1].exclusive_budget(lease[2], lease[0]['fd']) if lease else None
                if budget is not None and time.monotonic() - budget[0] >= budget[1]:
                    raise ExclusiveHoldTimeout('Exclusive heavy hold expired')
                try:
                    code = process.wait(timeout=1)
                    break
                except subprocess.TimeoutExpired:
                    pass
        outcome['exit'] = code if code >= 0 else 128 - code
        return outcome['exit']


if __name__ == '__main__':
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        sys.exit(main())
    except ExclusiveHoldTimeout as error:
        print(str(error), file=sys.stderr)
        sys.exit(124)
    except TimeoutError as error:
        print(error, file=sys.stderr)
        sys.exit(LOCK_TIMEOUT)
    except KeyboardInterrupt:
        sys.exit(130)
    except (OSError, ValueError, RuntimeError) as error:
        print(f'Heavy queue refused: {error}', file=sys.stderr)
        sys.exit(1)
