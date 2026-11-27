"""Acquire host-wide full-gate capacity before charging build resources."""
from contextlib import contextmanager
import fcntl
import json
import math
import os
from pathlib import Path
import stat
import subprocess

LOCK = Path.home()/'secode-full-workspace.lock'
CLAIM = 'SECODE_FULL_GATE_LEASE'


def writer_waiting(handle):
    info = os.fstat(handle.fileno())
    identity = (os.major(info.st_dev), os.minor(info.st_dev), info.st_ino)
    with Path('/proc/locks').open('rb') as stream:
        raw = stream.read(1024*1024+1)
    if len(raw) > 1024*1024:
        raise RuntimeError('Full-slot lock table exceeds 1 MiB')
    for line in raw.decode('ascii').splitlines():
        fields = line.split()
        if len(fields) != 9 or fields[1:5] != ['->', 'FLOCK', 'ADVISORY', 'WRITE']:
            continue
        device = fields[6].split(':')
        if len(device) == 3 and (int(device[0],16), int(device[1],16), int(device[2])) == identity:
            return True
    return False


def inherited(path=None):
    path = LOCK if path is None else path
    raw = os.environ.get(CLAIM)
    if raw is None:
        return None
    if len(raw) > 1024:
        raise ValueError('Full-slot claim exceeds its bound')
    value = json.loads(raw)
    if (not isinstance(value, dict) or set(value) != {'fd', 'device', 'inode', 'mode'} or
            any(type(value[k]) is not int or value[k] < 0 for k in ('fd','device','inode')) or
            value['fd'] > 1048576 or value['mode'] not in ('READ', 'WRITE')):
        raise ValueError('Invalid inherited full-slot claim')
    info = os.fstat(value['fd']); expected = path.stat()
    if (not stat.S_ISREG(info.st_mode) or
            (info.st_dev,info.st_ino) != (expected.st_dev,expected.st_ino) or
            (info.st_dev,info.st_ino) != (value['device'],value['inode'])):
        raise ValueError('Inherited full-slot descriptor changed')
    with Path(f'/proc/self/fdinfo/{value["fd"]}').open() as stream:
        record = stream.read(65537)
    key = f'{os.major(info.st_dev):02x}:{os.minor(info.st_dev):02x}:{info.st_ino}'
    if len(record) > 65536 or not any(
            len(fields) > 6 and fields[:1] == ['lock:'] and
            fields[2:5] == ['FLOCK','ADVISORY',value['mode']] and fields[6] == key
            for fields in map(str.split, record.splitlines())):
        raise ValueError('Inherited full-slot descriptor does not retain its kernel lock')
    return value


def acquire(path, wait=0, *, shared=False):
    if not math.isfinite(wait) or not 0 <= wait <= 43200:
        raise ValueError('Full-slot wait must be between zero and twelve hours')
    claim = inherited(path)
    if claim is not None:
        if not shared and claim['mode'] != 'WRITE':
            raise ValueError('An exclusive full gate cannot borrow shared capacity')
        return os.fdopen(os.dup(claim['fd']), 'a')
    handle = path.open('a')
    try:
        # Join the exclusive kernel queue behind an already waiting writer.
        # New readers cannot keep replenishing its shared predecessors.
        shared = shared and not writer_waiting(handle)
        if wait:
            subprocess.run(['flock', '--shared' if shared else '--exclusive',
                            '--conflict-exit-code','73','--wait',str(wait),str(handle.fileno())],
                           pass_fds=(handle.fileno(),), timeout=wait+1, check=True)
        else:
            fcntl.flock(handle, (fcntl.LOCK_SH if shared else fcntl.LOCK_EX) | fcntl.LOCK_NB)
        return handle
    except BaseException:
        handle.close()
        raise


@contextmanager
def reserve(wait, *, shared=True, path=None):
    path = LOCK if path is None else path
    with acquire(path, wait, shared=shared) as handle:
        info = os.fstat(handle.fileno())
        # An inherited writer remains a writer even for an ordinary full gate.
        key = f'{os.major(info.st_dev):02x}:{os.minor(info.st_dev):02x}:{info.st_ino}'
        with Path(f'/proc/self/fdinfo/{handle.fileno()}').open() as stream:
            record = stream.read(65537)
        modes = [fields[4] for fields in map(str.split,record.splitlines())
                 if len(fields)>6 and fields[:1]==['lock:'] and
                 fields[2:4]==['FLOCK','ADVISORY'] and fields[6]==key]
        if len(record)>65536 or len(modes)!=1:
            raise ValueError('Full-slot reservation has no unique kernel lock')
        previous = os.environ.get(CLAIM)
        os.environ[CLAIM] = json.dumps(dict(fd=handle.fileno(), device=info.st_dev,
                                           inode=info.st_ino, mode=modes[0]))
        try:
            yield handle
        finally:
            if previous is None:
                os.environ.pop(CLAIM,None)
            else:
                os.environ[CLAIM]=previous


def inherited_fds():
    value = inherited()
    return (value['fd'],) if value is not None else ()
