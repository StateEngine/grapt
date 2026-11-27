"""Reject inherited product branding on shipped SE Code surfaces."""
from pathlib import Path
import json
import re
import subprocess


LEGACY = 'open' + 'code'
# Upstream attribution fragments; the assembled THIRD_PARTY_NOTICES.md is untracked.
ATTRIBUTION_ROOT = 'notices/'
# One exact declaration at the CLI compatibility boundary, never a file exemption.
COMPATIBILITY_FILE = 'crates/grapt-cli/src/branding_compat.rs'
LEGACY_CONFIG_DECLARATION = (b'pub(crate) const LEGACY_CONFIG_ENV: &str = "'
                             + LEGACY.upper().encode() + b'_CONFIG";')
SURFACES = ('crates/', 'scripts/release/', 'scripts/package/',
            'packages/plugin-bridge/', 'docs/release-packaging/',
            'docs/daily-driver/', 'README.md', 'docs/RELEASE-0.1.md',
            'scripts/build/branding.py', 'scripts/build/test_branding.py',
            ATTRIBUTION_ROOT)
# Benchmark JSON witnesses retain actual competitor metadata. Check their
# filenames so the renamed model-limit witness cannot return; competitor
# installer and driver commands keep their real tool names.
FILENAME_SURFACES = ('bench/',)

# Coordinates of a provider gateway we connect to: exact declaration lines in
# the two files that route by it, never a file exemption.
GATEWAY_DECLARATIONS = (
    b'const SESSION_ROUTED_HOST: &str = "' + LEGACY.encode() + b'.ai";',
    b'const SESSION_ROUTING_HEADER: &str = "x-' + LEGACY.encode() + b'-session";',
)
GATEWAY_FILES = ('crates/grapt-provider/src/transport/adapter.rs', 'crates/grapt-login/src/key.rs')

MAX_FILE_BYTES = 4 * 1024 * 1024
# Public provider metadata retains actual competitor names and documentation URLs.
CATALOG = 'crates/grapt-connections/src/catalog/models.dev-2026-11-03.json'
MAX_CATALOG_BYTES = 16 * 1024 * 1024
CHECKLIST = 'docs/RELEASE-0.1.md'
# These comparison sentences name a measured competitor, not this product.
# Match complete lines in this one document; additional branding still fails.
VERSION = rb'[0-9]+(?:\.[0-9]+){1,3}'
COMPARISON_REFERENCES = (
    re.compile(rb'qualified value among Codex ' + VERSION + rb', Claude Code ' + VERSION
               + rb', ' + LEGACY.encode() + rb'2 ' + VERSION + rb' and the', re.IGNORECASE),
    re.compile(LEGACY.encode()
               + rb'2 and TS frontend CPU is unavailable because separate parent/nested-job',
               re.IGNORECASE),
)


def violations(root, paths):
    problems = []
    for name in sorted(set(paths)):
        filename_only = name.startswith(FILENAME_SURFACES)
        if filename_only and not name.casefold().endswith('.json'):
            continue
        if LEGACY in name.casefold():
            problems.append(f'{name}: inherited branding in filename')
        path = root / name
        if not path.exists():  # A tracked deletion is absent from the shipped tree.
            continue
        if path.is_symlink() or not path.is_file():
            raise ValueError(f'Branding check requires a regular file: {name}')
        bound = MAX_CATALOG_BYTES if name == CATALOG else MAX_FILE_BYTES
        with path.open('rb') as stream:
            data = stream.read(bound + 1)
        if len(data) > bound:
            raise ValueError(f'Branding check file exceeds its {bound}-byte bound: {name}')
        if name == CATALOG:
            providers = json.loads(data)
            if (not isinstance(providers, dict) or not 1 <= len(providers) <= 256 or
                    any(not isinstance(row, dict) or row.get('id') != key or
                        not isinstance(row.get('models'), dict) for key, row in providers.items())):
                raise ValueError('Bundled provider catalog has an invalid metadata shape')
            continue
        if name.startswith(ATTRIBUTION_ROOT) or filename_only:
            continue
        for number, line in enumerate(data.splitlines(), 1):
            if name == COMPATIBILITY_FILE and line.strip() == LEGACY_CONFIG_DECLARATION:
                continue
            if name in GATEWAY_FILES and line.strip() in GATEWAY_DECLARATIONS:
                continue
            if name == CHECKLIST and any(pattern.fullmatch(line.strip()) for pattern in COMPARISON_REFERENCES):
                continue
            if LEGACY.encode() in line.lower():
                problems.append(f'{name}:{number}: inherited branding in content')
    return problems


def check(root):
    paths = subprocess.check_output(
        ['git', 'ls-files', '--cached', '--others', '--exclude-standard', '-z',
         '--', *SURFACES, *FILENAME_SURFACES], cwd=root, timeout=30).decode().split('\0')
    return violations(root, [name for name in paths if name])


def main():
    problems = check(Path(__file__).resolve().parents[2])
    if problems:
        raise SystemExit('\n'.join(problems))
    print('Shipped SE Code branding check passed')


if __name__ == '__main__':
    main()
