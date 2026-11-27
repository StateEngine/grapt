"""Resolve operator storage locations without embedding a machine identity."""
import os
from pathlib import Path
import re


def load_environment():
    path = Path.home()/'.config/secode/build.env'
    try:
        with path.open('rb') as source:
            raw = source.read(65537)
    except FileNotFoundError:
        return {}
    if len(raw) > 65536:
        raise ValueError('Operator configuration exceeds 64 KiB')
    selected = {}
    for line in raw.decode('utf-8').splitlines():
        if not line.strip() or line.startswith('#'):
            continue
        key, separator, value = line.partition('=')
        if (not separator or not re.fullmatch(r'(?:SECODE|STATEENGINE)_[A-Z0-9_]+', key)
                or not value or value != value.strip() or any(c in value for c in '\x00\"\'')):
            raise ValueError('Operator configuration requires plain unquoted NAME=value rows')
        os.environ.setdefault(key, value)
        selected[key] = os.environ[key]
    return selected


def powershell_environment(script):
    return Path(__file__).with_name('operator_environment.ps1').read_text(encoding='utf-8') + '\n' + script


_CONFIGURATION = load_environment()


def directory(name, fallback):
    selected = Path(os.environ.get(name, fallback))
    if not selected.is_absolute():
        raise ValueError(name + ' must name an absolute directory')
    return selected


def data_root():
    return directory('SECODE_DATA_ROOT', Path.home()/'.local/share/secode')


def worktrees():
    return directory('SECODE_WORKTREE_PARENT', data_root()/'worktrees')


def cache_root():
    return directory('SECODE_CACHE_DIR', Path.home()/'.cache/secode')


def guest_root():
    """Where the landing VMs keep their state: the configured build mount, else
    the user's data root. Never discovered, so a guest path cannot move mounts."""
    return directory('SECODE_NVME_ROOT', data_root())


def nvme_mount():
    configured = os.environ.get('SECODE_NVME_ROOT')
    if configured is not None:
        path = directory('SECODE_NVME_ROOT', cache_root()).resolve()
        return path if path != path.parent else None
    # A bind-mounted cache already identifies the build filesystem. Refuse the
    # system root when no dedicated build mount was configured or discovered.
    path = cache_root().resolve()
    while path != path.parent and not os.path.ismount(path):
        path = path.parent
    return path if path != path.parent else None


if __name__ == '__main__':
    import shlex
    import sys
    if sys.argv[1:] != ['--shell']:
        raise SystemExit('Use --shell to load operator settings in a shell')
    for name, value in _CONFIGURATION.items():
        print('export ' + name + '=' + shlex.quote(value))
