"""Bounded discovery of SE Code Cargo outputs, including retired layouts."""
from operator_paths import worktrees
import os
from pathlib import Path
import re
import stat
import sys

import evict_targets as targets


def directory(path):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return False
    return stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid()


def secode_checkout(path):
    return (directory(path) and (path / '.git').exists()
            and (path / 'Cargo.toml').is_file()
            and (path / 'crates/grapt-types/Cargo.toml').is_file())


def owner_marker(target):
    value, _ = targets.marker_text(target / '.secode-worktree', 4096)
    if not value or '\0' in value or not Path(value).is_absolute():
        raise RuntimeError('Target owner is not an absolute path')
    return Path(value)


def cargo_target(path):
    if not directory(path):
        return False
    try:
        info = (path / 'CACHEDIR.TAG').lstat()
    except FileNotFoundError:
        return False
    return stat.S_ISREG(info.st_mode) and info.st_size <= 4096


def still_owned(target, owner):
    try:
        if re.fullmatch('[0-9a-f]{64}', target.name):
            try:
                return targets.marker_owner(target)[0] == owner
            except FileNotFoundError:
                # An absent marker is attributed by the checkout path its directory
                # name hashes to, so revalidate that inference before any deletion.
                return (targets.checkout_hash(owner.resolve()) == target.name
                        and secode_checkout(owner))
        try:
            return owner_marker(target) == owner
        except FileNotFoundError:
            return cargo_target(target) and (owner == target or
                    target == owner / 'target' and secode_checkout(owner))
    except (OSError, RuntimeError, UnicodeError):
        return False


def owns_layout(path):
    """Whether this cache path is a kind discovery itself attributes and owns.

    The one place that knows the supported layouts. A target inventory that
    keeps its own copy of that knowledge goes stale: a legacy layout discovery
    still owns would then be reported as an orphan right after it grew.
    """
    name = Path(path).name
    if name == 'worktrees':
        return True
    if name in {'.leases', 'gate-logs'}:
        return True
    if re.fullmatch(r'(?:rustc_.+|[0-9]+\.[0-9]+(?:\.[0-9]+)?)', name):
        return cargo_target(path)
    return False


def discover(cache, home, deadline, qualification=None):
    owners = {}
    root = cache / 'target'
    # A build that never registered leaves no marker, so its target is named only
    # by the hash of a live checkout's path. That attests path identity, not
    # registration: it is inference from a directory name, and it cannot recover
    # a checkout that no longer exists.
    unmarked = {}
    if directory(root):
        for entry in targets.bounded_entries(root, 64, deadline):
            path = Path(entry.path)
            if not directory(path) or entry.name == '.leases':
                continue
            trees = path if entry.name == 'worktrees' else path / 'worktrees'
            if directory(trees):
                for child in targets.bounded_entries(trees, 256, deadline):
                    target = Path(child.path)
                    if not directory(target) or not re.fullmatch('[0-9a-f]{64}', child.name):
                        continue
                    try:
                        # Absence of the checkout does not invalidate its retained marker.
                        owner, _ = targets.marker_owner(target)
                        owners[target] = owner
                    except FileNotFoundError:
                        unmarked.setdefault(child.name, []).append(target)
                    except (OSError, RuntimeError, UnicodeError) as error:
                        # Present but malformed or mismatched stays preserved.
                        print(f'Preserved unattributed target {target}: {error}', file=sys.stderr)
            elif re.fullmatch(r'(?:rustc_.+|[0-9]+\.[0-9]+(?:\.[0-9]+)?)', entry.name) and cargo_target(path):
                try:
                    owners[path] = owner_marker(path)
                except FileNotFoundError:
                    # This is the dedicated SE Code cache's former shared target.
                    # The target itself is its process-attribution root, not a checkout.
                    owners[path] = path
                except (OSError, RuntimeError, UnicodeError) as error:
                    print(f'Preserved unattributed legacy target {path}: {error}', file=sys.stderr)

    roots = [home]
    workspace = worktrees()
    if home == Path.home() and directory(workspace):
        roots.append(workspace)
    for parent in roots:
        for entry in targets.bounded_entries(parent, 16384, deadline):
            owner = Path(entry.path)
            if not secode_checkout(owner):
                continue
            if cargo_target(owner / 'target'):
                owners[owner / 'target'] = owner
            # The directory name is the hash of the checkout path, so a live
            # checkout claims its unmarked target. still_owned revalidates this
            # inference under the admission lock before anything is deleted.
            for target in unmarked.pop(targets.checkout_hash(owner.resolve()), ()):
                owners[target] = owner

    for targets_left in unmarked.values():
        for target in targets_left:
            print(f'Preserved unattributed target {target}: no live checkout matches its directory name',
                  file=sys.stderr)

    if qualification is not None and directory(qualification):
        for entry in targets.bounded_entries(qualification, 256, deadline):
            run = Path(entry.path)
            if not directory(run) or not cargo_target(run / 'target'):
                continue
            target = run / 'target'
            try:
                owner = owner_marker(target)
            except FileNotFoundError:
                owner = run if secode_checkout(run) else None
            except (OSError, RuntimeError, UnicodeError) as error:
                print(f'Preserved unattributed qualification target {target}: {error}', file=sys.stderr)
                continue
            if owner is not None and (secode_checkout(owner) or targets.target_missing(owner)):
                owners[target] = owner
            else:
                print(f'Preserved unattributed qualification target {target}', file=sys.stderr)
    return owners
