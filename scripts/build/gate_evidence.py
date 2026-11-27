"""Keep a bounded dependency inventory beside a failed gate's surviving log."""
import hashlib
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import stat
import sys
import shutil
import tempfile
import time

MAX_ENTRIES = 16384
MAX_FILES = 4096
MAX_BYTES = 8 * 1024**3
MAX_SECONDS = 30


def inventory(target):
    deps = Path(target) / 'debug' / 'deps'
    result = {'version': 1, 'target': str(target), 'files': [], 'incomplete': []}
    names = []
    try:
        with os.scandir(deps) as entries:
            for index, entry in enumerate(entries):
                if index >= MAX_ENTRIES or len(names) >= MAX_FILES:
                    result['incomplete'].append('directory entry limit')
                    break
                if Path(entry.name).suffix in ('.rlib', '.rmeta'):
                    names.append(entry.name)
    except FileNotFoundError:
        result['incomplete'].append('dependency directory absent')
        return result
    remaining = MAX_BYTES
    deadline = time.monotonic() + MAX_SECONDS
    for name in sorted(names):
        row = {'name': name}
        result['files'].append(row)
        try:
            descriptor = os.open(deps / name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(descriptor, 'rb') as source:
                before = os.fstat(source.fileno())
                if not stat.S_ISREG(before.st_mode):
                    raise ValueError('not a regular file')
                row['bytes'] = before.st_size
                digest = hashlib.sha256()
                unread = before.st_size
                while unread:
                    if remaining <= 0 or time.monotonic() >= deadline:
                        raise ValueError('hash byte/time limit')
                    chunk = source.read(min(1024**2, remaining, unread))
                    if not chunk:
                        raise ValueError('file shortened while hashing')
                    remaining -= len(chunk)
                    unread -= len(chunk)
                    digest.update(chunk)
                after = os.fstat(source.fileno())
                current = (deps / name).lstat()
                row['sha256'] = digest.hexdigest()
                stamps = [(info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
                          for info in (before, after, current)]
                row['stable'] = stamps[0] == stamps[1] == stamps[2]
                if not row['stable']:
                    result['incomplete'].append(name + ': changed while hashing')
        except (OSError, ValueError) as error:
            row['error'] = str(error)
            result['incomplete'].append(name + ': hash unavailable')
    return result


def preserve(target, log):
    """Evidence errors are visible and never replace the original gate failure."""
    temporary = None
    try:
        result = inventory(target)
        destination = Path(log).with_suffix(Path(log).suffix + '.deps.json')
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode='w', dir=destination.parent,
                                         prefix=destination.name + '.', delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(result, stream, indent=2)
            stream.write('\n')
        temporary.replace(destination)
        temporary = None
        detail = ' (partial; see incomplete entries)' if result['incomplete'] else ''
        print(f'Failed gate dependency evidence: {destination}{detail}', file=sys.stderr)
    except (OSError, ValueError) as error:
        print(f'Failed gate dependency evidence unavailable: {error}', file=sys.stderr)
    finally:
        if temporary is not None:
            try:
                temporary.unlink()
            except OSError as error:
                print(f'Dependency evidence temporary file retained: {error}', file=sys.stderr)

ARCHIVE_SECONDS = 7 * 86400
ARCHIVE_MAX_BYTES = 8 * 1024**3
ARCHIVE_MAX_GROUPS = 4096
ARCHIVE_SIDECARS = ('.deps.json', '.nextest.xml', '.nextest.toml',
                    '.ram.deps.json', '.ram.json')
ARCHIVE_MARKER = '.secode-gate-archive'
ARCHIVE_MARKER_CONTENT = b'secode failed-gate archive group v1\n'


def archive_root():
    """Failed-gate evidence lives outside evictable build targets."""
    override = os.environ.get('SECODE_GATE_FAILURE_ARCHIVE')
    return Path(override) if override else operator_data_root() / 'gate-failures'


def operator_data_root():
    from operator_paths import data_root
    return data_root()


def _refuse(detail):
    print(f'Failed gate evidence retention refused: {detail}', file=sys.stderr)


def _stamped_groups(root):
    """Real (never symlinked) groups this archiver created, oldest first.

    A timestamped name alone is not ownership: only groups carrying the
    marker written at archive time are managed, so pre-existing timestamped
    evidence is preserved, never deleted.
    """
    groups = []
    with os.scandir(root) as entries:
        for entry in entries:
            if entry.is_symlink() or not entry.is_dir(follow_symlinks=False):
                continue
            marker = Path(entry.path) / ARCHIVE_MARKER
            if marker.is_symlink() or not marker.is_file():
                continue
            try:
                stamp = datetime.strptime(entry.name[:16], '%Y%m%dT%H%M%SZ')
                stamp = stamp.replace(tzinfo=timezone.utc)
            except ValueError:
                continue
            groups.append((entry.name, stamp))
    groups.sort(key=lambda group: (group[1], group[0]))
    return groups


def _remove_group(group):
    """Delete one group the marker proves this archiver created; report
    failures instead of claiming success."""
    try:
        if group.is_symlink():
            raise ValueError('group is a symlink')
        marker = group / ARCHIVE_MARKER
        if marker.is_symlink() or not marker.is_file():
            raise ValueError('group lacks the archive ownership marker')
        shutil.rmtree(group)
        return True
    except (OSError, ValueError) as error:
        print(f'Failed gate evidence prune incomplete: {group}: {error}', file=sys.stderr)
        return False


def archive_failure(log):
    """Copy a failed gate's log and sidecars into the seven-day retention archive.

    The originals stay where the immediate consumers (failure records, CI,
    tests) read them; only managed copies under a real, non-linked root are
    ever pruned. Retention errors are visible and never replace the original
    gate failure.
    """
    try:
        log = Path(log)
        if not log.is_file():
            return
        root = archive_root()
        if root.is_symlink():
            _refuse(f'archive root is a symlink: {root}')
            return
        try:
            source = log.resolve()
            managed = root.resolve()
        except (OSError, RuntimeError) as error:
            print(f'Failed gate evidence retention unavailable: {error}', file=sys.stderr)
            return
        if source == managed or managed in source.parents:
            _refuse(f'source is inside the archive root: {source}')
            return
        stamp = time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
        group = root / (stamp + '-' + str(os.getpid()))
        copied = 0
        created = False
        failure = None
        try:
            group.mkdir(parents=True)
            created = True
            (group / ARCHIVE_MARKER).write_bytes(ARCHIVE_MARKER_CONTENT)
            for name in (log.name, *(log.name + suffix for suffix in ARCHIVE_SIDECARS)):
                candidate = log.parent / name
                if candidate.is_file():
                    shutil.copy2(str(candidate), str(group / name))
                    copied += 1
        except (OSError, ValueError) as error:
            failure = error
            if created:
                shutil.rmtree(group, ignore_errors=True)
        prune_archive(root)
        if failure is not None:
            print(f'Failed gate evidence retention unavailable: {failure}', file=sys.stderr)
            return
        print(f'Failed gate evidence retained for {ARCHIVE_SECONDS // 86400} days: {group} ({copied} files)',
              file=sys.stderr)
    except (OSError, ValueError) as error:
        print(f'Failed gate evidence retention unavailable: {error}', file=sys.stderr)


def prune_archive(root):
    """Drop aged-out failed-gate groups, then the oldest until the group-count
    and byte bounds hold. Only real directories under a real root that carry
    this archiver's ownership marker are managed; anything unrecognized,
    including timestamped evidence that was never archived, is preserved and
    never deleted. A failed deletion stays on disk and is still accounted, and
    every failure is reported instead of claimed as retained."""
    root = Path(root)
    if root.is_symlink():
        _refuse(f'archive root is a symlink: {root}')
        return
    if not root.is_dir():
        return
    now = datetime.now(timezone.utc)
    for name, stamp in _stamped_groups(root):
        if (now - stamp).total_seconds() > ARCHIVE_SECONDS:
            _remove_group(root / name)
    survivors = []
    budget = ARCHIVE_MAX_GROUPS
    for name, stamp in reversed(_stamped_groups(root)):
        if budget > 0:
            budget -= 1
            survivors.append((name, stamp))
        elif not _remove_group(root / name):
            survivors.append((name, stamp))
    survivors.sort(key=lambda group: (group[1], group[0]))
    total = 0
    sized = []
    for name, _ in survivors:
        size = 0
        try:
            size = sum(part.stat().st_size for part in (root / name).iterdir()
                       if part.is_file())
        except OSError as error:
            print(f'Failed gate evidence prune incomplete: {root / name}: {error}',
                  file=sys.stderr)
        sized.append((name, size))
        total += size
    for name, size in sized:
        if total <= ARCHIVE_MAX_BYTES:
            break
        if _remove_group(root / name):
            total -= size
