"""Compose an explicitly reviewed queue and bisect named qualification failures."""
from dataclasses import dataclass
from pathlib import Path
import re

import landing

MAX_CANDIDATES = 8


@dataclass(frozen=True)
class Candidate:
    root: Path
    branch: str
    commit: str
    base: str
    owner: str
    reviewer: str
    acceptance: str
    tier: str


def admissions(value):
    """The owner supplies their review reference; this never manufactures ACCEPT."""
    fields = set(Candidate.__dataclass_fields__)
    if not isinstance(value, list) or not 1 <= len(value) <= MAX_CANDIDATES:
        raise ValueError('A batch needs one to eight explicitly accepted candidates')
    rows = []
    for item in value:
        if not isinstance(item, dict) or set(item) != fields:
            raise ValueError('Every candidate needs its exact source and independent ACCEPT reference')
        if any(not isinstance(v, str) or not 0 < len(v) <= 256 for v in item.values()):
            raise ValueError('Candidate fields must be bounded strings')
        row = Candidate(**(item | {'root': Path(item['root'])}))
        if (not row.root.is_absolute() or row.root.resolve() != row.root or
                not re.fullmatch('[a-z][a-z0-9-]{0,127}', row.branch) or
                any(not re.fullmatch('[0-9a-f]{40}', v) for v in (row.commit, row.base)) or
                any(not re.fullmatch('agt-[a-z0-9-]{3,80}', v) for v in (row.owner, row.reviewer)) or
                row.owner == row.reviewer or not re.fullmatch('msg-[a-zA-Z0-9-]{1,100}', row.acceptance) or
                row.tier not in {'1', '2'}):
            raise ValueError('Invalid accepted source, owner, independent reviewer or tier')
        rows.append(row)
    if len({row.commit for row in rows}) != len(rows) or len({row.root for row in rows}) != len(rows):
        raise ValueError('Each accepted candidate must have its own commit and owning checkout')
    return rows


def head(root):
    return landing.git(root, 'rev-parse', 'HEAD').decode().strip()


def compose(repository, checkout, branch, base, candidates):
    """Keep every accepted commit as an ancestor and every owner branch untouched."""
    changed = set()
    patches = []
    for row in candidates:
        landing.source(row.root, row.commit, row.branch)
        landing.heavy.run(['git', 'fetch', '--no-tags', '--', str(row.root), row.commit],
                          cwd=repository, check=True, timeout=120)
        landing.git(repository, 'merge-base', '--is-ancestor', row.base, row.commit)
        landing.git(repository, 'merge-base', '--is-ancestor', row.base, base)
        owned = landing.paths(repository, row.base, row.commit)
        if not owned or len(owned) > 512 or len(changed | owned) > 512:
            raise ValueError('Batch changes must contain one to 512 paths')
        # The normal landing helper permits some generated-file composition.
        # Batching is deliberately narrower: no shared path or ancestor path.
        for left in owned:
            for right in changed | landing.paths(repository, row.base, base):
                a, b = left.casefold(), right.casefold()
                if a == b or a.startswith(b + '/') or b.startswith(a + '/'):
                    raise ValueError(f'Batch overlap requires owner composition: {left}')
        changed.update(owned)
        patches.append((row, owned, landing.raw_diff(repository, row.base, row.commit)))
    if checkout.exists() or checkout.is_symlink():
        raise ValueError('Preserve the existing batch checkout')
    landing.heavy.run(['git', 'worktree', 'add', '-b', branch, str(checkout), base],
                      cwd=repository, check=True, timeout=120)
    for row, _, _ in patches:
        body = (f'build(landing): compose accepted slice\n\n'
                f'Accepted source: {row.commit}\n'
                'Co-Authored-By: secode-seats <353560407+secode-seats[bot]@users.noreply.github.com>\n')
        landing.heavy.run(['git', '-c', 'core.hooksPath=/dev/null', 'merge', '--no-ff',
                          '--no-edit', '-m', body, row.commit],
                         cwd=checkout, check=True, timeout=120)
    combined = head(checkout)
    for row, owned, original in patches:
        landing.git(checkout, 'merge-base', '--is-ancestor', row.commit, combined)
        actual = landing.git(checkout, 'diff', '--raw', '--no-abbrev', '--no-renames',
                             '-z', base, combined, '--', *sorted(owned))
        if actual != original:
            raise ValueError('Batch composition changed an accepted patch')
        landing.source(row.root, row.commit, row.branch)
    landing.source(checkout, combined, branch)
    return combined


@dataclass(frozen=True)
class Outcome:
    status: str  # landed, red (named source failure), or blocked
    detail: str


def drain(candidates, attempt, record):
    """At most 2N-1 qualifications; unknown failures stop, never certify or blame."""
    pending = [list(candidates)]
    results = []
    while pending:
        group = pending.pop(0)
        outcome = attempt(group)
        if outcome.status not in {'landed', 'red', 'blocked'}:
            raise ValueError('Unknown batch outcome')
        event = dict(commits=[row.commit for row in group], status=outcome.status,
                     detail=outcome.detail)
        record(event)
        results.append(event)
        if outcome.status == 'blocked':
            break
        if outcome.status == 'red' and len(group) > 1:
            middle = len(group)//2
            pending[0:0] = [group[:middle], group[middle:]]
    return results
