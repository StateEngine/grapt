"""Windows qualification plans: retained cross checks and the native PC leg."""
import os
from pathlib import Path
import sys
import test_features


TARGET = 'x86_64-pc-windows-msvc'


def native_commands(packages, *, workspace=False, nightly=False, jobs=2):
    """Run both fixture-enabled and ordinary tests on Windows itself."""
    import slow_tests
    if type(jobs) is not int or jobs != 2:
        raise ValueError('Native Windows Cargo requires exactly 2 jobs')
    if not packages and not workspace:
        return []
    if len(packages) > 256 or any(not 0 < len(name) <= 64 or not all(c.isascii() and (c.isalnum() or c in '-_') for c in name)
                                  for name in packages):
        raise ValueError('Invalid bounded Windows package scope')
    selectors = ['--workspace'] if workspace else [arg for name in sorted(set(packages)) for arg in ('-p', name)]
    features = test_features.arguments(None if workspace else packages)
    common = ['--locked', *selectors, '--target', TARGET, '-j', str(jobs)]
    result = []
    for enabled in ([features, []] if features else [[]]):
        result.append(['cargo', 'clippy', *common, *enabled, '--all-targets', '--', '-D', 'warnings'])
        if workspace or {'grapt-product', 'grapt-client'} & set(packages):
            result.append(['cargo', 'build', '--locked', '-p', 'grapt-cli', '--bin', 'grapt',
                           '--target', TARGET, '-j', str(jobs), *test_features.arguments(['grapt-cli'])])
        result.append(['cargo', 'test', *common, *enabled, '--no-fail-fast', '--', '--test-threads=1',
                       *([] if nightly else slow_tests.arguments(None if workspace else packages))])
    return result


def native_command(packages, *, commit, receipt, workspace=False, nightly=False, jobs=2, timeout=10800):
    """One command for the gate driver to start beside its Linux command list."""
    if not native_commands(packages, workspace=workspace, nightly=nightly, jobs=jobs):
        return None
    result = [sys.executable, str(Path(__file__).with_name('windows_native.py')),
              '--commit', commit, '--receipt', str(receipt), '--jobs', str(jobs), '--timeout', str(timeout)]
    if workspace:
        result.append('--workspace')
    else:
        for name in sorted(set(packages)):
            result += ['--package', name]
    if nightly:
        result.append('--nightly')
    return result


def verify_native(receipt, *, commit, packages, workspace=False, nightly=False):
    from windows_native_result import verify
    return verify(receipt, commit=commit, packages=packages, workspace=workspace, nightly=nightly)


def commands(packages, *, workspace=False):
    if not packages and not workspace:
        return []
    selectors = ['--workspace'] if workspace else [arg for name in packages for arg in ('-p', name)]
    features = test_features.arguments(None if workspace else packages)
    common = ['cargo', 'xwin']
    result = []
    # Fixture flags can hide ordinary tests, including crash-selector guards.
    for enabled in ([features, []] if features else [[]]):
        options = ['--locked', *selectors, '--target', TARGET, *enabled]
        result.extend([
            [*common, 'clippy', *options, '--all-targets', '--', '-D', 'warnings'],
            [*common, 'test', *options, '--no-run'],
        ])
    return result


def environment(command):
    if command[:2] != ['cargo', 'xwin']:
        return None
    import heavy
    raw = os.environ.get('SECODE_WINDOWS_TARGET_DIR', os.environ.get('CARGO_TARGET_DIR', ''))
    if not raw or not Path(raw).is_absolute():
        raise ValueError('Windows compilation requires the checkout NVMe target')
    target = heavy.nvme_target(raw)
    if target is None:
        raise ValueError('Windows compilation requires the checkout NVMe target')
    result = dict(os.environ, CARGO_TARGET_DIR=str(target), SECODE_WORKTREE_TARGET=str(target),
                  SECODE_TARGET_ROOT=str(target.parent.parent))
    # The cross build obtains its own disk lease; it cannot borrow a RAM lease.
    for name in ('SECODE_RAM_TARGET_RUN', 'SECODE_RAM_DISK_CACHE',
                 'SECODE_CACHE_LEASE_FD', 'SECODE_CACHE_ADMITTED_TARGET',
                 'SECODE_CACHE_ADMISSION_PROCESS'):
        result.pop(name, None)
    return result
