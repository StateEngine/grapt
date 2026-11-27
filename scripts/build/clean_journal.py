"""Durable, bounded record of what one cleanup pass decided about each target.

A scheduled cleanup runs under a timer, so nobody watches its stdout and the
journal is the only evidence it leaves. Without it a removal and a no-op look
the same afterwards, which is how a cleaner that frees nothing stays unnoticed.

Rows are bounded in count and bytes: a cleanup that keeps declining every target
must not grow the record of declining faster than it reclaims.
"""
import fcntl
import json
import os
from pathlib import Path
import stat
import tempfile
import time

from operator_paths import data_root


JOURNAL_ROWS = 200
JOURNAL_BYTES = 256 * 1024
ROW_FIELDS = {'observed_unix', 'target', 'outcome', 'bytes_before', 'bytes_after', 'detail'}
OUTCOMES = {'removed', 'retained', 'leased', 'live', 'unknown', 'unrecognised'}


def journal_path(directory=None):
    """One journal for the one owner of reclamation."""
    directory = (data_root() / 'logs') if directory is None else Path(directory)
    return directory / 'target-clean.json'


def row(target, outcome, *, bytes_before=None, bytes_after=None, detail=None):
    """One decision, with the sizes only where something was actually measured."""
    # Test the type before membership: an unhashable outcome would otherwise
    # raise TypeError out of the caller's error handling and reach the cleanup.
    if not isinstance(outcome, str) or outcome not in OUTCOMES:
        raise ValueError(f'Unknown cleanup outcome: {outcome!r}')
    for name, value in (('bytes_before', bytes_before), ('bytes_after', bytes_after)):
        if value is not None and type(value) is not int:
            raise ValueError(f'{name} must be an integer byte count or None')
    return {
        'observed_unix': time.time(),
        'target': str(target),
        'outcome': outcome,
        'bytes_before': bytes_before,
        'bytes_after': bytes_after,
        'detail': detail,
    }


def _valid_row(sample):
    if not isinstance(sample, dict) or set(sample) != ROW_FIELDS:
        return False
    if type(sample['observed_unix']) not in (int, float):
        return False
    if (not isinstance(sample['target'], str)
            or not isinstance(sample['outcome'], str)
            or sample['outcome'] not in OUTCOMES):
        return False
    for key in ('bytes_before', 'bytes_after'):
        if sample[key] is not None and type(sample[key]) is not int:
            return False
    return sample['detail'] is None or isinstance(sample['detail'], str)


def _document(raw):
    """Validate a stored journal; a damaged one is refused, never overwritten."""
    if len(raw) > JOURNAL_BYTES:
        raise RuntimeError('Cleanup journal exceeds its bound')
    try:
        value = json.loads(raw)
    except (ValueError, UnicodeError) as error:
        raise RuntimeError('Cleanup journal is unreadable') from error
    if (not isinstance(value, dict) or set(value) != {'version', 'samples'}
            or value['version'] != 1 or not isinstance(value['samples'], list)):
        raise RuntimeError('Cleanup journal has an unexpected shape')
    if not all(_valid_row(sample) for sample in value['samples']):
        raise RuntimeError('Cleanup journal holds an invalid row')
    return value


def _encoded(document):
    return json.dumps(document, allow_nan=False, separators=(',', ':'), sort_keys=True) + '\n'


def _bounded_read(path):
    """Read the journal within its declared bound before parsing any of it.

    Reading first and checking the length afterwards would let a corrupt or
    replaced journal allocate its whole size in memory before the cap applied.
    """
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode):
            raise RuntimeError('Cleanup journal is not a regular file')
        # The size is checked before any of it is read, so a replaced or corrupt
        # journal cannot allocate its whole length before the cap applies.
        if info.st_size > JOURNAL_BYTES:
            raise RuntimeError('Cleanup journal exceeds its bound')
        raw = source.read(JOURNAL_BYTES + 1)
    if len(raw) > JOURNAL_BYTES:
        raise RuntimeError('Cleanup journal exceeds its bound')
    return raw


def _locked(path):
    """Take the journal's lock, or return None when it cannot be taken safely.

    The open itself is non-blocking: a FIFO left at the lock's name would hang a
    plain ``open`` before ``flock`` was ever reached. Every step that can fail
    yields None, so the journal is skipped and cleanup carries on.
    """
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_NONBLOCK
                             | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    except OSError:
        return None
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            os.close(descriptor)
            return None
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(descriptor)
        return None
    return descriptor


def append(rows, directory=None):
    """Add this pass's rows to the journal, keeping it within its bounds.

    Telemetry never costs cleanup. The lock is opened and taken without waiting,
    so "written now" and "skipped" are the only outcomes; a damaged journal or an
    unwritable directory is reported to the caller rather than raised at it. The
    write is atomic, so an interrupted pass leaves the previous journal intact.
    """
    rows = list(rows)
    if not rows:
        return None
    path = journal_path(directory)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError:
        return None
    descriptor = _locked(path.with_name(path.name + '.lock'))
    if descriptor is None:
        return None
    try:
        try:
            document = _document(_bounded_read(path))
        except FileNotFoundError:
            document = {'version': 1, 'samples': []}
        document['samples'] = (document['samples'] + rows)[-JOURNAL_ROWS:]
        encoded = _encoded(document)
        while len(encoded.encode()) > JOURNAL_BYTES and len(document['samples']) > 1:
            document['samples'].pop(0)
            encoded = _encoded(document)
        if len(encoded.encode()) > JOURNAL_BYTES:
            raise RuntimeError('Cleanup journal row exceeds its bound')
        temporary, staging = tempfile.mkstemp(
            dir=path.parent, prefix='.target-clean-', suffix='.next')
        try:
            with os.fdopen(temporary, 'w') as output:
                output.write(encoded)
                output.flush()
                os.fsync(output.fileno())
            os.replace(staging, path)
        except BaseException:
            Path(staging).unlink(missing_ok=True)
            raise
    except OSError:
        return None
    finally:
        os.close(descriptor)
    return path
