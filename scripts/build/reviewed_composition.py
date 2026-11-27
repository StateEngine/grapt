"""Retain a full gate only for an explicitly reviewed TUI input composition."""
import hashlib
from pathlib import Path
import re
import stat
import subprocess
import tempfile

# This is an input-adapter composition, not a general gate-cost waiver. New
# dependency, authority, core and build changes must earn their own full gate.
INPUT_PATHS = frozenset({
    'crates/grapt-client/src/client.rs',
    'crates/grapt-tui/src/main.rs',
    'crates/grapt-tui/src/record_input.rs',
    'crates/grapt-tui/src/windows_input.rs',
})
# Each upstream adaptation is one small fragment under notices/; the assembled
# THIRD_PARTY_NOTICES.md is generated and untracked, so no other document is covered.
INPUT_PREFIXES = ('notices/',)


def git(root, *args):
    value = subprocess.check_output(['git', '--no-optional-locks', '-C', str(root), *args], timeout=60)
    if len(value) > 1024 * 1024:
        raise ValueError('Reviewed composition Git output exceeds 1 MiB')
    return value


def retained_arguments(value):
    if not isinstance(value, dict) or set(value) != {'commit', 'base', 'log'}:
        raise ValueError('Retained full needs exactly commit, base and log')
    if any(not isinstance(v, str) or not v or '\0' in v for v in value.values()):
        raise ValueError('Invalid retained full value')
    if any(not re.fullmatch('[0-9a-f]{40}', value[k]) for k in ('commit', 'base')):
        raise ValueError('Retained full needs full commit identities')
    if len(value['log']) > 4096 or not Path(value['log']).is_absolute():
        raise ValueError('Retained full log must be absolute')
    return value


def digest(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > 65536:
        raise ValueError('Retained full receipt must be a bounded regular file')
    return hashlib.sha256(path.read_bytes()).hexdigest()


def patch(root, base, commit):
    rows = git(root, 'diff', '--raw', '--no-abbrev', '--no-renames', '-z', base, commit).split(b'\0')
    if rows.pop() != b'' or len(rows) % 2:
        raise ValueError('Malformed reviewed composition diff')
    result = {}
    for header, path in zip(rows[::2], rows[1::2]):
        name = path.decode('utf-8')
        if name in result:
            raise ValueError('Duplicate reviewed composition path')
        result[name] = header.decode('ascii')
    return result


def regular_text(root, commit, path, optional=False):
    entry = git(root, 'ls-tree', '-z', commit, '--', path)
    if not entry and optional:
        return
    if not entry.startswith(b'100644 blob ') or entry.count(b'\0') != 1:
        raise ValueError(f'Reviewed input must remain regular text: {path}')
    oid = entry.split(b'\t', 1)[0].split()[-1].decode('ascii')
    if int(git(root, 'cat-file', '-s', oid)) > 1024 * 1024:
        raise ValueError(f'Reviewed input exceeds 1 MiB: {path}')
    text = git(root, 'cat-file', 'blob', oid)
    if b'\0' in text:
        raise ValueError(f'Reviewed input is binary: {path}')
    text.decode('utf-8')
    return text


def unchanged_test_replay(root, path, commit, base, retained):
    # Accepted canonical edits can change the blob identity of a retained test
    # without changing its authored patch. Recompute the exact text merge;
    # production, dependency, core and build paths retain raw-entry equality.
    if not path.startswith('crates/grapt-product/tests/') or not path.endswith('.rs'):
        return False
    revisions = (base, retained['base'], retained['commit'])
    blobs = [regular_text(root, revision, path) for revision in revisions]
    with tempfile.TemporaryDirectory(prefix='secode-reviewed-test-') as directory:
        files = [Path(directory) / name for name in ('canonical', 'base', 'qualified')]
        for file, blob in zip(files, blobs):
            file.write_bytes(blob)
        merged = subprocess.run(['git', 'merge-file', '--stdout', *map(str, files)],
                                capture_output=True, timeout=60)
    if merged.returncode:
        raise ValueError(f'Canonical test edits conflict with the qualified patch: {path}')
    return merged.stdout == regular_text(root, commit, path)


def coverage(root, commit, base, retained):
    retained_arguments(retained)
    git(root, 'merge-base', '--is-ancestor', retained['base'], retained['commit'])
    git(root, 'merge-base', '--is-ancestor', retained['base'], base)
    git(root, 'merge-base', '--is-ancestor', base, commit)
    old = patch(root, retained['base'], retained['commit'])
    new = patch(root, base, commit)
    fresh = sorted(p for p in old.keys() | new.keys() if old.get(p) != new.get(p))
    if not fresh:
        raise ValueError('Composition has no fresh input qualification scope')
    for path in fresh:
        if path not in INPUT_PATHS and not path.startswith(INPUT_PREFIXES) and not unchanged_test_replay(root, path, commit, base, retained):
            raise ValueError('Composition changes work outside the retained full input scope')
    for path in fresh:
        # Existing qualified inputs and every reviewed head must be plain text.
        # New named adapter files are allowed; deletion or type changes refuse.
        regular_text(root, retained['commit'], path, optional=True)
        regular_text(root, commit, path)
        for change in (old.get(path), new.get(path)):
            if change is not None and any(mode not in ('000000', '100644') for mode in change[1:].split()[:2]):
                raise ValueError(f'Reviewed input changes file type or mode: {path}')
    return fresh


def proof(root, commit, base, retained):
    import receipt
    retained_arguments(retained)
    path = Path(retained['log']).with_suffix(Path(retained['log']).suffix + '.receipt.json')
    before = digest(path)
    fresh = coverage(root, commit, base, retained)
    original = receipt.verify(Path(retained['log']), retained['commit'],
                              base=retained['base'], require_full=True)
    if original['mode'] != 'full' or original.get('base') != retained['base'] or digest(path) != before:
        raise ValueError('Retained result must be an unchanged exact-base full gate')
    return {'retained_full': dict(retained), 'receipt_sha256': before,
            'fresh_paths': fresh}


def commands(packages, paths, root, base):
    import gate_plan
    # Recheck the complete ordinary test graph, then the normal affected plan:
    # formatting, licenses, strict lint/tests, CLI prerequisite and all law checks.
    return [['cargo', 'check', '--locked', '--workspace', '--tests'],
            *gate_plan.commands('affected', packages, set(paths), root, base=base)]
