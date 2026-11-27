"""Qualify plain documentation with Git, without reserving build capacity."""
from pathlib import Path
import time
import json
import stat

import landing


def only_docs(root, base, commit):
    rows = landing.git(root, 'diff', '--raw', '--no-abbrev', '--no-renames', '-z', base, commit, '--').split(b'\0')
    if rows.pop() != b'' or not rows or len(rows) % 2 or len(rows) > 512:
        return False
    for header, raw_path in zip(rows[::2], rows[1::2]):
        path = raw_path.decode('utf-8')
        name = path.casefold()
        allowed = (name.rsplit('/', 1)[-1] not in {'agents.md', 'claude.md', 'skill.md'} and
                   (name.startswith('docs/') and name.endswith('.md') or
                    name in {'readme.md', 'contributing.md', 'changelog.md'}))
        fields = header.decode('ascii').split()
        if (not allowed or len(fields) != 5 or not fields[0].startswith(':')
                or fields[0][1:] not in {'000000', '100644'}
                or fields[1] not in {'000000', '100644'}):
            return False
    return True


def checked(root, request, advanced):
    accepted, base = request['accepted'], request['old_base']
    owned = landing.paths(root, base, accepted)
    if not only_docs(root, base, accepted):
        raise ValueError('Documentation fast path requires only plain Markdown documentation')
    landing.assert_disjoint(owned, landing.paths(root, base, advanced))
    original = landing.raw_diff(root, base, accepted)
    if advanced != base:
        landing.rebase(root, advanced, False)
    candidate = landing.git(root, 'rev-parse', 'HEAD').decode().strip()
    if landing.raw_diff(root, advanced, candidate) != original or not only_docs(root, advanced, candidate):
        raise ValueError('Documentation composition changed the reviewed patch')
    landing.git(root, '-c', 'core.whitespace=-blank-at-eol,blank-at-eof,space-before-tab', 'diff', '--check', advanced, candidate, '--')
    source = {'commit': candidate, 'clean': True}
    if landing.git(root, 'status', '--porcelain', '--untracked-files=all'):
        raise ValueError('Documentation composition changed its source')
    return dict(schema='secode.docs-landing.v1', accepted=accepted, old_base=base,
                new_base=advanced, candidate=candidate, source=source,
                finished_unix=time.time())


def compose(root, request):
    if request['full'] or 'retained_full' in request:
        raise ValueError('Documentation fast path cannot waive a requested full gate')
    landing.source(root, request['accepted'], request['branch'])
    landing.git(root, 'merge-base', '--is-ancestor', request['old_base'], request['accepted'])
    if not only_docs(root, request['old_base'], request['accepted']):
        raise ValueError('Documentation fast path requires only plain Markdown documentation')
    landing.heavy.run(['git', 'fetch', '--no-tags', '--', request['canonical'], 'main'],
                      cwd=root, check=True, timeout=120)
    advanced = landing.git(root, 'rev-parse', 'FETCH_HEAD').decode().strip()
    landing.git(root, 'merge-base', '--is-ancestor', request['old_base'], advanced)
    if advanced == request['old_base']:
        record = checked(root, request, advanced)
    else:
        with landing.detached_source(root, request['accepted']) as checkout:
            record = checked(checkout, request, advanced)
    landing.source(root, request['accepted'], request['branch'])
    path = Path(request['receipt'])
    with path.open('x', encoding='utf-8') as output:
        json.dump(record, output)
        output.write('\n')
    landing.heavy.run(['git', 'update-ref', 'refs/heads/' + landing.export_branch(request),
                       record['candidate'], '0'*40], cwd=root, check=True, timeout=60)
    print('Documentation checks PASS; no build reservation needed', flush=True)
    return record


def verify(root, request):
    if request['full'] or 'retained_full' in request:
        raise ValueError('Documentation fast path cannot waive a requested full gate')
    landing.source(root, request['accepted'], request['branch'])
    landing.git(root, 'merge-base', '--is-ancestor', request['old_base'], request['accepted'])
    path = Path(request['receipt'])
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > 4096:
        raise ValueError('Documentation receipt must be a bounded regular file')
    record = json.loads(path.read_text())
    if (record.get('schema') != 'secode.docs-landing.v1' or
            any(record.get(key) != request[key] for key in ('accepted', 'old_base', 'new_base', 'candidate'))
            or not 0 <= time.time() - record['finished_unix'] <= 86400):
        raise ValueError('Documentation receipt differs from its source or base')
    exported = landing.git(root, 'rev-parse', 'refs/heads/' + landing.export_branch(request)).decode().strip()
    if exported != request['candidate']:
        raise ValueError('Documentation export differs from its qualified candidate')
    # Verify the exact reviewed patch and whitespace again; no Cargo metadata or
    # retained full-gate receipt is consumed by this documentation-only proof.
    with landing.detached_source(root, request['candidate']) as checkout:
        landing.git(checkout, 'merge-base', '--is-ancestor', request['new_base'], request['candidate'])
        if (not only_docs(checkout, request['old_base'], request['accepted']) or
                not only_docs(checkout, request['new_base'], request['candidate']) or
                landing.raw_diff(checkout, request['old_base'], request['accepted']) !=
                landing.raw_diff(checkout, request['new_base'], request['candidate'])):
            raise ValueError('Documentation proof changed its reviewed patch')
        landing.assert_disjoint(landing.paths(checkout, request['old_base'], request['accepted']),
                                landing.paths(checkout, request['old_base'], request['new_base']))
        landing.git(checkout, '-c', 'core.whitespace=-blank-at-eol,blank-at-eof,space-before-tab', 'diff', '--check', request['new_base'], request['candidate'], '--')
        if landing.source(checkout, request['candidate'], '') != record.get('source'):
            raise ValueError('Documentation proof source changed')
    landing.source(root, request['accepted'], request['branch'])
    print('Documentation-only composition verified', flush=True)
    return record
