#!/usr/bin/env python3
"""Retain SE Code scratch for seven days and archive nominated idle worktrees."""
from operator_paths import data_root, worktrees
import argparse
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import time

import evict_targets as targets
import orphan_cache
import target_discovery

RETENTION_SECONDS = 7 * 86400
ROOTS = (data_root()/'diagnostics', data_root()/'scratch', Path('/tmp'))
# A process census reads every own process's kernel sources and matches them
# against every candidate, so it is the dominant cost of a pass (~32 s over
# 2,200 candidates on the workstation, 2026-11-20): one census answers the
# filtering for every candidate it covers until it is this old. Sixty seconds
# bounds how stale the filter may be while a full run pays for only a handful
# of censuses; removals are gated by fresh single-path censuses instead.
CENSUS_MAX_AGE_SECONDS = 60
# Consecutive runs that remove nothing before a non-zero exit asks for a human.
ZERO_PROGRESS_ESCALATION_RUNS = 3


class CensusUnavailable(RuntimeError):
    """A blind process census: every candidate is held and the pass refuses.

    A blind observation is a refusal, never a budget condition: it must reach
    main and exit 1 even when the deadline happens to be spent at the same
    moment."""


class Census:
    """Hold one process-activity census for one candidate filtering pass.

    orphan_cache.active reads every own process's kernel sources, which the old
    loop paid once per candidate tree. One Census is created per pass over one
    fixed cohort of candidates; it holds a single fresh census and re-serves it
    until it is CENSUS_MAX_AGE_SECONDS old, so a long pass pays for only a
    handful of censuses. It only FILTERS candidates: every removal is
    additionally gated by a fresh single-path census taken right before it, so
    no rm relies on a filtered observation. A blind census still refuses: the
    query that takes it raises, exactly as an uncached blind census did, and a
    raise is never cached.
    """

    def __init__(self, deadline, proc):
        self._deadline = deadline
        self._proc = proc
        self._taken = None
        self._free = None

    def unused(self, paths):
        # unused() itself returns the free set and raises on a blind census,
        # so a held entry is always a usable set.
        if self._taken is None or time.monotonic() - self._taken > CENSUS_MAX_AGE_SECONDS:
            self._free = unused(paths, self._deadline, self._proc)
            self._taken = time.monotonic()
        return self._free


def git(root, *arguments):
    value = subprocess.check_output(['git', '-C', str(root), *arguments], text=True, timeout=120).strip()
    if len(value) > 1024 * 1024:
        raise ValueError('Retention Git output exceeds one MiB')
    return value


def mounts(table=Path('/proc/self/mountinfo')):
    with table.open('rb') as stream:
        raw = stream.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise ValueError('Mount inventory exceeds one MiB')
    result = set()
    for line in raw.splitlines():
        fields = line.split()
        if len(fields) < 10 or b'-' not in fields:
            raise ValueError('Malformed mount inventory')
        name = re.sub(rb'\\([0-7]{3})', lambda match: bytes([int(match[1], 8)]), fields[4])
        path = Path(os.fsdecode(name))
        if not path.is_absolute():
            raise ValueError('Mount inventory contains a relative path')
        result.add(path)
    return result


def dirty(path):
    # Ignored content can include local credentials and scratch outside Git.
    # It cannot be recovered from a bundle, so it must prevent source removal.
    return git(path, 'status', '--porcelain', '--untracked-files=all', '--ignored=matching')


def idle(path, now, deadline, *, scratch=True):
    """Never follow mounts/links or expire a retained directory or repository."""
    original = path.lstat()
    mounted = mounts()
    if path in mounted or any(path in candidate.parents for candidate in mounted):
        return False
    if original.st_uid != os.getuid():
        return False
    if stat.S_ISREG(original.st_mode):
        return path.name != '.secode-retain' and now - original.st_mtime >= RETENTION_SECONDS
    if not stat.S_ISDIR(original.st_mode):
        return False
    pending = [path]
    count = 0
    while pending:
        directory = pending.pop()
        if scratch and (directory / 'HEAD').is_file() and (directory / 'objects').is_dir():
            return False
        for entry in targets.bounded_entries(directory, targets.MAX_TARGET_ENTRIES, deadline):
            count += 1
            if count > targets.MAX_TARGET_ENTRIES:
                raise RuntimeError('Retention inventory exceeds its entry bound')
            if entry.name == '.secode-retain' or scratch and entry.name == '.git':
                return False
            if not scratch and entry.name in ('.git', 'target'):
                continue
            info = entry.stat(follow_symlinks=False)
            if info.st_dev != original.st_dev or now - info.st_mtime < RETENTION_SECONDS:
                return False
            if stat.S_ISDIR(info.st_mode):
                pending.append(Path(entry.path))
    return now - original.st_mtime >= RETENTION_SECONDS


def unused(paths, deadline, proc):
    observed = orphan_cache.active(paths, deadline, proc)
    if observed is None:
        raise CensusUnavailable('Process activity is unavailable; preserve retention candidates')
    return set(paths) - observed


def tree_bytes(path, deadline):
    """Bounded apparent size of one tree, taken only from a tree about to be
    removed, and never reported: apparent length over-counts sparse and
    hard-linked data, so the run's statvfs delta is the disk figure.

    Never follows mounts or symlinks and sums only the tree's own device, so
    the walk also proves the tree is enumerable under the entry bound before
    an rm -rf. The candidate scan never calls this: sizing every candidate
    could spend the whole budget before the first removal."""
    original = path.lstat()
    if stat.S_ISREG(original.st_mode):
        return original.st_size
    total = 0
    pending = [path]
    count = 0
    while pending:
        directory = pending.pop()
        for entry in targets.bounded_entries(directory, targets.MAX_TARGET_ENTRIES, deadline):
            count += 1
            if count > targets.MAX_TARGET_ENTRIES:
                raise RuntimeError('Retention inventory exceeds its entry bound')
            info = entry.stat(follow_symlinks=False)
            if info.st_dev != original.st_dev:
                continue
            if stat.S_ISREG(info.st_mode):
                total += info.st_size
            elif stat.S_ISDIR(info.st_mode):
                pending.append(Path(entry.path))
    return total


def scratch_candidates(now, deadline, roots=ROOTS):
    """Expired scratch paths, unsized: the scan must not pay a walk per tree."""
    found = []
    for root in roots:
        if not root.exists():
            continue
        if root.is_symlink():
            raise ValueError('Retention roots must be real directories')
        if (root / '.secode-retain').exists():
            continue
        for entry in targets.bounded_entries(root, 16384, deadline):
            if root == Path('/tmp') and not entry.name.startswith(('secode-gate-', 'secode-nightly-')):
                continue
            path = Path(entry.path)
            if (entry.is_dir(follow_symlinks=False)
                    or root != Path('/tmp') and entry.is_file(follow_symlinks=False)) and idle(path, now, deadline):
                found.append(path)
    return found


def reclaim_scratch(now, deadline, apply=False, roots=ROOTS, *, proc):
    """Remove expired scratch under the shared budget.

    Returns (removed, remaining, exhausted, scan_complete). removed is one
    path per removed (or planned, with apply=False) candidate. remaining is
    one unsized path per candidate still present afterwards. exhausted is
    True where the time budget ran out mid-pass, scan_complete is False where
    the enumeration itself ran out and the remaining count is a lower bound.
    A pass that spends its budget stops at the next candidate boundary and
    reports what remains instead of failing the whole run. The run's disk
    figure is main's statvfs free-space delta, never a per-tree total:
    apparent lengths over-count sparse and hard-linked data.

    Safety: the cached census only FILTERS candidates. Every removal pays its
    slow checks (idle walk, bounded sizing walk) first, then a fresh
    single-path census, then the fast retain-marker and lstat pin checks,
    then the rm, so nothing slow runs after the observation it relies on. A
    blind census refuses (CensusUnavailable, exit 1 through main), even at
    the deadline; a plan never pays the removal gate, because it removes
    nothing.
    """
    census = Census(deadline, proc)
    candidates = []
    removed = []
    exhausted = False
    scan_complete = False
    try:
        candidates = scratch_candidates(now, deadline, roots)
        scan_complete = True
        for path in sorted(candidates):
            if path not in census.unused(candidates):
                continue
            before = path.lstat()
            if path not in census.unused(candidates) or not idle(path, now, deadline):
                continue
            after = path.lstat()
            if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
                continue
            if apply:
                # Slow work first: the sizing walk proves the tree is
                # enumerable under the entry bound before an rm -rf; its
                # apparent size is not reported (statvfs delta, in main).
                tree_bytes(path, deadline)
                # The removal gate: a fresh single-path census, never the
                # cached filter, because a process can take a reference any
                # moment after the filter ran.
                if path not in unused([path], deadline, proc):
                    continue
                # A retain marker can appear while the census runs; it is
                # fast, so it is re-checked after the census.
                if os.path.lexists(path / '.secode-retain'):
                    continue
                try:
                    after = path.lstat()
                except OSError:
                    continue
                if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
                    continue
                if stat.S_ISREG(after.st_mode):
                    path.unlink()
                else:
                    subprocess.run(['rm', '-rf', '--one-file-system', '--', str(path)],
                                   check=True, timeout=targets.remaining(deadline))
                removed.append(path)
                # The outcome log comes only after the removal: nothing
                # blocking may sit between the final gate check and the rm.
                print(f'Removed expired scratch {path}', flush=True)
            else:
                print(f'Would remove expired scratch {path}', flush=True)
                removed.append(path)
    except CensusUnavailable:
        # A blind observation is a refusal, never a budget condition: it must
        # reach main and exit 1 even when the deadline is spent at the same
        # moment.
        raise
    except RuntimeError:
        # The other deadline-bound failure is the time limit itself; any other
        # RuntimeError while time remains is a real error and still aborts.
        if time.monotonic() < deadline:
            raise
        exhausted = True
    removed_set = set(removed)
    remaining = [path for path in candidates if path not in removed_set]
    return removed, remaining, exhausted, scan_complete


def worktree_paths(config):
    if config is None:
        return []
    if config.stat().st_size > 65536:
        raise ValueError('Worktree retention configuration exceeds 64 KiB')
    value = json.loads(config.read_text())
    if (not isinstance(value, dict) or set(value) != {'worktrees'}
            or not isinstance(value['worktrees'], list) or len(value['worktrees']) > 256):
        raise ValueError('Expected a bounded explicit worktrees list')
    paths = []
    for item in value['worktrees']:
        path = Path(item)
        if (not path.is_absolute() or path.is_symlink()
                or path.parent not in (worktrees(), Path.home())):
            raise ValueError('Nominate exact SE Code worktrees under home or the NVMe worktrees directory')
        paths.append(path)
    if len(set(paths)) != len(paths):
        raise ValueError('Duplicate worktree retention nomination')
    return paths


def archive_worktree(path, archives, now, deadline, apply=False, *, proc):
    # The mixed worktrees parent is never enumerated or recursively removed.
    census = Census(deadline, proc)
    if not path.exists():
        return
    if not target_discovery.secode_checkout(path) or path.is_symlink():
        raise ValueError(f'Not a real SE Code Rust checkout: {path}')
    if dirty(path):
        print(f'Preserved dirty worktree {path}', flush=True)
        return
    if not idle(path, now, deadline, scratch=False):
        return
    gitdir = Path(git(path, 'rev-parse', '--absolute-git-dir'))
    for name in ('HEAD', 'index', 'logs/HEAD'):
        marker = gitdir / name
        if marker.exists() and now - marker.stat().st_mtime < RETENTION_SECONDS:
            return
    common = Path(git(path, 'rev-parse', '--path-format=absolute', '--git-common-dir'))
    registered = [Path(line[9:]) for line in git(path, 'worktree', 'list', '--porcelain').splitlines()
                  if line.startswith('worktree ')]
    if any(path in other.parents for other in registered if other != path):
        raise ValueError('Preserve checkout containing another registered worktree')
    if (path / '.git').is_dir():
        # An unrelated clone may borrow this object database through alternates.
        print(f'Preserved standalone object database {path}', flush=True)
        return
    original = path.lstat()
    if path not in census.unused([path]):
        return
    if not apply:
        print(f'Would bundle all refs and remove idle SE Code worktree {path}', flush=True)
        return
    archives.mkdir(parents=True, exist_ok=True)
    # Persist newly created archive parents before depending on their entries.
    for directory in (archives, *archives.parents):
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    commit = git(path, 'rev-parse', 'HEAD')
    refs = git(path, 'for-each-ref', '--format=%(objectname) %(refname)').splitlines()
    archive = archives / f'{path.name}-{stamp}-{commit[:12]}.bundle'
    # Include detached HEAD as well as every ref, regardless of main's location.
    # No prerequisites: this bundle must restore into an empty repository.
    with tempfile.TemporaryDirectory(prefix='.bundle-', dir=archives) as temporary:
        bundle = Path(temporary) / 'worktree.bundle'
        git(path, 'bundle', 'create', str(bundle), '--all', 'HEAD')
        check = Path(temporary) / 'verify.git'
        subprocess.run(['git', 'init', '--bare', '-q', str(check)], check=True,
                       timeout=targets.remaining(deadline))
        git(check, 'bundle', 'verify', str(bundle))
        heads = git(check, 'bundle', 'list-heads', str(bundle)).splitlines()
        if not {*refs, f'{commit} HEAD'} <= set(heads):
            raise RuntimeError('Archive does not retain the saved refs and worktree HEAD')
        # Header verification does not read the object pack. Actually import it,
        # check its full object graph, then resolve every advertised object.
        git(check, 'bundle', 'unbundle', str(bundle))
        git(check, 'fsck', '--full', '--strict', '--no-reflogs')
        for head in heads:
            git(check, 'cat-file', '-e', head.split(' ', 1)[0] + '^{object}')
        git(check, 'cat-file', '-e', commit + '^{commit}')
        with bundle.open('rb') as stream:
            os.fsync(stream.fileno())
        os.link(bundle, archive)  # Atomic publication that refuses an existing name.
        descriptor = os.open(archives, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    # Tight removal gate: the slow checks first, then the fresh single-path
    # census, then the fast checks, so nothing slow runs after the
    # observation the removal relies on.
    if (git(path, 'rev-parse', 'HEAD') != commit
            or dirty(path)
            or not idle(path, now, deadline, scratch=False)):
        raise RuntimeError(f'Worktree became active; preserved source and archive {archive}')
    if path not in unused([path], deadline, proc):
        raise RuntimeError(f'Worktree became active; preserved source and archive {archive}')
    if os.path.lexists(path / '.secode-retain'):
        raise RuntimeError(f'Worktree retained; preserved source and archive {archive}')
    current = path.lstat()
    if (not stat.S_ISDIR(current.st_mode)
            or (original.st_dev, original.st_ino) != (current.st_dev, current.st_ino)):
        raise RuntimeError(f'Worktree became active; preserved source and archive {archive}')
    git(common, 'worktree', 'remove', str(path))
    print(f'Archived {path} to {archive} and removed its idle checkout', flush=True)


def load_progress(state):
    """The zero-removal streak; a missing or corrupt file means no evidence."""
    try:
        value = json.loads((state / 'retention-progress.json').read_text())
    except (OSError, ValueError):
        return {'zero_progress_runs': 0}
    if (not isinstance(value, dict)
            or type(value.get('zero_progress_runs')) is not int
            or not 0 <= value['zero_progress_runs'] <= 1_000_000):
        return {'zero_progress_runs': 0}
    return value


def save_progress(state, progress):
    temporary = (state / 'retention-progress.json').with_name('retention-progress.json.tmp')
    temporary.write_text(json.dumps(progress, sort_keys=True) + '\n')
    os.replace(temporary, state / 'retention-progress.json')


def record_progress(state, apply, removed, remaining, scan_complete, disk_free=0):
    """Write the run's honest marker and decide the exit code.

    The marker's disk figure is the run's statvfs free-space delta (disk_free),
    because apparent per-tree lengths over-count sparse and hard-linked data;
    a plan removes nothing, so it claims no disk change. The remaining backlog
    is counted in trees and never sized, because sizing it is what the budget
    exists for. Progress or a drained backlog exits 0. Only K consecutive runs
    that removed nothing exit non-zero (2) to demand attention; a real error
    already exits 1 through main's handler. A plan (apply=False) reports but
    never touches the streak of real maintenance runs.
    """
    verb = 'removed' if apply else 'would remove'
    disk_text = f', disk free {disk_free / 10**9:+.1f} GB' if apply else ''
    if not scan_complete:
        print(f'backlog remains: uncounted (enumeration exceeded the time budget), '
              f'{verb} {len(removed)} this run{disk_text}', flush=True)
    elif remaining:
        print(f'backlog remains: {len(remaining)} trees, '
              f'{verb} {len(removed)} this run{disk_text}', flush=True)
    if not apply:
        return
    progress = load_progress(state)
    # An uncounted backlog is unknown, not drained: it counts as zero progress.
    backlog = bool(remaining) or not scan_complete
    streak = 0 if removed else (progress['zero_progress_runs'] + 1 if backlog else 0)
    progress.update({
        'version': 1,
        'zero_progress_runs': streak,
        'last_removed': len(removed),
        'last_remaining_trees': len(remaining),
        'last_disk_free_bytes': int(disk_free),
        'updated': datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
    })
    save_progress(state, progress)
    if not removed and backlog and streak >= ZERO_PROGRESS_ESCALATION_RUNS:
        state_text = (f'{len(remaining)} trees remain' if scan_complete
                      else 'the backlog is uncounted')
        print(f'ALERT: retention removed nothing in {streak} consecutive runs; '
              f'{state_text}. Inspect the scratch roots and process holdings.',
              file=sys.stderr, flush=True)
        raise SystemExit(2)


def filesystem_free_bytes(paths):
    """Available bytes per filesystem holding any of these paths.

    The before/after delta of this map is the run's honest disk figure:
    apparent file lengths over-count sparse and hard-linked data. A path
    that vanished contributes nothing on the sample that misses it, so a
    device counts only when it is present on both sides of the pass.
    """
    free = {}
    for path in paths:
        try:
            device = path.stat().st_dev
        except OSError:
            continue
        if device in free:
            continue
        try:
            observation = os.statvfs(path)
            free[device] = observation.f_bavail * observation.f_frsize
        except OSError:
            continue
    return free


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true', help='default is read-only planning')
    parser.add_argument('--worktrees', type=Path, help='explicit SE Code-only nomination JSON; no discovery')
    parser.add_argument('--archives', type=Path, default=data_root()/'archives/worktrees')
    args = parser.parse_args()
    paths = worktree_paths(args.worktrees)
    archive_root = args.archives.resolve()
    if any(archive_root == root or root in archive_root.parents for root in (*ROOTS, *paths)):
        raise ValueError('Keep bundles outside all expired scratch and worktrees')
    state = data_root()/'state'
    state.mkdir(parents=True, exist_ok=True)
    with (state / 'retention.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        deadline = time.monotonic() + 900
        now = time.time()
        free_before = filesystem_free_bytes((*ROOTS, *paths))
        removed, remaining, exhausted, scan_complete = reclaim_scratch(
            now, deadline, args.apply, roots=ROOTS, proc=Path('/proc'))
        for path in paths:
            archive_worktree(path, archive_root, now, deadline, args.apply, proc=Path('/proc'))
        free_after = filesystem_free_bytes((*ROOTS, *paths))
        disk_free = sum(after - free_before[device]
                        for device, after in free_after.items() if device in free_before)
        record_progress(state, args.apply, removed, remaining, scan_complete, disk_free)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        raise SystemExit(f'Retention stopped: {error}')
