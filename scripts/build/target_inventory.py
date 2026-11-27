"""Read-only accounting of managed and checkout-local Cargo targets."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import time

import evict_targets as targets


MAX_TARGETS = 256
# StateEngine owns this checkout; do not inspect its target.
EXCLUDED = {'5e-authr2'}


def measure(path, deadline):
    row = {'path': str(path), 'mtime': None, 'allocated_bytes': None}
    try:
        if path.is_symlink() or not path.is_dir():
            raise ValueError('Target is absent or is not a real directory')
        row['mtime'] = path.stat().st_mtime
        row['allocated_bytes'] = targets.usage(path, deadline)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        row['error'] = str(error)
    return row


def filesystem(path):
    row = {'path': str(path), 'total_bytes': None, 'used_bytes': None,
           'available_bytes': None}
    try:
        disk = shutil.disk_usage(path)
        row.update(total_bytes=disk.total, used_bytes=disk.used, available_bytes=disk.free)
    except OSError as error:
        row['error'] = str(error)
    return row


def report(cache, home, seconds=900):
    deadline = time.monotonic() + seconds
    managed = []
    other_roots = []
    root = cache / 'target'
    if root.is_dir() and not root.is_symlink():
        for toolchain in targets.bounded_entries(root, 64, deadline):
            if not toolchain.is_dir(follow_symlinks=False) or toolchain.name == '.leases':
                continue
            path = Path(toolchain.path)
            trees = path if toolchain.name == 'worktrees' else path / 'worktrees'
            if trees.is_symlink() or not trees.is_dir():
                other_roots.append(measure(path, deadline))
                continue
            for entry in targets.bounded_entries(trees, MAX_TARGETS, deadline):
                if not entry.is_dir(follow_symlinks=False):
                    continue
                if len(managed) >= MAX_TARGETS:
                    raise RuntimeError('Managed target inventory exceeds its bound')
                path = Path(entry.path)
                row = measure(path, deadline)
                row['owner'] = None
                try:
                    owner, _ = targets.marker_owner(path)
                    row['owner'] = str(owner)
                except (OSError, RuntimeError, UnicodeError) as error:
                    row['owner_error'] = str(error)
                managed.append(row)
    unmanaged = []
    for entry in targets.bounded_entries(home, 16384, deadline):
        if entry.name in EXCLUDED or not entry.is_dir(follow_symlinks=False):
            continue
        path = Path(entry.path) / 'target'
        if path.is_dir() and not path.is_symlink():
            if len(unmanaged) >= MAX_TARGETS:
                raise RuntimeError('Unmanaged target inventory exceeds its bound')
            unmanaged.append(measure(path, deadline))
    # Whole roots deduplicate hardlinks; never substitute the per-target sum.
    return {
        'schema': 'secode.target-inventory.v1', 'read_only': True,
        'measured_unix': time.time(), 'budget_bytes': targets.CAP_BYTES,
        'free_space_floor_bytes': targets.FREE_SPACE_FLOOR_BYTES,
        'build_reserve_bytes': targets.BUILD_RESERVE_BYTES,
        'free_space_floor_note': 'Fresh Cargo admission enforces the filesystem floor plus build reserve',
        'filesystem': filesystem(cache.resolve()),
        'root_filesystem': filesystem(Path('/')),
        'whole_cache': measure(cache, deadline), 'managed_total': measure(root, deadline),
        'managed_worktrees': sorted(managed, key=lambda row: row['path']),
        'other_managed_roots': sorted(other_roots, key=lambda row: row['path']),
        'unmanaged_targets': sorted(unmanaged, key=lambda row: row['path']),
        'excluded_checkouts': sorted(EXCLUDED),
        'size_note': 'Allocated bytes per du invocation; hardlinks can overlap between rows',
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cache', type=Path, required=True)
    parser.add_argument('--home', type=Path, required=True)
    args = parser.parse_args()
    result = report(args.cache, args.home)
    print(json.dumps(result, indent=2), flush=True)
    rows = [result['filesystem'], result['root_filesystem'],
            result['whole_cache'], result['managed_total'],
            *result['managed_worktrees'], *result['other_managed_roots'],
            *result['unmanaged_targets']]
    return int(any('error' in row for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
