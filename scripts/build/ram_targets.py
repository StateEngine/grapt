"""Disposable gate targets with cgroup-enforced RAM ceilings and disk fallback."""
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time

ROOT = Path('/tmp/secode-ramtarget')
GIB = 1024**3
RUN_BYTES = 24*GIB
TOTAL_BYTES = 72*GIB
FLOOR_BYTES = 24*GIB
MARKER = '.secode-ram-run'
HEAVY_RAM_PROTOCOL = 1


def identity(pid):
    raw = Path(f'/proc/{pid}/stat').read_text()
    fields = raw.rsplit(')', 1)[1].split()
    if len(raw) > 8192 or fields[0] == 'Z':
        raise ProcessLookupError(pid)
    return [pid, fields[19], Path('/proc/sys/kernel/random/boot_id').read_text().strip()]


def owned(path, directory=False):
    info = path.lstat()
    if (info.st_uid != os.getuid() or info.st_mode & 0o077
            or not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))):
        raise ValueError(f'Unrecognized RAM target ownership: {path}')
    return info


@contextmanager
def locked():
    ROOT.mkdir(mode=0o700, exist_ok=True)
    owned(ROOT, True)
    fd = os.open(ROOT/'.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'a') as handle:
        owned(ROOT/'.lock')
        # Admission never queues behind another RAM target operation.
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield


def read_run(path):
    owned(path, True)
    info = owned(path/MARKER)
    if info.st_size > 4096:
        raise ValueError('Oversized RAM target owner')
    row = json.loads((path/MARKER).read_text())
    if (set(row) != {'owner', 'unit', 'budget', 'checkout'}
            or not re.fullmatch(r'run-[a-z0-9_]+', path.name)
            or row['unit'] != f'secode-ram-{path.name}.scope'
            or type(row['budget']) is not int or not 0 < row['budget'] <= RUN_BYTES
            or not isinstance(row['checkout'], str) or not Path(row['checkout']).is_absolute()
            or not isinstance(row['owner'], list) or len(row['owner']) != 3
            or type(row['owner'][0]) is not int or row['owner'][0] <= 0
            or not isinstance(row['owner'][1], str) or not row['owner'][1].isdigit()
            or not isinstance(row['owner'][2], str)
            or not re.fullmatch('[0-9a-f-]{36}', row['owner'][2])):
        raise ValueError('Invalid RAM target owner')
    return row


def entries():
    with os.scandir(ROOT) as stream:
        paths = []
        for entry in stream:
            if entry.name == '.lock':
                continue
            if len(paths) >= 64:
                raise ValueError('Too many RAM target entries')
            paths.append(Path(entry.path))
    return paths


def available():
    for line in Path('/proc/meminfo').read_text().splitlines():
        if line.startswith('MemAvailable:'):
            return int(line.split()[1])*1024
    raise ValueError('MemAvailable unavailable')


def check_capacity(budget=RUN_BYTES):
    reserved = sum(read_run(path)['budget'] for path in entries())
    # Reserve the entire ceiling even when a peer has not started writing yet.
    if reserved + budget > TOTAL_BYTES:
        raise ValueError('72 GiB RAM target reservation limit')
    if available() - reserved - budget < FLOOR_BYTES:
        raise ValueError('24 GiB available-memory reserve')
    if shutil.disk_usage(ROOT).free < budget:
        raise ValueError('Insufficient tmpfs capacity')


def capacity(budget=RUN_BYTES):
    filesystem = subprocess.check_output(['findmnt', '-n', '-o', 'FSTYPE', '-T', str(ROOT.parent)],
                                         text=True, timeout=5).strip()
    if filesystem != 'tmpfs':
        raise ValueError('RAM target root is not on tmpfs')
    with locked():
        check_capacity(budget)


def allocate(checkout, budget=RUN_BYTES):
    if type(budget) is not int or not 0 < budget <= RUN_BYTES:
        raise ValueError('Invalid RAM target ceiling')
    capacity(budget)
    with locked():
        check_capacity(budget)
        path = Path(tempfile.mkdtemp(prefix='run-', dir=ROOT))
        row = {'owner': identity(os.getpid()), 'unit': f'secode-ram-{path.name}.scope',
               'budget': budget, 'checkout': str(checkout.resolve())}
        marker = path/MARKER
        try:
            fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as handle:
                json.dump(row, handle)
        except BaseException:
            shutil.rmtree(path)
            raise
        return path, row


def cgroup():
    raw = Path('/proc/self/cgroup').read_text().strip()
    if not raw.startswith('0::/') or len(raw) > 4096:
        raise ValueError('Cgroup v2 unavailable')
    return Path('/sys/fs/cgroup')/raw[4:]


def validate(target, checkout):
    """Only a live reserved run inside its enforced scope may bypass disk accounting."""
    raw = os.environ.get('SECODE_RAM_TARGET_RUN')
    if not raw:
        return False
    # Fixture and nested Cargo targets retain their ordinary disk admission even
    # when launched by a RAM gate. An inherited hint grants them no RAM exemption.
    if not target.is_relative_to(ROOT):
        return False
    path = Path(raw)
    if path.parent != ROOT or path.is_symlink():
        raise ValueError('RAM target is outside the owned root')
    owned(ROOT, True)
    row = read_run(path)
    group = cgroup()
    if (identity(row['owner'][0]) != row['owner'] or row['checkout'] != str(checkout.resolve())
            or group.name != row['unit'] or int((group/'memory.max').read_text()) > row['budget']
            or (group/'memory.swap.max').read_text().strip() != '0'
            or (group/'memory.oom.group').read_text().strip() != '1'
            or target != path/'target'/'worktrees'/hashlib.sha256(str(checkout.resolve()).encode()).hexdigest()):
        raise ValueError('RAM target has no live memory governor')
    return True


def remove(path, row, deadline=None):
    def timeout(limit):
        left = limit if deadline is None else min(limit, deadline-time.monotonic())
        if left <= 0:
            raise RuntimeError('RAM cleanup exceeded its time limit')
        return left
    # Stop all writers, including descendants left behind by a killed supervisor.
    stopped = subprocess.run(['systemctl', '--user', 'stop', row['unit']],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=timeout(15))
    result = subprocess.run(['systemctl', '--user', 'show', row['unit'],
                             '-p', 'LoadState', '-p', 'ActiveState', '-p', 'ControlGroup'],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=timeout(5))
    lines = result.stdout.splitlines()
    fields = dict(line.split('=', 1) for line in lines if '=' in line)
    if (result.returncode != 0 or len(lines) != 3
            or set(fields) != {'LoadState', 'ActiveState', 'ControlGroup'}):
        raise RuntimeError('RAM writer state unavailable; preserve target')
    absent = fields == {'LoadState':'not-found','ActiveState':'inactive','ControlGroup':''}
    if not absent and (stopped.returncode != 0 or fields['LoadState'] != 'loaded'
                       or fields['ActiveState'] not in ('inactive', 'failed')):
        raise RuntimeError('RAM target writers have not stopped')
    if not absent and fields['ControlGroup']:
        raw = Path(fields['ControlGroup'])
        if not raw.is_absolute() or '..' in raw.parts or raw.name != row['unit']:
            raise RuntimeError('Unrecognized RAM writer cgroup; preserve target')
        group = Path('/sys/fs/cgroup')/str(raw).lstrip('/')
        try:
            events = dict(line.split() for line in (group/'cgroup.events').read_text().splitlines())
        except FileNotFoundError:
            events = {'populated':'0'}  # The stopped unit's cgroup has already been removed.
        if events.get('populated') != '0':
            raise RuntimeError('RAM target writer cgroup remains populated')
    if read_run(path) != row:
        raise ValueError('RAM target owner changed; preserve it')
    subprocess.run(['rm', '-rf', '--one-file-system', '--', str(path)], check=True, timeout=timeout(30))
    if path.exists():
        raise RuntimeError('Incomplete RAM target cleanup')
    if fields['ActiveState'] == 'failed':
        # The named scope is stopped and empty; drop its retained failure state.
        subprocess.run(['systemctl', '--user', 'reset-failed', row['unit']],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       check=True, timeout=timeout(5))


def reap(dry_run=False, deadline=None):
    removed = []
    failed = []
    deadline = time.monotonic()+60 if deadline is None else deadline
    if not ROOT.exists():
        return removed
    with locked():
        for path in entries():
            if time.monotonic() >= deadline:
                raise RuntimeError('RAM cleanup exceeded its time limit; remaining targets preserved')
            try:
                row = read_run(path)
                boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
                live = False
                if row['owner'][2] == boot:
                    try:
                        live = identity(row['owner'][0]) == row['owner']
                    except (FileNotFoundError, ProcessLookupError):
                        pass
                if live:
                    continue
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                print(f'Preserved RAM target {path}: {error}', file=sys.stderr)
                continue
            try:
                if not dry_run:
                    remove(path, row, deadline)
                print(f'{"Would remove" if dry_run else "Removed"} orphan RAM target {path}', flush=True)
                removed.append(path)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                print(f'Orphan RAM target cleanup failed {path}: {error}', file=sys.stderr)
                failed.append(path.name)
    if failed:
        raise RuntimeError('Orphan RAM targets remain: '+', '.join(failed))
    return removed


def pressure():
    try:
        return Path('/proc/pressure/io').read_text().strip()
    except OSError:
        return 'unavailable'


def child(arguments):
    """Read the kernel ceiling before executing any gate command, and retain accounting."""
    group = cgroup()
    path = Path(os.environ['SECODE_RAM_TARGET_RUN'])
    checkout = Path.cwd()
    validate(Path(os.environ['CARGO_TARGET_DIR']), checkout)
    (path/'started').write_text('governor verified\n')
    code = 1
    peak_shmem = 0
    try:
        import heavy
        process = subprocess.Popen(arguments, pass_fds=heavy.inherited_fds())
        while True:
            values = dict(line.split() for line in (group/'memory.stat').read_text().splitlines())
            peak_shmem = max(peak_shmem, int(values['shmem']))
            try:
                code = process.wait(timeout=0.25)
                break
            except subprocess.TimeoutExpired:
                continue
        return code
    finally:
        values = dict(line.split() for line in (group/'memory.stat').read_text().splitlines())
        events = dict(line.split() for line in (group/'memory.events').read_text().splitlines())
        (path/'accounting.json').write_text(json.dumps({
            'memory_peak_bytes': int((group/'memory.peak').read_text()),
            'tmpfs_sample_peak_bytes': max(peak_shmem, int(values['shmem'])),
            'tmpfs_final_bytes': int(values['shmem']), 'oom_kill': int(events['oom_kill']), 'exit': code}))


def run_gate(arguments, log):
    """Return None for disk execution; otherwise return the completed RAM gate status."""
    if os.environ.get('SECODE_RAM_TARGET_RUN') or any(arg in arguments for arg in ('--list', '--help', '-h')):
        return None
    path = None
    target = None
    row = None
    borrowed = False
    process = None
    began = time.monotonic()
    before = pressure()
    record = {'mode': 'disk', 'outcome': 'fallback', 'fallback': None, 'io_before': before}
    # The receipt/log must survive deletion of the disposable target.
    if log is None:
        log = Path(os.environ['CARGO_TARGET_DIR'])/'gate-logs'/f'{os.getpid()}.log'
        arguments.extend(['--log', str(log)])
    try:
        # Preserve the existing whole-disk-cache guard before allocating throwaway RAM.
        import evict_targets
        import heavy
        if heavy.disk_only():
            raise ValueError('Scheduler required disk placement')
        checkout = Path.cwd()
        if 'SECODE_TARGET_ROOT' not in os.environ:
            raise ValueError('A RAM gate needs the managed target root to preserve the disk cache first')
        disk = evict_targets.prepare(Path(os.environ['SECODE_TARGET_ROOT']),
                                     Path(os.environ['CARGO_TARGET_DIR']), checkout,
                                     proc=Path('/proc'))
        disk.close()
        reservation = heavy.reservation()
        borrowed = reservation is not None
        path, row = reservation if borrowed else allocate(checkout)
        record.update(mode='ram', outcome='running')
        scratch = path / 'tmp'
        scratch.mkdir(mode=0o700, exist_ok=True)
        owned(scratch, True)
        target = path/'target'/'worktrees'/hashlib.sha256(str(checkout).encode()).hexdigest()
        environment = dict(os.environ, SECODE_RAM_TARGET_RUN=str(path),
                           SECODE_RAM_DISK_CACHE=str(Path(os.environ['SECODE_TARGET_ROOT']).parent.parent.resolve()),
                           SECODE_TARGET_ROOT=str(path/'target'), SECODE_WORKTREE_TARGET=str(target),
                           CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL='0',
                           TMPDIR=str(scratch), TMP=str(scratch), TEMP=str(scratch))
        import cpu_priority
        selected_slice = cpu_priority.run_slice()
        cpu_priority.ram_slice(path, selected_slice)
        command = ['systemd-run', '--user', '--scope', '--quiet', '--unit', row['unit'],
                   '--slice=' + selected_slice,
                   '-p', f'MemoryMax={RUN_BYTES}', '-p', 'MemorySwapMax=0', '-p', 'OOMPolicy=kill',
                   '-p', 'TimeoutStopSec=5',
                   '-p', 'RuntimeMaxSec=28800', sys.executable, str(Path(__file__).resolve()), '--child',
                   sys.executable, str(Path(__file__).with_name('gate.py').resolve()), *arguments]
        heavy.note_target('ram')
        process = subprocess.Popen(command, env=environment, pass_fds=heavy.inherited_fds())
        code = process.wait()
        accounting = path/'accounting.json'
        values = json.loads(accounting.read_text()) if accounting.is_file() else {}
        record.update(values)
        if not (path/'started').exists() or values.get('oom_kill') or not values:
            record.update(mode='disk', outcome='fallback')
            record['fallback'] = 'RAM controls unavailable or RAM worker exceeded its ceiling'
        else:
            record['outcome'] = ('passed' if code == 0 else
                                 'disk-admission-refused' if code == 75 else 'failed')
            return code
    except KeyboardInterrupt:
        # Cancellation never executes the ordinary disk path after this attempt.
        record.update(outcome='interrupted', exit=130)
        raise
    except evict_targets.DiskAdmissionRefused:
        record.update(outcome='disk-admission-refused', exit=75)
        raise
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        record.update(mode='disk', outcome='fallback')
        record['fallback'] = str(error)
    finally:
        if target is not None and record['outcome'] != 'passed':
            import gate_evidence
            gate_evidence.preserve(target, log.with_suffix(log.suffix + '.ram'))
        else:
            gate_evidence = None
        retained = borrowed and record['outcome'] in ('passed', 'failed', 'disk-admission-refused')
        if path is not None and not retained:
            try:
                remove(path, row)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                if record['outcome'] != 'interrupted':
                    raise
                # Keep cancellation authoritative; the hourly reaper retains this target.
                record['cleanup_error'] = str(error)
                print(f'Interrupted gate cleanup failed; target preserved: {error}', file=sys.stderr)
        if record['outcome'] == 'interrupted' and process is not None:
            try:
                # The scope stop has begun; under load systemd can lag well
                # past an idle-box ten seconds before the child reaps.
                load = os.getloadavg()[0] / max(1, os.cpu_count() or 1)
                process.wait(timeout=min(240.0, 30.0 * max(1.0, load)))
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                record['child_reap_error'] = str(error)
                print(f'Interrupted gate child reap failed: {error}', file=sys.stderr)
        record.update(seconds=time.monotonic()-began, io_after=pressure())
        log.parent.mkdir(parents=True, exist_ok=True)
        log.with_suffix(log.suffix+'.ram.json').write_text(json.dumps(record, indent=2)+'\n')
        if gate_evidence is not None and record['outcome'] in ('failed', 'disk-admission-refused'):
            gate_evidence.archive_failure(log)
        detail = record.get('cleanup_error') or record['fallback']
        if not detail:
            detail = ('No new Cargo command admitted' if record['outcome'] == 'disk-admission-refused'
                      else 'Scheduler owns RAM cleanup' if retained else 'RAM target removed')
        print(f'Gate target: {record["mode"]}; {record["outcome"]}; {detail}', flush=True)
    # A failed shared RAM attempt cannot start the disk gate while a peer runs.
    heavy.require_disk(Path(os.environ['CARGO_TARGET_DIR']))
    heavy.note_target('disk')
    return None


if __name__ == '__main__':
    if sys.argv[1:2] != ['--child']:
        raise SystemExit('This helper is launched by the gate')
    raise SystemExit(child(sys.argv[2:]))
