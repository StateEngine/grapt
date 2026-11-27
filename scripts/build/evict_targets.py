"""Admit builds against the whole cache; reclaim only owned, idle targets."""
import fcntl
from functools import lru_cache
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

import cargo_observation


# Binding cap for the shared SE Code cache. One 1.9 TB disk is shared with
# StateEngine, so the cache keeps a ceiling far below the disk rather than the
# 300 GiB it used to advertise: three admissions to the 60 GiB floor happened
# in one day with the old number.
# Protected bytes still cannot block build progress; `effective_cap` only trims
# this further to honour the floor.
CAP_BYTES = 150 * 1024**3
FREE_SPACE_FLOOR_BYTES = 60_000_000_000
BUILD_RESERVE_BYTES = 0
HEAVY_QUEUE = Path.home() / '.local/state/secode/heavy/tickets.json'
RECENT_SECONDS = 60 * 60
# Whole targets idle longer than this are eviction candidates, oldest first.
# Twelve hours, not a day: a gate target is ~96 GB and an idle one blocks the
# floor long before it is a day old.
STALE_SECONDS = 12 * 3600
MAINTENANCE_SECONDS = 900
RELEASED = '.secode-released'
FINISHED = '.secode-finished'
MAX_TARGET_ENTRIES = 262144
MAX_ROOT_ENTRIES = 256
# The eviction record keeps the newest rows only; one JSON line each.
EVICTION_RECORDS_KEPT = 500
DERIVED_ENTRIES = {
    'debug', 'release', 'tests', 'tmp', 'doc', 'CACHEDIR.TAG', '.rustc_info.json',
    '.secode-worktree', '.secode-admission', 'checkpoint-tests', RELEASED, FINISHED,
}


class DiskAdmissionRefused(RuntimeError):
    """A temporary capacity refusal; no new Cargo command was admitted."""


def effective_cap(total, free, cap):
    return min(cap, max(0, total + free - FREE_SPACE_FLOOR_BYTES - BUILD_RESERVE_BYTES))


def queued_targets(owners, deadline, proc, queue=None):
    # Queued protection is best effort: only a provable checkout adds protection.
    # Live-process, lease, pin and recent-use guards remain independent.
    try:
        descriptor = os.open(queue if queue is not None else HEAVY_QUEUE,
                             os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'rb') as stream:
            if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                return set()
            raw = stream.read(1024 * 1024 + 1)
    except FileNotFoundError:
        return set()
    except OSError:
        return set()
    try:
        if len(raw) > 1024 * 1024:
            return set()
        state = json.loads(raw)
        rows = state['tickets']
        if state['version'] != 1 or not isinstance(rows, list) or len(rows) > 1024:
            return set()
        protected = set()
        for row in rows:
            remaining(deadline)
            try:
                pid, start = row['pid'], row['start']
                if type(pid) is not int or pid <= 0 or not isinstance(start, str) or len(start) > 100:
                    continue
                identity = process_identity(pid, cargo_only=False, proc=proc)
                if identity is None or start != f'{identity[2]}:{identity[1]}':
                    continue
                cwd = (Path(proc) / str(pid) / 'cwd').resolve(strict=True)
            except (OSError, ValueError, KeyError, TypeError):
                continue
            matches = {target for target, owner in owners.items()
                       if cwd == owner or owner in cwd.parents}
            if not matches:
                continue
            protected.update(matches)
        return protected
    except (OSError, ValueError, KeyError, TypeError):
        return set()


def remaining(deadline):
    seconds = deadline - time.monotonic()
    if seconds <= 0:
        raise RuntimeError('Target cache maintenance exceeded its time limit')
    return seconds


def bounded_entries(path, limit, deadline=None):
    with os.scandir(path) as entries:
        result = []
        for entry in entries:
            if deadline is not None:
                remaining(deadline)
            if len(result) >= limit:
                raise RuntimeError(f"Cache inventory exceeds {limit} entries: {path}")
            result.append(entry)
        if deadline is not None:
            remaining(deadline)
        return result


def marker_text(marker, limit):
    descriptor = os.open(marker, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise RuntimeError(f'Invalid bounded target marker: {marker}')
        raw = source.read(limit+1)
        after = os.fstat(source.fileno())
        if (len(raw) > limit or (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns)
                != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)):
            raise RuntimeError(f'Target marker changed while reading: {marker}')
    return raw.decode().strip(), info


def checkout_hash(checkout):
    """The cache names a checkout's target directory by the hash of its path."""
    return hashlib.sha256(str(checkout).encode()).hexdigest()


def marker_owner(target):
    value, info = marker_text(target / '.secode-worktree', 4096)
    owner = Path(value)
    if '\0' in value or not owner.is_absolute() or checkout_hash(owner) != target.name:
        raise RuntimeError(f"Target owner does not match its directory: {target}")
    return owner, max(info.st_mtime, target.stat().st_mtime)


def lease(target, shared=False):
    # Locks live outside target dirs, so deletion/recreation cannot replace the lock inode.
    # Builds share the lease: Cargo serializes them on its own lock, and any process that
    # inherited it and outlives its build (a test's survivor, a plugin launcher) must not
    # refuse the next one. Only the evictor takes it exclusively, so a target any build or
    # survivor still holds is never deleted.
    directory = target.parent.parent / '.leases'
    directory.mkdir(exist_ok=True)
    handle = (directory / target.name).open('a')
    try:
        fcntl.flock(handle, (fcntl.LOCK_SH if shared else fcntl.LOCK_EX) | fcntl.LOCK_NB)
    except BaseException:
        handle.close()
        raise
    return handle


def _refuse(sink, reason):
    """Record why a census refused, without changing its None contract.

    A refusal that cannot say why is not evidence of anything. The sink is a list
    owned by the caller; a census that no one asked to explain passes None and
    behaves exactly as before.
    """
    if sink is not None:
        sink.append(reason)


def live_targets(owners, proc, deadline=None, *, owner_cwd=True, refused=None):
    """Protect direct Cargo too; unresolved same-user outputs protect the whole cache."""
    protected = set()
    for entry in bounded_entries(proc, 32768, deadline):
        if deadline is not None:
            remaining(deadline)
        if not entry.name.isdecimal():
            continue
        process = Path(entry.path)
        try:
            if process.stat().st_uid != os.getuid():
                continue
            process_stat = (process / 'stat').read_text()
            if process_stat.rsplit(')', 1)[-1].split()[0] in {'Z', 'X'}:
                if cargo_observation.exited(process, deadline):
                    continue
                return None
            name = (process / 'comm').read_text().strip()
            # Running product/test binaries also retain their build outputs.
            try:
                executable = Path(os.readlink(process / 'exe').removesuffix(' (deleted)'))
                protected.update(target for target in owners if target in executable.parents)
            except PermissionError:
                if name in {'cargo', 'rustc', 'rustdoc'}:
                    return None
            if name not in {'cargo', 'rustc', 'rustdoc'}:
                try:
                    cwd = Path(os.readlink(process / 'cwd').removesuffix(' (deleted)'))
                    if owner_cwd:
                        protected.update(target for target, owner in owners.items()
                                         if cwd == owner or owner in cwd.parents)
                except PermissionError:
                    pass  # Unrelated protected services (e.g. systemd) are not unknown Cargo.
                continue
            with (process / 'environ').open('rb') as handle:
                raw = handle.read(1024 * 1024 + 1)
            if len(raw) > 1024 * 1024:
                _refuse(refused, f'pid {process.name} environ exceeded its read bound')
                return None
            environment = dict(item.split(b'=', 1) for item in raw.split(b'\0') if b'=' in item)
            target = environment.get(b'CARGO_TARGET_DIR', environment.get(b'CARGO_BUILD_TARGET_DIR'))
            with (process / 'cmdline').open('rb') as handle:
                arguments = handle.read(1024 * 1024 + 1).split(b'\0')
            if sum(len(arg) for arg in arguments) > 1024 * 1024:
                _refuse(refused, f'pid {process.name} cmdline exceeded its read bound')
                return None
            explicit = []
            for index, argument in enumerate(arguments):
                if argument.startswith(b'--target-dir='):
                    explicit.append(argument.split(b'=', 1)[1])
                elif argument == b'--target-dir' and index + 1 < len(arguments):
                    explicit.append(arguments[index + 1])
            values = ([target] if target else []) + explicit
            paths = [Path(os.fsdecode(value)) for value in values]
            # Preserve explicit outputs even if a later observation refuses.
            protected.update(path.resolve() for path in paths if path.is_absolute())
            try:
                cwd = (process / 'cwd').resolve(strict=True)
            except FileNotFoundError:
                if not process.exists():
                    continue
                _refuse(refused, f'pid {process.name} cwd did not resolve')
                return None  # A live process with an unresolved cwd has incomplete evidence.
            # trybuild and similar tools launch Cargo from inside their owned output.
            # Protect that enclosing target just as a build in its source checkout.
            matches = [path for path, owner in owners.items()
                       if cwd in (owner, path) or owner in cwd.parents or path in cwd.parents]
            protected.update(matches)
            for path in paths:
                protected.add((path if path.is_absolute() else cwd / path).resolve())
            # A build that names its own output root holds that target; it is
            # what the target is for. Protecting it lets a pass finish for every
            # other target, where a veto stopped all cleanup whenever a build
            # ran. The same root is the only thing both vetoes excuse, so any
            # reference into another target still refuses.
            own, own_token = cargo_observation.own_output(environment, arguments)
            if own is not None:
                # The own output protects itself, and an enclosing target that
                # contains it, because a live writer inside that target is using
                # it. Nothing wider is protected.
                protected.add(own)
                protected.update(path for path in owners if own == path or path in own.parents)
            if not matches:
                import cargo_targets
                observation_deadline = deadline or time.monotonic() + 60
                why = []
                if not cargo_observation.foreign(process, proc,
                                                 cargo_targets.managed_roots(owners),
                                                 observation_deadline, excused=own_token,
                                                 refused=why, own_output=own):
                    detail = '; '.join(why) if why else 'no cause was recorded'
                    _refuse(refused, f'pid {process.name}: {detail}')
                    return None
                if not cargo_targets.foreign(cwd, environment, arguments, owners,
                                             observation_deadline, kind=name,
                                             excused=own):
                    _refuse(refused, f'pid {process.name} ({name}) named a managed root it does '
                                     f'not own')
                    return None
        except (FileNotFoundError, ProcessLookupError):
            if not cargo_observation.exited(process, deadline):
                _refuse(refused, f'pid {process.name} vanished during observation')
                return None
            continue
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            _refuse(refused, f'pid {process.name} was unreadable: '
                             f'{cargo_observation.describe(error)}')
            return None
    if deadline is not None:
        remaining(deadline)
    queued = queued_targets(owners, deadline or time.monotonic() + 5, proc)
    return None if queued is None else protected | queued


def usage(target, deadline):
    # One whole-root invocation deduplicates hardlinks. Never substitute sums
    # of target estimates for this physical allocated-byte observation.
    for attempt in range(2):
        result = subprocess.run(['du', '-sx', '--block-size=1', '--', str(target)],
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                timeout=remaining(deadline))
        remaining(deadline)
        if len(result.stdout) > 8192:
            raise RuntimeError('Cache accounting output exceeds its bound')
        try:
            size = int(result.stdout.split(None, 1)[0])
        except (IndexError, ValueError) as error:
            if result.returncode == 1:
                # Distinguish a vanished root from unreadable or invalid output.
                target.stat()
                if attempt == 0:
                    continue
            raise RuntimeError('Cache accounting returned an invalid size') from error
        if size < 0:
            raise RuntimeError('Cache accounting returned a negative size')
        # du can report a total and exit 1 when rustc removes a walked entry.
        return size


@lru_cache(maxsize=1)
def rust_targets():
    compiler = shutil.which('rustc') or str(Path.home()/'.cargo/bin/rustc')
    # The list is fetched once per run; a loaded box needs a longer bound to
    # start rustc than an idle one, so scale it by the observed load.
    load = os.getloadavg()[0] / max(1, os.cpu_count() or 1)
    timeout = min(240.0, 30.0 * max(1.0, load))
    names = subprocess.check_output([compiler, '--print', 'target-list'],
                                    text=True, timeout=timeout).splitlines()
    if not names or len(names)>1024 or any(len(name)>128 for name in names):
        raise RuntimeError('Invalid bounded Rust target list')
    return frozenset(names)


def disposable(target, deadline, *, retain_qualification=True):
    # Admission keeps qualification records; periodic hygiene can age them out.
    # Unknown top-level content always protects the target.
    allowed = DERIVED_ENTRIES | (set() if retain_qualification else {'.secode-retain', 'gate-logs'})
    for entry in bounded_entries(target, 256, deadline):
        if entry.name not in allowed and not (
                entry.is_dir(follow_symlinks=False) and entry.name in rust_targets()):
            return False
    return True


def latest_modified(target, deadline):
    # Do not rely on du --time: this host's uutils du reports the root mtime
    # even when a nested binary changed. Bound metadata reads independently.
    info = target.stat()
    device = info.st_dev
    modified = info.st_mtime
    pending = [target]
    count = 0
    while pending:
        remaining(deadline)
        path = pending.pop()
        try:
            with os.scandir(path) as entries:
                for entry in entries:
                    remaining(deadline)
                    count += 1
                    if count > MAX_TARGET_ENTRIES:
                        raise RuntimeError('Target modification inventory exceeds its entry bound')
                    try:
                        info = entry.stat(follow_symlinks=False)
                    except FileNotFoundError:
                        continue
                    if info.st_dev != device:
                        continue
                    modified = max(modified, info.st_mtime)
                    if stat.S_ISDIR(info.st_mode):
                        pending.append(Path(entry.path))
        except FileNotFoundError:
            if path == target or not target_missing(path):
                raise
    remaining(deadline)
    return modified


def released(target, owner, modified):
    marker = target / RELEASED
    try:
        info = marker.lstat()
        return (stat.S_ISREG(info.st_mode) and info.st_size <= 4096
                and marker.read_text().strip() == str(owner)
                and modified <= info.st_mtime)
    except (OSError, UnicodeError):
        return False


def target_missing(target):
    # Path.exists() can hide permission and other I/O failures as absence.
    try:
        target.lstat()
    except FileNotFoundError:
        return True
    return False


def finished(target, owner, modified):
    """A landed marker survives checkout retirement, but never a later build."""
    marker = target / FINISHED
    try:
        commit, info = marker_text(marker, 64)
        if modified > info.st_mtime:
            return False
        if not re.fullmatch('[0-9a-f]{40}', commit):
            return False
        if target_missing(owner):
            return True
        actual = subprocess.check_output(['git', '-C', str(owner), 'rev-parse', 'HEAD'],
                                         timeout=10, stderr=subprocess.DEVNULL).decode().strip()
        return actual == commit
    except (OSError, RuntimeError, UnicodeError, subprocess.SubprocessError):
        return False


def record_directory():
    """Where eviction records live: the data root's diagnostics, never the cache.

    Resolved per call so a fixture can point it at its own temporary directory
    without touching the operator's real data root.
    """
    from operator_paths import data_root
    return data_root() / 'diagnostics'


def eviction_record(cache, target, size, reason, removed_at):
    """One durable line per removed target, beside the cache's own diagnostics.

    Attribution would otherwise depend on the evicting job's stderr, which is
    deleted with its scratch directory. This keeps the facts beside the run's own
    durable diagnostics, where they outlive it.

    It is written under the data root, not the cache: `usage` walks the whole
    cache root to enforce the cap, so a record there would be counted as cache
    bytes and grow the very number it records. `disk_hygiene`'s budget rows use
    the same directory for the same reason.

    Recording never fails the eviction. The target is already gone by the time
    this is called, so an unwritable directory is warned about and left alone:
    declining to evict because the log could not be written would be worse than
    the missing line.
    """
    directory = record_directory()
    row = {'removed_unix': removed_at, 'path': str(target), 'bytes': size,
           'reason': reason, 'pid': os.getpid(),
           'admitted_target': os.environ.get('SECODE_CACHE_ADMITTED_TARGET')}
    try:
        directory.mkdir(parents=True, exist_ok=True)
        path = directory / 'evictions.jsonl'
        with path.open('a') as output:
            output.write(json.dumps(row, sort_keys=True) + '\n')
        # Bounded by whole lines, newest kept, so it stays parseable per line.
        # The read-then-rewrite is atomic with respect to other evictors because
        # production calls `evict` inside `admission_lock`; the replacement is
        # still written to a temporary file and renamed, so a reader never sees
        # a half-written log if a future caller skips that lock.
        try:
            lines = path.read_text().splitlines()
        except (OSError, UnicodeError):
            return
        if len(lines) > EVICTION_RECORDS_KEPT:
            kept = '\n'.join(lines[-EVICTION_RECORDS_KEPT:]) + '\n'
            descriptor, temporary = tempfile.mkstemp(prefix='.evictions-', dir=directory)
            try:
                with os.fdopen(descriptor, 'w') as output:
                    output.write(kept)
                os.replace(temporary, path)
            finally:
                Path(temporary).unlink(missing_ok=True)
    except (OSError, ValueError, TypeError) as failure:
        print(f'Eviction record unavailable for {target}: {failure}',
              file=sys.stderr, flush=True)


def evict(cache, current, now=None, cap=None, deadline=None, *, proc):
    cap = CAP_BYTES if cap is None else cap
    if cap <= 0:
        raise RuntimeError('Cache admission budget must be positive')
    now = time.time() if now is None else now
    deadline = time.monotonic() + MAINTENANCE_SECONDS if deadline is None else deadline
    targets = []
    # Count sccache, fixtures, evidence, legacy/unmarked targets and every
    # other root entry, but never consider them deletion candidates.
    bounded_entries(cache, MAX_ROOT_ENTRIES, deadline)
    total = usage(cache, deadline)
    free = shutil.disk_usage(cache.resolve()).free
    limit = effective_cap(total, free, cap)
    target_cache = cache / 'target'
    toolchains = bounded_entries(target_cache, 64, deadline) if target_cache.is_dir() and not target_cache.is_symlink() else []
    for toolchain in toolchains:
        remaining(deadline)
        if not toolchain.is_dir(follow_symlinks=False):
            continue
        worktrees = Path(toolchain.path) / 'worktrees'
        if worktrees.is_symlink() or not worktrees.is_dir():
            continue
        for entry in bounded_entries(worktrees, 256, deadline):
            remaining(deadline)
            if not re.fullmatch('[0-9a-f]{64}', entry.name) or not entry.is_dir(follow_symlinks=False):
                continue
            target = Path(entry.path)
            try:
                try:
                    owner, used = marker_owner(target)
                except (FileNotFoundError, RuntimeError, UnicodeError):
                    # Count unmarked outputs against pressure, but do not guess ownership.
                    owner, used = None, target.stat().st_mtime
                size = usage(target, deadline)
                protected_content = owner is None or not disposable(target, deadline)
                modified = used if protected_content or (now - used < RECENT_SECONDS and not (target / RELEASED).exists()) else latest_modified(target, deadline)
                target.lstat()
                targets.append((target, owner, max(used, modified), size))
            except FileNotFoundError:
                if not target_missing(target):
                    raise
    owners = {target: owner for target, owner, _, _ in targets if owner is not None}
    protected = live_targets(owners, proc, deadline=deadline)
    remaining(deadline)
    managed_estimate = sum(size for _, _, _, size in targets)
    removed = []
    if protected is not None:
        for target, owner, used, size in sorted(targets, key=lambda row: (row[2], str(row[0]))):
            remaining(deadline)
            try:
                if owner is None or target == current:
                    continue
                if target in protected:
                    if total > limit:
                        import duplicate_targets
                        if duplicate_targets.prune_owned(target, owner, owners, deadline, now,
                                                          proc=proc):
                            total = usage(cache, deadline)
                            free = shutil.disk_usage(cache.resolve()).free
                            limit = effective_cap(total, free, cap)
                    continue
                if not disposable(target, deadline):
                    continue
                landed = finished(target, owner, used)
                if now - used < RECENT_SECONDS and not landed:
                    continue
                try:
                    owner.stat()
                    owner_present = True
                except FileNotFoundError:
                    owner_present = False
                if owner_present and now - used < STALE_SECONDS and total <= limit and not landed and not released(target, owner, used):
                    continue
                try:
                    held = lease(target)
                except BlockingIOError:
                    continue
            except FileNotFoundError:
                if not target_missing(target):
                    raise
                continue
            try:
                try:
                    # A build may have finished since inventory. Recheck after
                    # taking its lease before considering this target disposable.
                    _, latest_use = marker_owner(target)
                    modified = latest_modified(target, deadline)
                    if ((now - max(latest_use, modified) < RECENT_SECONDS
                         and not finished(target, owner, max(latest_use, modified)))
                            or not disposable(target, deadline)):
                        continue
                except FileNotFoundError:
                    if not target_missing(target):
                        raise
                    continue
                # A legacy Cargo may have started during the disk inventory.
                active = live_targets(owners, proc, deadline=deadline)
                remaining(deadline)
                if active is None or target in active:
                    continue
                import orphan_cache
                references = orphan_cache.active(owners, deadline, proc)
                if references is None or target in references:
                    continue
                subprocess.run(['rm', '-rf', '--one-file-system', '--', str(target)],
                               check=True, timeout=remaining(deadline))
                if target.exists():
                    raise RuntimeError(f'Target was not fully removed: {target}')
                removed.append(target)
                # After the removal succeeded, so the line describes a real
                # eviction; a failure here must not undo or fail it.
                if landed:
                    reason = 'landed'
                elif total > limit:
                    reason = 'over-budget'
                else:
                    reason = 'stale'
                eviction_record(cache, target, size, reason, time.time())
                # Hardlinks can survive outside the removed target. Only a
                # fresh whole-root observation establishes actual reduction.
                total = usage(cache, deadline)
                free = shutil.disk_usage(cache.resolve()).free
                limit = effective_cap(total, free, cap)
                print(f'Evicted build target: {target}', file=sys.stderr, flush=True)
            finally:
                held.close()
    remaining(deadline)
    total = usage(cache, deadline)
    free = shutil.disk_usage(cache.resolve()).free
    limit = effective_cap(total, free, cap)
    print(f'Whole cache: {total} allocated bytes / {limit} byte effective cleanup target '
          f'(configured {cap}); available disk: {free} bytes; '
          f'{len(removed)} targets evicted. Managed-target estimates: {managed_estimate} bytes '
          '(not the whole cache or physical reclaim).', file=sys.stderr, flush=True)
    if free < FREE_SPACE_FLOOR_BYTES + BUILD_RESERVE_BYTES:
        raise DiskAdmissionRefused(
            f'DISK-ADMISSION refusal (not a test failure): Cache admission refused; '
            f'C={total}, F={free}, floor={FREE_SPACE_FLOOR_BYTES}, reserve={BUILD_RESERVE_BYTES}, '
            f'effective_cap={limit}, configured_cap={cap}, whole_cache_bytes_remaining={total}. '
            'The remaining whole-cache bytes include protected, unknown and unmanaged content; '
            'per-target estimates can overlap through hardlinks. '
            'No running build was stopped and no new Cargo command was admitted. '
            'Report these numbers for safe reclamation or capacity; do not lower the floor or reserve.')
    if total > cap:
        print(f'WARNING: cache remains above its cleanup target: {total} / {cap} bytes; '
              'protected, unknown and unmanaged content is preserved. '
              'Free disk satisfies the floor and reserve; continuing build admission.',
              file=sys.stderr, flush=True)
    return removed, total


def write_owner(path, owner):
    path.write_text(str(owner) + '\n')


def publish_owner(marker, owner):
    """Create the owner marker whole or not at all. Builds share the lease, so two first
    registrations can overlap: the owner is staged beside the marker and linked into place,
    which never overwrites, so a reader sees no marker or the complete owner."""
    descriptor, staged = tempfile.mkstemp(dir=marker.parent, prefix=f'{marker.name}.')
    os.close(descriptor)
    staged = Path(staged)
    try:
        write_owner(staged, owner)
        try:
            os.link(staged, marker)
        except FileExistsError:
            pass  # An overlapping registration published first; the caller validates it.
    finally:
        staged.unlink(missing_ok=True)


def register(target_root, target, root):
    """Claim the checkout target without sweeping other builds' caches."""
    cache = target_root.parent.resolve()
    target = target.resolve()
    expected = target_root.resolve() / 'worktrees' / hashlib.sha256(str(root.resolve()).encode()).hexdigest()
    if target != expected:
        raise RuntimeError('Target cache maintenance refuses a target outside this checkout')
    cache.mkdir(parents=True, exist_ok=True)
    target.parent.mkdir(parents=True, exist_ok=True)
    held = lease(target, shared=True)
    try:
        target.mkdir(exist_ok=True)
        marker = target / '.secode-worktree'
        if not (marker.exists() or marker.is_symlink()):
            publish_owner(marker, root.resolve())
        owner, _ = marker_owner(target)
        if owner != root.resolve():
            raise RuntimeError('Build target belongs to another checkout')
        (target / RELEASED).unlink(missing_ok=True)
        # Ownership stays immutable while other registrants read it.
        # marker_owner already includes directory mtime in recent-use protection.
        os.utime(target, None)
        return held
    except BaseException:
        held.close()
        raise


def admission_lock(cache, deadline):
    handle = (cache / '.eviction.lock').open('a')
    try:
        # flock performs the bounded wait without a polling loop. Its child and
        # this process share the open description; the lock lasts until close.
        subprocess.run(['flock', '--exclusive', '--wait', str(remaining(deadline)),
                        str(handle.fileno())], pass_fds=(handle.fileno(),),
                       check=True, timeout=remaining(deadline))
        return handle
    except BaseException:
        handle.close()
        raise


def boot_id(proc):
    """The boot ID under the census root, never the host's.

    A fixture supplies its own `sys/kernel/random/boot_id`, so process identity
    is proved entirely from injected state rather than borrowed from the host.

    A missing or blank boot ID is a refusal, not an empty string. An empty value
    would let two processes from different boots match a stored identity, and
    `prepare` reuses an admission record before rechecking capacity, so that
    identity would skip the capacity check it was meant to gate. The missing
    file propagates as it always did; a blank one raises the same way.
    """
    path = Path(proc) / 'sys/kernel/random/boot_id'
    value = path.read_text().strip()
    if not value:
        raise ValueError(f'Boot identity is blank: {path}')
    return value


def process_identity(pid, cargo_only=True, *, proc):
    # An identity is three components -- the pid, its start time and the boot
    # that produced it -- and one that is missing a part is not a weaker
    # identity, it is none at all. Anything incomplete refuses here, because
    # prepare reuses a matching admission record before it rechecks capacity: a
    # blank component would let a process from another boot match a stored
    # record and skip the capacity check it was meant to gate.
    #
    # proc is required with no default: a census must never read the host's table
    # by omission, and the boot ID comes from the same root so a fixture owns its
    # whole identity rather than borrowing part of it.
    if type(pid) is not int or pid <= 0:
        raise ValueError('Process identity has no pid')
    process = Path(proc) / str(pid)
    if not process.is_dir():
        raise FileNotFoundError(f'No such process: {process}')
    if cargo_only and (process / 'comm').read_text().strip() != 'cargo':
        return None  # A direct wrapper call has no Cargo lifetime to reuse.
    raw = (process / 'stat').read_text()
    if len(raw) > 8192:
        raise RuntimeError('Oversized parent process identity')
    fields = raw.rsplit(')', 1)[1].split()
    if fields[0] == 'Z':
        return None
    if len(fields) <= 19:
        raise ValueError('Process stat carries no start time')
    start = fields[19]
    if not start:
        raise ValueError('Process start time is blank')
    return [pid, start, boot_id(proc)]


def _complete_identity(identity):
    """True only when an identity carries a pid, a start time and a boot ID.

    The three parts are what make an identity worth reusing: the pid alone is
    reused by the kernel, the start time separates two lifetimes of one pid, and
    the boot ID separates them across restarts. A blank or missing part proves
    nothing, so it is refused rather than compared.
    """
    if not isinstance(identity, list) or len(identity) != 3:
        return False
    pid, start, boot = identity
    return (type(pid) is int and pid > 0
            and isinstance(start, str) and bool(start.strip())
            and isinstance(boot, str) and bool(boot.strip()))


def prepare(target_root, target, root, identity=None, *, proc):
    import ram_targets
    ram = ram_targets.validate(target, root)
    if ram and not os.environ.get('SECODE_RAM_DISK_CACHE'):
        raise RuntimeError('RAM gate disk-cache provenance is missing')
    # An admission record is reused on identity match alone, before capacity is
    # rechecked, so an identity missing a component must never be allowed to
    # match. Reject it here rather than trusting the caller or the record.
    if identity is not None and not _complete_identity(identity):
        raise ValueError(f'Admission identity is incomplete: {identity!r}')
    cache = (Path(os.environ['SECODE_RAM_DISK_CACHE']) if ram
             else target_root.parent.parent).resolve()
    held = register(target_root, target, root)
    deadline = time.monotonic() + MAINTENANCE_SECONDS
    try:
        (target / '.secode-finished').unlink(missing_ok=True)
        with admission_lock(cache, deadline):
            target = target.resolve()
            record = target / '.secode-admission'
            if identity is not None:
                try:
                    info = record.lstat()
                    previous = json.loads(record.read_text()) if stat.S_ISREG(info.st_mode) and info.st_size <= 4096 else None
                except (OSError, ValueError):
                    previous = None
                if previous == {'process': identity, 'budget': CAP_BYTES, 'admitted': True}:
                    return held
                # Allocate the record before measurement, including its bytes in
                # the admission observation. Failed observations stay pending.
                record.unlink(missing_ok=True)
                record.write_text(json.dumps({'process': identity, 'budget': CAP_BYTES, 'admitted': False}))
            evict(cache, target, deadline=deadline, proc=proc)
            if identity is not None:
                record.write_text(json.dumps({'process': identity, 'budget': CAP_BYTES, 'admitted': True}))
        return held
    except BaseException:
        held.close()
        raise


def inherit(held, target, environment=None, *, proc):
    environment = os.environ if environment is None else environment
    os.set_inheritable(held.fileno(), True)
    environment['SECODE_CACHE_LEASE_FD'] = str(held.fileno())
    environment['SECODE_CACHE_ADMITTED_TARGET'] = str(target.resolve())
    environment['SECODE_CACHE_ADMISSION_PROCESS'] = json.dumps(
        process_identity(os.getpid(), cargo_only=False, proc=proc))


def inherited_lease(target, *, proc):
    if os.environ.get('SECODE_CACHE_ADMITTED_TARGET') != str(target.resolve()):
        return None
    try:
        raw = os.environ.get('SECODE_CACHE_ADMISSION_PROCESS', '')
        if len(raw) > 4096:
            return None
        origin = json.loads(raw)
        if (not isinstance(origin, list) or len(origin) != 3
                or process_identity(origin[0], cargo_only=False, proc=proc) != origin):
            return None
        descriptor = int(os.environ.get('SECODE_CACHE_LEASE_FD', '-1'))
        info = os.fstat(descriptor)
        path = target.parent.parent / '.leases' / target.name
        expected = path.stat()
        if (info.st_dev, info.st_ino) != (expected.st_dev, expected.st_ino):
            return None
        held = os.fdopen(os.dup(descriptor), 'a')
        try:
            fcntl.flock(held, fcntl.LOCK_SH | fcntl.LOCK_NB)
            return held
        except BaseException:
            held.close()
            raise
    except (OSError, ValueError, TypeError, IndexError):
        return None


def retain(target, root, retained=True):
    owner, _ = marker_owner(target)
    if owner != root.resolve():
        raise RuntimeError('Retention pin belongs to another checkout')
    marker = target / '.secode-retain'
    if retained:
        marker.unlink(missing_ok=True)
        marker.write_text(str(root.resolve()) + '\n')
    else:
        marker.unlink(missing_ok=True)
        release = target / RELEASED
        release.unlink(missing_ok=True)
        release.write_text(str(root.resolve()) + '\n')


def main():
    if 'SECODE_TARGET_ROOT' not in os.environ:
        raise SystemExit('This command needs the managed target root: source scripts/shared-cargo.sh '
                         'or a cache-layout shell, or run from a job whose target is inside the build cache.')
    target_root = Path(os.environ['SECODE_TARGET_ROOT'])
    target = Path(os.environ.get('CARGO_TARGET_DIR') or os.environ.get('SECODE_WORKTREE_TARGET', target_root))
    root = Path(os.environ['SECODE_WORKTREE_ROOT'])
    arguments = sys.argv[1:]
    if arguments in (['--retain'], ['--release']):
        marker_owner(target)  # Pins apply to existing owned outputs only.
        held = register(target_root, target, root)
        try:
            retain(target, root, arguments == ['--retain'])
        finally:
            held.close()
        return
    compiler = arguments[:1] == ['--compiler']
    held = inherited_lease(target, proc=Path('/proc')) if compiler else None
    if held is None:
        held = prepare(target_root, target, root,
                       process_identity(os.getppid(), proc=Path('/proc')) if compiler else None,
                       proc=Path('/proc'))
    if arguments == ['--register']:
        held.close()
        return
    inherit(held, target, proc=Path('/proc'))
    if compiler:
        os.execvp(arguments[1], arguments[1:])
    os.execvp('cargo', ['cargo', *arguments])


if __name__ == '__main__':
    try:
        main()
    except DiskAdmissionRefused as error:
        print(error, file=sys.stderr)
        sys.exit(75)
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        print(f'Cache admission refused: {error}. No compiler/Cargo command started; '
              'check whole-cache capacity, ownership and the maintenance lock.', file=sys.stderr)
        sys.exit(1)
