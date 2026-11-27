"""Keep Windows checks for a batch unless every changed path is provably inert.

A batch may skip the Windows leg only when nothing in it can reach Windows: all
changed paths are documentation, or scripts that run on the build box alone.
Anything else runs the checks, including .rs files, manifests, toolchain and
Cargo inputs, workflows, and paths this file has never seen. The rule fails
toward running, so a new kind of file costs a redundant Windows run rather than
a missed one.

The allowlist is deliberately small. Most batches change something else, so most
batches keep both legs; skipping is the exception, not the default.
"""
from pathlib import PurePosixPath
import re


# Documentation in any directory: a reader sees it, no build does.
DOCUMENT_SUFFIXES = ('.md', '.markdown', '.txt', '.rst', '.adoc')
# Box-only scripts: they drive Linux seats on the build host and are not part of
# the Windows package. Within these prefixes the Windows machinery is still a
# build input, because changing it changes the check itself.
BOX_SCRIPTS = (
    'scripts/build/', 'scripts/ci/', 'scripts/daily-use/', 'scripts/stateengine-native/',
)
# A gate-path module drives or verifies the Windows leg, so a change to one must
# run it. The list is the planner's own local imports plus the modules that plan
# and verify its commands; test_planner_imports_are_build_inputs walks those
# imports so this cannot drift silently.
WINDOWS_HARNESS = ('windows_gate.py', 'windows_scope.py', 'gate.py', 'gate_plan.py',
                   'receipt.py', 'test_features.py', 'slow_tests.py',
                   'windows_native_result.py', 'heavy.py')
GATE_PATH = WINDOWS_HARNESS
# Root-level prose that carries no build input.
ROOT_DOCUMENTS = ('README.md', 'AGENTS.md', 'CLAUDE.md')
# A path whose name looks inert is still a build input when it lives here.
# clippy.toml and deny.toml belong here: they change the checks themselves.
SENSITIVE = ('crates/', '.cargo/', '.github/', 'Cargo.toml', 'Cargo.lock',
             'rust-toolchain', 'build.rs', 'packages/', 'scripts/package/',
             'scripts/release/', 'clippy.toml', 'deny.toml', 'about.toml')

COMMIT = re.compile('[0-9a-f]{40}')


def build_input(name):
    """Whether a file name under a box-script prefix still reaches Windows."""
    if name.endswith('.ps1'):
        return True
    if name.startswith(('windows', 'xwin')):
        return True
    return name in GATE_PATH


def inert(path):
    """True only for a path a Windows build provably does not read."""
    if path.startswith(SENSITIVE) or PurePosixPath(path).name in ('Cargo.toml', 'Cargo.lock',
                                                                 'build.rs', 'rust-toolchain.toml',
                                                                 'rust-toolchain'):
        return False
    if path in ROOT_DOCUMENTS:
        return True
    if path.endswith(DOCUMENT_SUFFIXES):
        return True
    if path.startswith(BOX_SCRIPTS):
        return not build_input(PurePosixPath(path).name)
    return False


def required(paths, root=None, base=None, head=None):
    """Whether the Windows leg must run. An unknown answer means yes.

    `paths` must be the complete changed set for the commit range. An empty set
    is an unknown change, not an empty diff, so it still runs the checks.
    """
    if not paths:
        return True
    return not all(inert(path) for path in paths)
