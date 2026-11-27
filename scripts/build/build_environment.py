"""Load the existing SE Code shell defaults without forwarding seat credentials."""
import argparse
import json
import os
from pathlib import Path
import shlex
import stat
import subprocess
import sys

import tmp_scratch


EXPORTS = ('CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_HOME', 'CARGO_TARGET_DIR', 'CARGO_BUILD_JOBS', 'CARGO_INCREMENTAL',
           'RUSTFLAGS', 'SECODE_CACHE_DIR', 'SECODE_TARGET_ROOT',
           'SECODE_WORKTREE_ROOT', 'SECODE_WORKTREE_TARGET', 'SECODE_WINDOWS_TARGET_DIR',
           'SECODE_FIXTURE_COMMIT', 'SECODE_FIXTURES')
SEAT_EXPORTS = ('CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER', 'CARGO_HOME', 'CARGO_BUILD_JOBS', 'CARGO_INCREMENTAL',
                'SECODE_CACHE_DIR')


def load(root, environment=None):
    helpers = Path(__file__).resolve().parent
    script = helpers/'build-cache.sh'
    if not script.is_file() and helpers.name == 'build' and helpers.parent.name == 'scripts':
        script = helpers.parent/'build-cache.sh'
    if not script.is_file():
        installed = Path.home()/'.local/lib/secode-release/build-cache.sh'
        script = installed if installed.is_file() else Path(root)/'scripts/build-cache.sh'
    code = ('import json,os; keys=' + repr(EXPORTS) + '; '
            'values={k:os.environ[k] for k in keys if k in os.environ}; '
            'assert all(len(v)<=4096 for v in values.values()), "Build environment value exceeds bound"; '
            'print(json.dumps(values))')
    result = subprocess.run(['bash', '-eu', '-c', 'source "$1"; exec "$2" -c "$3"',
                             'secode-build-env', str(script), sys.executable, code],
                            cwd=root, env=environment, capture_output=True, text=True,
                            check=True, timeout=20)
    values = json.loads(result.stdout)
    # A caller outside the managed cache has no managed-root claim; every other
    # export is still required, so a partial environment never passes unnoticed.
    if ('SECODE_TARGET_ROOT' in values) != ('SECODE_WORKTREE_TARGET' in values):
        raise ValueError('Invalid bounded build environment')
    if not set(EXPORTS) - {'SECODE_TARGET_ROOT', 'SECODE_WORKTREE_TARGET', 'SECODE_WINDOWS_TARGET_DIR'} <= set(values):
        raise ValueError('Invalid bounded build environment')
    if any(not isinstance(v, str) or len(v) > 4096 for v in values.values()):
        raise ValueError('Invalid bounded build environment')
    return values


def for_checkout(cwd, environment):
    directory = Path(cwd).resolve()
    for root in [directory, *directory.parents][:64]:
        if ((root/'.git').exists() and (root/'Cargo.toml').is_file()
                and (root/'crates/grapt-types/Cargo.toml').is_file()):
            selected = dict(environment)
            if (selected.get('SECODE_WORKTREE_ROOT') not in (None, str(root))
                    and selected.get('CARGO_TARGET_DIR') == selected.get('SECODE_WORKTREE_TARGET')):
                selected.pop('CARGO_TARGET_DIR', None)
            values = load(root, selected)
            target = Path(values['CARGO_TARGET_DIR']).resolve()
            marker = target/'.secode-worktree'
            managed = values.get('SECODE_TARGET_ROOT')
            try:
                descriptor = os.open(marker, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                with os.fdopen(descriptor, 'rb') as source:
                    info = os.fstat(source.fileno())
                    if not stat.S_ISREG(info.st_mode) or info.st_size > 4096:
                        raise ValueError('Build target owner is not a bounded regular marker')
                    if source.read(4097) != os.fsencode(str(root)+'\n'):
                        raise ValueError('Build target belongs to another checkout')
            except FileNotFoundError:
                if marker.is_symlink():
                    raise ValueError('Build target owner is unreadable') from None
                if managed is not None and target == Path(values['SECODE_WORKTREE_TARGET']).resolve():
                    from evict_targets import register
                    with register(Path(managed), target, root):
                        pass
            if managed is not None and target == Path(managed).resolve():
                raise ValueError('Build refuses the retired shared target directory')
            selected = dict(environment, **values)
            if managed is None:
                # The caller chose an output outside the managed cache. Drop any
                # inherited claim, so no child presents itself as cache-owned.
                for name in ('SECODE_TARGET_ROOT', 'SECODE_WORKTREE_TARGET', 'SECODE_WINDOWS_TARGET_DIR'):
                    selected.pop(name, None)
            selected.pop('RUSTC_WRAPPER', None)
            selected.pop('RUSTC_WORKSPACE_WRAPPER', None)
            # A caller-provided temp wins; otherwise the job gets its own
            # NVMe run directory so passing tests leave the RAM /tmp empty.
            if not any(selected.get(key) for key in tmp_scratch.KEYS):
                selected, _ = tmp_scratch.job_environment(selected)
            return selected
    return None  # Foreign projects retain their own environment.


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('checkout', type=Path)
    args = parser.parse_args()
    values = for_checkout(args.checkout, os.environ) or load(args.checkout, os.environ)
    print('unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER')
    for name in SEAT_EXPORTS:
        print(f'export {name}={shlex.quote(values[name])}')


if __name__ == '__main__':
    main()
