"""Per-run test scratch on the operator's storage, so passing tests leave the RAM-backed /tmp empty.

The box mounts /tmp as tmpfs; a test that forgets to clean up spends memory
for everyone. SE Code checkouts therefore get a fresh per-run directory on
the operator test scratch as their TMPDIR when one can be made. When that
base is unavailable the caller's environment is returned unchanged:
relocation degrades, never fails.
"""
import os
from pathlib import Path
import shutil
import time

import operator_paths

KEYS = ('TMPDIR', 'TMP', 'TEMP')
SWEEP_SECONDS = 86400
MAX_SWEEP_ENTRIES = 4096


def base_path():
    """The test-scratch base: an explicit override, else the operator data root."""
    configured = os.environ.get('SECODE_TEST_TMP_BASE')
    if configured:
        return Path(configured)
    return operator_paths.data_root() / 'test-tmp'


def resolve_base():
    """The test-scratch base, or None when it cannot be probed writable."""
    try:
        base = base_path()
        os.makedirs(base, exist_ok=True)
        probe = os.path.join(base, f'.probe-{os.getpid()}')
        os.mkdir(probe)
        os.rmdir(probe)
    except (OSError, ValueError):
        return None
    return base


def sweep(base, now=None):
    """Reap run directories idle past the bound; never touch foreign names."""
    if now is None:
        now = time.time()
    deadline = now - SWEEP_SECONDS
    try:
        entries = os.listdir(base)
    except OSError:
        return
    for name in entries[:MAX_SWEEP_ENTRIES]:
        if not name.startswith('run-'):
            continue
        path = os.path.join(base, name)
        try:
            if os.lstat(path).st_mtime < deadline:
                shutil.rmtree(path, ignore_errors=True)
        except OSError:
            pass


def job_environment(environment):
    """Give the job a fresh run directory as its temp; (environment, run dir)."""
    base = resolve_base()
    if base is None:
        return environment, None
    sweep(base)
    stamp = int(time.time() * 1000)
    run_dir = os.path.join(base, f'run-{os.getpid()}-{stamp}')
    while os.path.exists(run_dir):
        stamp += 1
        run_dir = os.path.join(base, f'run-{os.getpid()}-{stamp}')
    os.mkdir(run_dir)
    updated = dict(environment)
    updated.update({key: run_dir for key in KEYS})
    return updated, run_dir


def residue(run_dir):
    """Entries a finished run left behind, for the gate's residue check."""
    if run_dir is None:
        return []
    try:
        return sorted(os.listdir(run_dir))
    except OSError:
        return []
