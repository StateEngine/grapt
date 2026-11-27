"""Run gate work above ordinary seat builds, without a CPU quota."""
import argparse
import math
import os
import re
from pathlib import Path
import signal
import stat
import tempfile
import subprocess
import uuid

SLICE = 'secodegate.slice'
WEIGHT = 1000
USER_GROUP = Path(f'/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service')
CGROUP = Path('/sys/fs/cgroup') / str(USER_GROUP).lstrip('/')


def units():
    # No dash in the slice name: it must be a sibling of app.slice, rather
    # than a child of an implicitly created, equally weighted parent slice.
    return {SLICE: f'''[Unit]
Description=SE Code gate CPU priority
[Slice]
CPUWeight={WEIGHT}
'''}


def small(path):
    with path.open('rb') as source:
        raw = source.read(4097)
    if len(raw) > 4096:
        raise ValueError('CPU priority control exceeds its bound')
    return raw.decode().strip()


def configured():
    try:
        gate = int(small(CGROUP / SLICE / 'cpu.weight'))
        background = int(small(CGROUP / 'app.slice/cpu.weight'))
        quota = small(CGROUP / SLICE / 'cpu.max').split()
    except FileNotFoundError:
        return False
    return gate == WEIGHT and 1 <= background < gate and len(quota) == 2 and quota[0] == 'max'


def active():
    value = small(Path('/proc/self/cgroup'))
    return value.startswith('0::' + str(USER_GROUP / SLICE) + '/')


def valid_slice(name):
    return name == SLICE or bool(re.fullmatch(re.escape(SLICE[:-6]) + r'-[0-9a-f]{32}[.]slice', name))


def run_slice():
    value = small(Path('/proc/self/cgroup'))
    prefix = '0::' + str(USER_GROUP / SLICE) + '/'
    if value.startswith(prefix):
        candidate = value[len(prefix):].split('/', 1)[0]
        if re.fullmatch(re.escape(SLICE[:-6]) + r'-[0-9a-f]{32}[.]slice', candidate):
            return candidate
    return SLICE


def recorded_slice(path):
    try:
        descriptor = os.open(path / 'priority-slice', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return None
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077 or info.st_size > 128:
            raise ValueError('RAM priority placement is not a bounded owned file')
        value = source.read(129).decode('ascii').strip()
    if not valid_slice(value):
        raise ValueError('RAM priority slice is not owned')
    return value


def ram_slice(path, selected):
    if not valid_slice(selected):
        raise ValueError('RAM priority slice is not owned')
    # Keep the original RAM lease wire format. The bounded sidecar records
    # placement for accounting without requiring a systemd query under Queue.locked.
    marker = path / 'priority-slice'
    with tempfile.NamedTemporaryFile(mode='w', encoding='ascii', dir=path,
                                     prefix='.priority-slice-', delete=False) as output:
        temporary = Path(output.name)
        output.write(selected + '\n')
    try:
        try:
            os.link(temporary, marker)  # Accounting sees either absent or a complete record.
        except FileExistsError:
            if recorded_slice(path) != selected:
                raise ValueError('Preserve an existing RAM priority placement') from None
    finally:
        temporary.unlink()


def state(unit):
    result = subprocess.run(['systemctl', '--user', 'show', unit,
        '-p', 'LoadState', '-p', 'ActiveState', '-p', 'ControlGroup', '-p', 'FragmentPath'],
        capture_output=True, text=True, timeout=5)
    if len(result.stdout) > 8192:
        raise RuntimeError('Gate scope state exceeds its bound')
    lines = result.stdout.splitlines()
    fields = dict(line.split('=', 1) for line in lines if '=' in line)
    if len(lines) != 4 or set(fields) != {'LoadState', 'ActiveState', 'ControlGroup', 'FragmentPath'}:
        raise RuntimeError('Gate scope state is unavailable')
    absent = dict(LoadState='not-found', ActiveState='inactive', ControlGroup='', FragmentPath='')
    if result.returncode and fields != absent:
        raise RuntimeError('Gate scope state could not be read')
    return fields


def stop(unit):
    before = state(unit)
    if before['LoadState'] == 'not-found':
        return
    expected = str(USER_GROUP / SLICE / unit)
    if before['ControlGroup'] not in ('', expected):
        raise RuntimeError('Gate scope ownership changed; preserve it')
    result = subprocess.run(['systemctl', '--user', 'stop', unit],
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=15)
    after = state(unit)
    if after['LoadState'] == 'not-found':
        return
    if result.returncode or after['ActiveState'] not in ('inactive', 'failed'):
        raise RuntimeError('Gate scope did not stop')
    if after['ControlGroup']:
        if after['ControlGroup'] != expected:
            raise RuntimeError('Gate scope ownership changed during shutdown')
        try:
            events = dict(line.split() for line in small(
                CGROUP / SLICE / unit / 'cgroup.events').splitlines())
        except FileNotFoundError:
            events = {'populated': '0'}
        if events.get('populated') != '0':
            raise RuntimeError('Gate scope still contains writers')


def run(command, *, timeout=43200):
    if not command or not math.isfinite(timeout) or not 0 < timeout <= 43200:
        raise ValueError('Gate priority needs a command and a bounded timeout')
    import heavy
    if active():
        return heavy.run(command, timeout=timeout).returncode
    if os.environ.get('SECODE_RAM_TARGET_RUN'):
        raise ValueError('Preserve the inherited RAM governor; enter gate priority before allocating RAM')
    if not configured():
        raise RuntimeError('Install and start the reviewed secodegate.slice before using gate priority')
    token = uuid.uuid4().hex
    unit = f'secode-gate-{os.getpid()}-{token}.scope'
    owned_slice = SLICE[:-6] + '-' + token + '.slice'
    before = state(owned_slice)
    if (before['LoadState'] != 'not-found' and
            (before['LoadState'] != 'loaded' or before['ActiveState'] != 'inactive'
             or before['ControlGroup'] or before['FragmentPath'])):
        raise RuntimeError('Preserve an existing gate slice')
    if state(unit)['LoadState'] != 'not-found':
        raise RuntimeError('Preserve an existing gate scope')
    import durable_jobs
    if durable_jobs.active() and SLICE == 'secodegate.slice':
        # ExecStopPost can join this sibling slice even after its supervisor
        # is killed before the normal finally block runs.
        record = durable_jobs.directory(os.environ[durable_jobs.JOB_ENV])/('priority-'+token+'.slice')
        with record.open('x') as output:
            output.write(owned_slice+'\n')
    arguments = ['systemd-run', '--user', '--scope', '--quiet', '--collect',
        '--slice=' + owned_slice, '--unit=' + unit, '-p', 'TimeoutStopSec=5',
        '-p', 'RuntimeMaxSec=43200', '--', *command]
    try:
        return heavy.run(arguments, timeout=timeout, pass_fds=heavy.inherited_fds()).returncode
    finally:
        # RAM workers have their own memory-governed scope. Both scopes live
        # under this run's slice, including after a supervisor crash.
        stop(owned_slice)


if __name__ == '__main__':
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--timeout', type=float, default=43200)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    try:
        raise SystemExit(run(command, timeout=args.timeout))
    except KeyboardInterrupt:
        raise SystemExit(130) from None
