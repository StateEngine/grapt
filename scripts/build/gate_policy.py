"""The 2026-10-24 landing-cost policy, shared by runner and controller."""

CORE = frozenset({'grapt-types', 'grapt-store', 'grapt-runtime', 'grapt-event', 'grapt-schema'})
BUILD_FILES = frozenset({'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'clippy.toml',
                         'build.rs', 'scripts/gate.sh', 'scripts/land.ps1', 'scripts/build-cache.sh'})
BUILD_PREFIXES = ('scripts/build/', 'scripts/release/', 'scripts/package/',
                  'scripts/daily-use/', '.cargo/', '.github/workflows/',
                  'packages/plugin-bridge/', 'fixtures/release-packaging/',
                  'remote-compute/release-packaging')

RETAINED_FULL_POLICY = 'Retained full gates support only Tier 2 reviewed input compositions; Tier 1 requires a fresh full gate on the composed commit.'


def required_mode(paths, *, row_a=False, root=None, base=None, head=None):
    """Renames/deletions supply both paths; documentation alone stays affected."""
    if root is not None and base is not None:
        from gate_scope import scoped_paths
        paths = scoped_paths(paths, root, base, head)
    for path in paths:
        parts = path.split('/')
        if (path in BUILD_FILES or path.startswith(BUILD_PREFIXES) or
                len(parts) >= 2 and parts[0] == 'crates' and parts[1] in CORE
                and not (path.endswith('.md') or len(parts) >= 3 and parts[2] == 'docs')):
            return 'full'
    return 'full' if row_a else 'affected'
