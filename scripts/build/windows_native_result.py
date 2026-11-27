"""Source and coverage checks for the native Windows leg of a gate."""
import hashlib
import json
from pathlib import Path
import re
import time

import windows_gate

SCHEMA = 'secode.native-windows-gate.v1'
MAX_LOG = 64 * 1024**2
CAPACITY_EXIT = 75


class CapacityUnavailable(RuntimeError):
    """The PC could not admit native work before the request deadline."""


def read(path, maximum):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > maximum:
        raise ValueError('Native gate input is missing, linked or oversized: '+str(path))
    with path.open('rb') as stream:
        raw = stream.read(maximum+1)
    if len(raw) > maximum:
        raise ValueError('Native gate input exceeds its bound')
    return raw


def verify(path, *, commit, packages, workspace=False, nightly=False):
    if not re.fullmatch('[0-9a-f]{40}', commit):
        raise ValueError('Native gate requires an exact source commit')
    path = Path(path)
    row = json.loads(read(path, 65536))
    if (row.get('schema') != SCHEMA or row.get('exit') != 0
            or row.get('source_before') != {'commit': commit, 'clean': True}
            or row.get('source_after') != {'commit': commit, 'clean': True}
            or row.get('target') != windows_gate.TARGET
            or row.get('packages') != sorted(set(packages))
            or row.get('workspace') is not workspace or row.get('nightly') is not nightly
            or row.get('children_clean') is not True):
        raise ValueError('Native Windows result does not cover the selected clean source and scope')
    expected = windows_gate.native_commands(packages, workspace=workspace, nightly=nightly,
                                            jobs=row.get('jobs'))
    if not expected or row.get('commands') != expected:
        raise ValueError('Native Windows result is missing required feature configurations')
    finished = row.get('finished_unix')
    if type(finished) not in (float, int) or not 0 <= time.time()-finished <= 86400:
        raise ValueError('Native Windows result is stale')
    raw = read(path.with_suffix(path.suffix+'.log'), MAX_LOG)
    if hashlib.sha256(raw).hexdigest() != row.get('log_sha256'):
        raise ValueError('Native Windows log differs from its result')
    return row
