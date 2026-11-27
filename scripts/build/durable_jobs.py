"""Keep heavy jobs in independent user services when a seat is relaunched."""
import json
import os
from pathlib import Path
import re
import shutil
import selectors
import select
import stat
import subprocess
import sys
import time
import uuid

JOBS = Path.home()/'.local/state/secode/jobs'
JOB_ENV = 'SECODE_DURABLE_JOB'
# Lease and run tags name descriptors of the spawning process. A systemd unit
# inherits the names over D-Bus but not the descriptors, so these never cross
# the job boundary (matches the strip gate.py applies to its suite child).
NONTRANSFERABLE_ENV = ('SECODE_HEAVY_LEASE', 'SECODE_FULL_GATE_LEASE',
                       'SECODE_RAM_TARGET_RUN')
MAX_LOG = 64*1024**2
# The history holds this many records before finished ones move to the archive.
MAX_HISTORY = 256
# An empty record older than this is a stranded admission, not a live job.
EMPTY_RECORD_AGE = 86400
# A loaded host's user manager can miss a 5 s answer; only a long silence ends the follow.
STATE_PATIENCE = 600


def read(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 65536:
            raise ValueError('Job record is not a bounded owned file')
        raw = source.read(65537)
    if len(raw) > 65536:
        raise ValueError('Job record exceeds its bound')
    return json.loads(raw)


def write(path, row):
    temporary = path.with_suffix('.tmp')
    with temporary.open('x') as output:
        json.dump(row, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    temporary.rename(path)


def directory(token):
    if not re.fullmatch('[0-9a-f]{32}', token):
        raise ValueError('Invalid durable job identity')
    path = JOBS/token
    try:
        info = path.lstat()
    except FileNotFoundError:
        path = JOBS.parent/'completed-jobs'/token
        info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Preserve an unowned or shared job directory')
    return path


def locate(token):
    """The job's directory, in the history or an archive, ownership-checked."""
    if not re.fullmatch('[0-9a-f]{32}', token):
        raise ValueError('Invalid durable job identity')
    for base in (JOBS, JOBS.parent/'completed-jobs', JOBS.parent/'jobs-archive'):
        path = base/token
        try:
            info = path.lstat()
        except FileNotFoundError:
            continue
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError('Preserve an unowned or shared job directory')
        return path
    raise FileNotFoundError('Durable job is absent from the history and its archives: '+token)


def archive(token):
    """Explicitly move one completed job; durable_archive holds the rules."""
    from durable_archive import archive as selected
    return selected(token)


def active():
    token = os.environ.get(JOB_ENV)
    return token is not None and re.fullmatch('[0-9a-f]{32}', token) is not None


def request_for(path, token):
    request = read(path/'request.json')
    if request.get('job') != token or request.get('unit') != 'secode-job-'+token+'.service':
        raise ValueError('Job request does not own the selected service')
    return request


def fenced(command):
    """Every new session is visible to StateEngine's detached-flock fence."""
    if not active():
        return command
    path = directory(os.environ[JOB_ENV])
    return [sys.executable, '-B', str(Path(__file__).resolve()), '--session',
            os.environ[JOB_ENV], *command]


def session(token, command):
    """Fence the session with a companion; exec keeps the child's signal status."""
    path = directory(token)
    ready_read, ready_write = os.pipe()
    held_read, held_write = os.pipe()
    companion = None
    try:
        # The companion belongs to this session and exits when the command and
        # its descendants close held_write. owned_process joins the whole group.
        probe = 'import os,sys;os.write(int(sys.argv[1]),b"1");os.close(int(sys.argv[1]));os.read(0,1)'
        companion = subprocess.Popen(fence(path, [sys.executable, '-c', probe, str(ready_write)]),
                                     stdin=held_read, pass_fds=(ready_write,))
        os.close(held_read); held_read = None
        os.close(ready_write); ready_write = None
        if not select.select([ready_read], [], [], 10)[0] or os.read(ready_read, 1) != b'1':
            raise RuntimeError('Session flock did not become ready')
        os.close(ready_read); ready_read = None
        os.set_inheritable(held_write, True)
        os.execvp(command[0], command)
    finally:
        for descriptor in (ready_read, ready_write, held_read, held_write):
            if descriptor is not None: os.close(descriptor)
        if companion is not None:
            companion.wait(timeout=10)


def fence(path, command):
    return ['flock', '--shared', '--close', str(path/'lifecycle.lock'), *command]


def service_state(unit):
    """The unit's ActiveState, or None while a busy user manager cannot answer."""
    try:
        result = subprocess.run(['systemctl', '--user', 'show', unit, '-p', 'ActiveState', '--value'],
                                capture_output=True, text=True, timeout=5)
    except subprocess.TimeoutExpired:
        return None
    if result.returncode or len(result.stdout) > 128:
        raise RuntimeError('Durable job service state is unavailable')
    return result.stdout.strip()


def follow(token):
    path = locate(token)
    try:
        request = request_for(path, token)
    except FileNotFoundError:
        path = locate(token)
        request = request_for(path, token)

    def relocated():
        nonlocal path, request
        selected = locate(token)
        if selected == path:
            return False
        next_request = request_for(selected, token)
        # The mover may repoint the stored log path; the identity fields
        # never change under an archival.
        if (next_request.get('job'), next_request.get('unit'), next_request.get('source')) != \
                (request.get('job'), request.get('unit'), request.get('source')):
            raise ValueError('Job request changed during archival')
        path = selected
        request = next_request
        return True
    offset = 0
    checked = 0
    answered = time.monotonic()
    while True:
        relocated()
        try:
            if (path/'output.log').exists():
                with (path/'output.log').open('rb') as log:
                    log.seek(offset)
                    raw = log.read(65536)
                offset += len(raw)
                if raw:
                    sys.stdout.buffer.write(raw)
                    sys.stdout.buffer.flush()
            if (path/'result.json').exists():
                row = read(path/'result.json')
                if type(row) is not dict or row.get('job') != token or row.get('source') != request['source'] \
                        or type(row.get('exit')) is not int:
                    raise ValueError('Durable terminal result differs from its request')
                try:
                    length = (path/'output.log').stat().st_size
                except FileNotFoundError:
                    if relocated():
                        continue
                    length = 0
                if offset < length:
                    continue
                print('Durable job result: '+str(path/'result.json'), flush=True)
                return row['exit']
            if time.monotonic()-checked > 10:
                state = service_state(request['unit'])
                if state in ('inactive', 'failed'):
                    if relocated():
                        continue
                    raise RuntimeError('Durable service ended without a terminal result: '+str(path))
                checked = time.monotonic()
                if state is not None:
                    answered = checked
                elif checked-answered > STATE_PATIENCE:
                    raise RuntimeError('Durable job service state is unavailable')
            time.sleep(.2)
        except FileNotFoundError:
            if not relocated():
                raise


def filled():
    with os.scandir(JOBS) as entries:
        for index, _ in enumerate(entries):
            if index + 1 >= MAX_HISTORY:
                return True
    return False


def archive_completed(limit):
    """Move the oldest finished records to the sibling archive, leaving the recent ones."""
    archive = JOBS.parent/'jobs-archive'
    if not archive.is_dir():
        try:
            archive.mkdir(mode=0o700)
        except FileExistsError:
            pass
    info = archive.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Preserve an unowned or shared job archive')
    now = time.time()
    finished = []
    count = 0
    with os.scandir(JOBS) as entries:
        for entry in entries:
            count += 1
            try:
                record = entry.stat(follow_symlinks=False)
            except OSError:
                continue
            if not stat.S_ISDIR(record.st_mode) or record.st_uid != os.getuid() or record.st_mode & 0o077:
                continue
            if re.fullmatch(r'staging-[0-9a-f]{32}', entry.name):
                # A hard kill between admission staging and its rename can
                # strand a staging record; no live admission survives the bound.
                # Only the exact staged token shape is ours to remove; a failed
                # removal leaves the running count untouched.
                if now-record.st_mtime > EMPTY_RECORD_AGE:
                    path = JOBS/entry.name
                    try:
                        shutil.rmtree(path)
                    except OSError:
                        pass
                    if not path.exists():
                        count -= 1
                continue
            if not re.fullmatch('[0-9a-f]{32}', entry.name):
                continue
            if now-record.st_mtime > EMPTY_RECORD_AGE:
                # rmdir only empties: an aborted admission must not hold a slot.
                try:
                    (JOBS/entry.name).rmdir()
                except OSError:
                    pass
                else:
                    if not (JOBS/entry.name).exists():
                        count -= 1
                        continue
            result = JOBS/entry.name/'result.json'
            if result.is_symlink() or not result.is_file():
                continue
            try:
                outcome = read(result)
            except (ValueError, FileNotFoundError):
                # A concurrent admission may have archived this candidate
                # between the existence check and the read; drop it and scan on.
                continue
            if type(outcome) is not dict:
                # A non-object record is not a finished job; keep it and continue.
                continue
            stamp = outcome.get('finished_unix')
            if type(stamp) is not int and type(stamp) is not float:
                stamp = result.stat().st_mtime
            finished.append((stamp, entry.name))
    finished.sort()
    moved = 0
    for stamp, name in finished:
        if count < limit:
            return moved
        if now-stamp < 7200:
            continue
        target = archive/name
        if target.is_symlink() or target.exists():
            continue
        # Validate the request before moving: a record whose request is
        # missing, unreadable or not an object cannot be repointed, so it
        # stays in the history and the scan continues.
        request_path = JOBS/name/'request.json'
        if request_path.is_symlink() or not request_path.is_file():
            continue
        try:
            request = read(request_path)
        except (ValueError, FileNotFoundError):
            continue
        if type(request) is not dict:
            continue
        try:
            (JOBS/name).rename(target)
        except OSError:
            continue
        try:
            # Both stored log paths must follow the job they describe.
            request['log'] = str(target/'output.log')
            write(target/'request.json', request)
            outcome = read(target/'result.json')
            if type(outcome) is dict:
                outcome['log'] = str(target/'output.log')
                write(target/'result.json', outcome)
        except (ValueError, OSError):
            pass
        count -= 1
        moved += 1
    return moved


def run(command, owner, *, detach=False):
    JOBS.mkdir(parents=True, exist_ok=True, mode=0o700)
    if filled():
        archive_completed(MAX_HISTORY)
    if filled():
        raise ValueError('Durable job history is full; automatic archival retains active, '
                         'pinned and recent records, use heavy archive JOB_ID for completed '
                         'records no longer needed by pending receipts')
    attempts = 0
    while True:
        token = uuid.uuid4().hex
        final = JOBS/token
        path = JOBS/('staging-'+token)
        # Stage outside the job-identity pattern so an aborted admission
        # never claims a history slot under its final name. A collision means
        # this token is already taken; pick a new one and retry, never
        # touching a stage this admission did not create.
        try:
            path.mkdir(mode=0o700)
        except FileExistsError:
            if attempts >= 8:
                raise
            attempts += 1
            continue
        committed = False
        try:
            unit = 'secode-job-'+token+'.service'
            source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True, timeout=10).strip()
            if not re.fullmatch('[0-9a-f]{40}', source):
                raise ValueError('Durable job requires an identifiable source commit')
            write(path/'request.json', dict(job=token, unit=unit, owner=owner, source=source,
                cwd=str(Path.cwd()), command=command, log=str(final/'output.log')))
            (path/'lifecycle.lock').touch(mode=0o600)
            # The supervisor itself must not name a seat's directory in its argv.
            helper = final/'durable_jobs.py'
            shutil.copyfile(__file__, path/'durable_jobs.py')
            shutil.copyfile(Path(__file__).with_name('cpu_priority.py'), path/'cpu_priority.py')
            arguments = ['systemd-run', '--user', '--quiet', '--collect', '--unit='+unit,
                '--slice=secodejobs.slice', '--working-directory='+str(Path.cwd()),
                '-p', 'Type=exec', '-p', 'KillMode=control-group', '-p', 'RuntimeMaxSec=86400',
                '-p', 'TimeoutStopSec=180', '-p', 'StandardOutput=journal', '-p', 'StandardError=journal',
                '-p', 'ExecStopPost='+sys.executable+' -B '+str(helper)+' --finish '+token]
            # Copy environment values over D-Bus, keeping them out of the request
            # files and launcher argv. Inherited leases never cross this boundary.
            arguments += ['--setenv='+key for key in os.environ
                          if re.fullmatch('[A-Za-z_][A-Za-z0-9_]*', key)
                          and key not in NONTRANSFERABLE_ENV]
            arguments += ['--setenv='+JOB_ENV+'='+token, '--', *fence(final, [sys.executable, '-B', str(helper), '--worker', token])]
            path.rename(final)
            committed = True
            break
        finally:
            if not committed:
                # A stage this admission created is always ours to remove; a
                # failed removal must not be hidden behind the launch.
                shutil.rmtree(path)
    subprocess.run(arguments, check=True, timeout=15)
    print('Durable job '+token+': '+str(final/'output.log')+'; disconnecting keeps it running.', flush=True)
    return 0 if detach else follow(token)


def worker(token):
    path = directory(token)
    request = request_for(path, token)
    with (path/'output.log').open('xb') as output:
        process = subprocess.Popen(fenced(request['command']), cwd=request['cwd'],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, start_new_session=True)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while True:
                ready = selector.select(.2)
                if ready:
                    raw = os.read(process.stdout.fileno(), 65536)
                    if not raw: break
                    if output.tell()+len(raw) > MAX_LOG:
                        raise ValueError('Durable job output exceeds 64 MiB')
                    output.write(raw);output.flush()
                elif process.poll() is not None:
                    break
        code = process.wait()
    write(path/'completed.json', dict(exit=code if code >= 0 else 128-code))
    return code if code >= 0 else 128-code


def finish(token):
    path = directory(token)
    request = request_for(path, token)
    import cpu_priority
    for index, record in enumerate(path.glob('priority-*.slice')):
        if index >= 64:
            raise ValueError('Durable job priority inventory exceeds its bound')
        selected = cpu_priority.small(record)
        expected = 'secodegate-'+record.name.removeprefix('priority-')
        if selected != expected or not re.fullmatch('secodegate-[0-9a-f]{32}[.]slice', selected):
            raise ValueError('Preserve an unowned priority slice')
        cpu_priority.stop(selected)
    code = 1
    if (path/'completed.json').exists():
        completed = read(path/'completed.json')['exit']
        result = os.environ.get('SERVICE_RESULT')
        if type(completed) is int and 0 <= completed <= 255 and (
                result == 'success' and completed == 0 or result == 'exit-code' and completed != 0):
            code = completed
    write(path/'result.json', dict(job=token, owner=request['owner'], source=request['source'],
        log=request['log'], exit=code, service_result=os.environ.get('SERVICE_RESULT', 'unknown'),
        finished_unix=time.time()))


if __name__ == '__main__':
    if len(sys.argv) >= 4 and sys.argv[1] == '--session':
        session(sys.argv[2], sys.argv[3:])
        raise SystemExit(1)
    if len(sys.argv) != 3 or sys.argv[1] not in ('--worker', '--finish', '--follow', '--cancel', '--archive'):
        raise SystemExit('Use --worker, --finish, --follow, --cancel or --archive with an exact job identity')
    mode, token = sys.argv[1:]
    if mode == '--worker':
        raise SystemExit(worker(token))
    if mode == '--finish':
        finish(token)
    elif mode == '--follow':
        raise SystemExit(follow(token))
    elif mode == '--archive':
        print(archive(token))
    else:
        path = directory(token)
        subprocess.run(['systemctl', '--user', 'stop', request_for(path, token)['unit']], check=True, timeout=190)
