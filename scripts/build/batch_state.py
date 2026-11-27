"""Owned, bounded controller checkpoints survive caller and network failures."""
import json
import os
import stat
from contextlib import contextmanager
import fcntl


@contextmanager
def lock(directory, *, create=False):
    if create:
        directory.mkdir(mode=0o700)
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Controller state requires a private owned directory')
    descriptor = os.open(directory / 'controller.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError('Another controller owns this attempt; inspect its durable job') from error
        yield
    finally:
        os.close(descriptor)


def save(path, state):
    temporary = path.with_suffix('.tmp')
    with temporary.open('w', encoding='utf-8') as output:
        json.dump(state, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())
    temporary.replace(path)
    descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def load(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_size > 1024**2:
            raise ValueError('Controller checkpoint must be a bounded owned file')
        value = source.read(1024**2 + 1)
    if len(value) > 1024**2:
        raise ValueError('Controller checkpoint exceeds its bound')
    return json.loads(value)
