"""Prune superseded Cargo executables and incremental build generations."""
from contextlib import contextmanager, ExitStack
import fcntl
import os
from pathlib import Path
import re
import stat
import subprocess

import evict_targets as targets


NAME = re.compile(r'([A-Za-z0-9_]+)-([0-9a-f]{16})(\.exe)?')


def identity(path):
    info = path.lstat()
    if not (stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode)):
        return None
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_mode


def executable(path, info):
    if not info or not stat.S_ISREG(info[4]) or not info[4] & 0o111:
        return False
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        opened = os.fstat(source.fileno())
        if (opened.st_dev, opened.st_ino) != info[:2]:
            raise RuntimeError('Executable changed during duplicate inventory')
        magic = source.read(4)
    return magic == b'\x7fELF' or magic[:2] == b'MZ'


def profiles(target, deadline):
    result = [target / 'debug', target / 'release']
    # One level of target triples, never recursive traversal through arbitrary links.
    for entry in targets.bounded_entries(target, 256, deadline):
        if entry.is_dir(follow_symlinks=False) and re.fullmatch(r'(?:[A-Za-z0-9_]+-){2,}[A-Za-z0-9_]+', entry.name):
            result.extend([Path(entry.path) / 'debug', Path(entry.path) / 'release'])
    return [path for path in result if not path.is_symlink() and path.is_dir()]


@contextmanager
def locked_profiles(target, deadline):
    # Honor Cargo's own profile locks too: direct Cargo does not borrow our lease.
    # Nonblocking acquisition lets builds keep their normal lock ordering.
    with ExitStack() as held:
        try:
            for profile in profiles(target, deadline):
                for name in ('.cargo-lock', '.cargo-build-lock', '.cargo-artifact-lock'):
                    targets.remaining(deadline)
                    descriptor = os.open(profile/name, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
                    stream = held.enter_context(os.fdopen(descriptor, 'a'))
                    if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
                        raise RuntimeError('Cargo profile lock is not a regular file')
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            yield False
            return
        yield True


def candidates(target, deadline):
    result = []
    for profile in profiles(target, deadline):
        directory = profile / 'deps'
        if directory.is_symlink() or not directory.is_dir():
            continue
        groups = {}
        for entry in targets.bounded_entries(directory, targets.MAX_TARGET_ENTRIES, deadline):
            match = NAME.fullmatch(entry.name)
            if match is None or not entry.is_file(follow_symlinks=False):
                continue
            path = Path(entry.path)
            info = identity(path)
            if executable(path, info):
                groups.setdefault((match[1], match[3]), []).append((path, info))
        for group in groups.values():
            ordered = sorted(group, key=lambda item: (item[1][3], item[0].name), reverse=True)
            for path, info in ordered[1:]:
                dependency = path.with_suffix('.d') if path.suffix == '.exe' else Path(str(path) + '.d')
                try:
                    dep_info = identity(dependency)
                except FileNotFoundError:
                    dep_info = None
                result.append((path, info, dependency, dep_info))
        incremental = profile/'incremental'
        if incremental.is_symlink() or not incremental.is_dir():
            continue
        groups = {}
        for entry in targets.bounded_entries(incremental, targets.MAX_TARGET_ENTRIES, deadline):
            match = re.fullmatch(r'([A-Za-z0-9_]+)-[a-z0-9]+', entry.name)
            if match and entry.is_dir(follow_symlinks=False):
                path = Path(entry.path)
                groups.setdefault(match[1], []).append((targets.latest_modified(path, deadline), path))
        # Keep two configurations per crate, plus every recently modified one.
        for group in groups.values():
            for _, path in sorted(group, reverse=True)[2:]:
                info = identity(path)
                if info is not None and stat.S_ISDIR(info[4]):
                    result.append((path, info, None, None))
    return result


def prune(target, deadline, now, dry_run=False):
    with locked_profiles(target, deadline) as locked:
        return prune_locked(target, deadline, now, dry_run) if locked else []


def prune_locked(target, deadline, now, dry_run, inventory=None):
    removed = []
    if inventory is None:
        inventory = candidates(target, deadline)
    for path, before, dependency, dep_before in inventory:
        targets.remaining(deadline)
        directory = stat.S_ISDIR(before[4])
        modified = targets.latest_modified(path, deadline) if directory else before[3]/1_000_000_000
        if now - modified < targets.RECENT_SECONDS:
            continue
        try:
            current = identity(path)
        except FileNotFoundError:
            continue
        if current != before:
            raise RuntimeError('Superseded artifact changed after inventory')
        if dep_before is not None and identity(dependency) != dep_before:
            raise RuntimeError('Duplicate dependency file changed after inventory')
        if dry_run:
            print(f'Would prune duplicate {path}')
        else:
            if directory:
                subprocess.run(['rm', '-rf', '--one-file-system', '--', str(path)],
                               check=True, timeout=targets.remaining(deadline))
                if path.exists():
                    raise RuntimeError('Incremental generation was not completely removed')
            else:
                path.unlink()
            if dep_before is not None:
                dependency.unlink()
            print(f'Pruned duplicate {path}', flush=True)
        removed.append(path)
    return removed


def prune_owned(target, owner, owners, deadline, now, dry_run=False, *, proc):
    """A live owner may keep working while its unused build generations expire."""
    import orphan_cache
    import target_discovery
    if target.is_symlink() or not target.is_dir() or not targets.disposable(
            target, deadline, retain_qualification=False):
        return []
    try:
        held = targets.lease(target)
    except BlockingIOError:
        return []
    try:
        try:
            inventory = candidates(target, deadline)
        except FileNotFoundError:
            return []  # Direct Cargo changed an artifact during the read-only probe.
        if not inventory or not any(now-before[3]/1_000_000_000 >= targets.RECENT_SECONDS
                                    for _, before, _, _ in inventory):
            return []
        # Observe references before opening our own Cargo profile lock files.
        # Owner cwd alone preserves the whole target, while builds and actual
        # artifact readers also prohibit pruning within it.
        builds = targets.live_targets(owners, proc, deadline=deadline, owner_cwd=False)
        references = orphan_cache.active([target], deadline, proc)
        if builds is None or references is None or target in builds or target in references:
            return []
        with locked_profiles(target, deadline) as locked:
            if (not locked or not target_discovery.still_owned(target, owner)
                    or not targets.disposable(target, deadline, retain_qualification=False)):
                return []
            return prune_locked(target, deadline, now, dry_run, inventory)
    finally:
        held.close()
