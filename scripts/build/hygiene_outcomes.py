"""Bounded record of each scheduled cleanup pass, so a failing one is visible.

A timer run leaves nobody reading its stdout. A cleaner that has started
refusing every pass then looks exactly like one that found nothing to do, which
is how a cleaner stayed off for twenty-five minutes before anyone noticed. This
records one row per pass and answers a single question: how many passes in a row
failed.

Rows are bounded in count and bytes, and every read or write is best effort. A
damaged or unwritable record must never change what a cleanup decides.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import time

from operator_paths import data_root


ROWS = 160
BYTES = 64 * 1024
# A detail is bounded in characters only as a first cut. The real bound is on the
# encoded document, because JSON escapes and multibyte text both expand a row;
# append trims until the whole record fits, so this can never overflow it.
DETAIL_CHARS = 512
# A pass either did its work or it did not. The streak state below is part of the
# pass row itself, so a recovery is recorded whether or not any watcher ran,
# retention cannot lose it, and delivery rows (kept in the deliverer's own state)
# can never be mistaken for passes.
ROW_FIELDS = {'observed_unix', 'outcome', 'detail', 'streak_start', 'streak_len'}
PASS_OUTCOMES = {'succeeded', 'refused', 'failed'}
OUTCOMES = PASS_OUTCOMES
# Three consecutive failures is enough to be a pattern, few enough that a single
# bad night shows up rather than waiting for a trend.
ALERT_STREAK = 3


def record_path(directory=None):
    """One record for the one periodic cleaner, beside its cache."""
    directory = Path(directory) if directory is not None else data_root()
    return directory / 'hygiene-outcomes.json'


def row(outcome, detail=None, observed_unix=None, previous=None):
    """One pass outcome, carrying the streak it belongs to.

    `previous` is the last pass row already stored, if any. The streak is decided
    here, when the pass runs, rather than inferred later by whoever reads the
    record: a success writes length 0, and a failure continues the previous
    streak or begins one at its own timestamp. So a recovery exists in the record
    even if no watcher ever ran, and nothing a deliverer writes can alter it.
    """
    if not isinstance(outcome, str) or outcome not in OUTCOMES:
        raise ValueError(f'Unknown pass outcome: {outcome!r}')
    if detail is not None and not isinstance(detail, str):
        raise ValueError('Pass detail must be a string or None')
    now = time.time() if observed_unix is None else observed_unix
    if detail is not None:
        detail = detail[:DETAIL_CHARS]
    if outcome == 'succeeded':
        start, length = None, 0
    elif (isinstance(previous, dict) and previous.get('outcome') in {'refused', 'failed'}
            and type(previous.get('streak_len')) is int and previous['streak_len'] > 0
            and type(previous.get('streak_start')) in (int, float)):
        start, length = previous['streak_start'], previous['streak_len'] + 1
    else:
        start, length = now, 1
    return {'observed_unix': now, 'outcome': outcome, 'detail': detail,
            'streak_start': start, 'streak_len': length}


def _valid_row(sample):
    return (isinstance(sample, dict) and set(sample) == ROW_FIELDS
            and type(sample['observed_unix']) in (int, float)
            and isinstance(sample['outcome'], str) and sample['outcome'] in OUTCOMES
            and (sample['detail'] is None or isinstance(sample['detail'], str))
            and type(sample['streak_len']) is int and sample['streak_len'] >= 0
            and (sample['streak_start'] is None
                 or (type(sample['streak_start']) in (int, float)
                     and sample['streak_len'] > 0))
            and (sample['outcome'] == 'succeeded' or sample['streak_len'] > 0))


def _encoded(document):
    return json.dumps(document, allow_nan=False, separators=(',', ':'),
                      sort_keys=True) + '\n'


def _document(raw):
    if len(raw) > BYTES:
        raise RuntimeError('Pass outcome record exceeds its bound')
    try:
        value = json.loads(raw)
    except (ValueError, UnicodeError) as error:
        raise RuntimeError('Pass outcome record is unreadable') from error
    if (not isinstance(value, dict) or set(value) != {'version', 'samples'}
            or value['version'] != 1 or not isinstance(value['samples'], list)
            or not all(_valid_row(sample) for sample in value['samples'])):
        raise RuntimeError('Pass outcome record has an unexpected shape')
    return value


def _bounded_read(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > BYTES:
            raise RuntimeError('Pass outcome record is not a bounded regular file')
        raw = source.read(BYTES + 1)
    if len(raw) > BYTES:
        raise RuntimeError('Pass outcome record exceeds its bound')
    return raw


def _locked(path):
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


def samples(directory=None):
    """Every recorded pass, oldest first. Unreadable means none, never a guess."""
    path = record_path(directory)
    try:
        return _document(_bounded_read(path))['samples']
    except (OSError, RuntimeError):
        return []


def trailing_failures(directory=None):
    """The length of the failure streak the newest pass belongs to.

    Read from the row the pass wrote, never recounted from the log: the length is
    durable even when retention drops the rows it counts, and no other writer's
    row can change it.
    """
    recorded = samples(directory)
    if not recorded:
        return 0
    return recorded[-1]['streak_len']


def streak_id(sample):
    """A stable identity for the failure streak this row belongs to.

    The streak's start is written into every row of that streak, so this stays
    the same as the record rolls and changes only when a success begins a new one.
    """
    seed = f"streak:{sample['streak_start']}"
    return hashlib.sha256(seed.encode('utf-8', 'replace')).hexdigest()[:32]


def append(sample, directory=None):
    """Record one pass, keeping the record within its bounds.

    Best effort by construction: the lock is taken without waiting and every
    failure is reported to the caller rather than raised at it, because the
    pass's own result is what matters and this is only its witness.
    """
    if not _valid_row(sample):
        raise ValueError('Invalid pass outcome row')
    field_path = record_path(directory)
    try:
        field_path.parent.mkdir(parents=True, exist_ok=True)
    except OSError:
        return None
    descriptor = _locked(field_path.with_name(field_path.name + '.lock'))
    if descriptor is None:
        return None
    try:
        try:
            document = _document(_bounded_read(field_path))
        except FileNotFoundError:
            document = {'version': 1, 'samples': []}
        # The streak is decided here, against the row already stored, so the pass
        # itself never has to read the record back. A caller that did not supply
        # the fields gets them filled from the record's own previous pass.
        previous = document['samples'][-1] if document['samples'] else None
        sample = row(sample['outcome'], sample['detail'],
                     observed_unix=sample['observed_unix'], previous=previous)
        samples = (document['samples'] + [sample])[-ROWS:]
        # The bound is on bytes, and JSON escaping plus multibyte text can make a
        # document far larger than its content. So trim until the encoded form
        # truly fits, dropping the oldest rows first: the record always reads
        # back, which is what keeps the failure streak visible.
        while True:
            encoded = _encoded({'version': 1, 'samples': samples})
            if len(encoded.encode()) <= BYTES or len(samples) <= 1:
                break
            samples.pop(0)
        if len(encoded.encode()) > BYTES:
            return None  # One row cannot fit; better no row than a torn record.
        descriptor_written, staging = tempfile.mkstemp(
            dir=field_path.parent, prefix='.hygiene-outcomes-', suffix='.next')
        try:
            with os.fdopen(descriptor_written, 'w') as output:
                output.write(encoded)
                output.flush()
                os.fsync(output.fileno())
            os.replace(staging, field_path)
        except BaseException:
            Path(staging).unlink(missing_ok=True)
            raise
    except (OSError, RuntimeError):
        return None
    finally:
        os.close(descriptor)
    return field_path
