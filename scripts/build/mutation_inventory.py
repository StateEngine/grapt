#!/usr/bin/env python3
"""Every write door into the store, resolved by the compiler, checked in as JSON.

Phase 1 of the gateway work (foundation phase 2): inventory, never convert.
The one inventory of mutation entry points, covering the full door set from
the phase 2 gate review: transaction constructors inside and outside the
store, the store's own transaction-owning methods, raw SQL on any handle
(including SQL that controls the transaction), writable connection opens,
callbacks handed a handle (projectors, read projections, migrations), the
first-contact paths, and every function that receives a raw handle.

Resolution comes from the compiler, not from text. The script writes a
throwaway clippy configuration that marks each door as a
`disallowed-methods` or `disallowed-types` path and runs
`cargo clippy --workspace --all-targets --message-format=json`. Clippy
resolves every use by type, so calls through `Deref`, aliases, re-exports,
qualified syntax and paths passed as values are all found. Text is only
used afterwards, to name the enclosing function and to read SQL literals.

Scope comes from the compiler too: with `--all-targets` a library or binary
target is checked twice (normal and test), so a line reported twice is
production code and a line reported once is compiled only under cfg(test).
Integration-test, bench and example targets are their own scopes. A
once-reported line whose path does not look like test code is
`scope-review`, never silently production or test.

Every path into a transaction is a site: a constructor, raw SQL `BEGIN`,
and every call to a store function (public or private) that opens one
itself. The backstop reads no cfg at all: every door-name token in a
production file that no compiler run reported is listed in
`uncompiled_review`. Every site stays `needs_review` until a person
confirms its shape.

The script fails closed; every unknown is a site or a failure, never
silence. It refuses a compiler stream without a successful build-finished
message or without an artifact for every workspace member, a door path
clippy cannot resolve in the store, an unreadable file, a site it cannot
place in a function, a store owner its pre-scan missed, and an inventory
with no production sites.

usage: mutation_inventory.py --config CLIPPY.TOML --diagnostics P=FILE ... [--check]
       mutation_inventory.py --print-config [--diagnostics P=FILE ...]
  --run-cargo P[=T]   run clippy itself for platform label P (optional --target T);
                      a label ending in +fixtures adds the gate's acceptance features
  --diagnostics P=FILE  read clippy JSON captured earlier for platform label P (repeatable)
  --config FILE       the clippy.toml the diagnostics were produced with (required)
  --print-config      print the clippy.toml for the next run and exit; given earlier
                      diagnostics, owners the compiler found become doors too
  --check             regenerate in memory and fail on drift from the JSON

No gate job runs --check or --run-cargo: a whole-workspace clippy is an
explicit, opt-in run (locally or in a heavy job), and --check compares
against saved diagnostics. The hosted helpers job runs only the hermetic
unit tests, which use fixture diagnostics and start no process.
"""
import argparse
import json
import os
import posixpath
import re
import subprocess
import sys
import tempfile
from collections import Counter, defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_features  # noqa: E402  (the gate's acceptance feature list)
from mutation_source import (  # noqa: F401  (re-exported for callers and tests)
    ANY_MOD, TESTISH, TEST_ITEM, TEST_MOD, TRAIT_HEAD, InventoryError, Source, body_open, brace_pairs,
    child_dir, declared, mark_includes, mark_test_files, mask, module_of)

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name('mutation-inventory.json')
STORE = 'crates/grapt-store/src/'
REASON = 'mutation inventory door'

# Transaction constructors: each use is one transaction entry (a site).
TX_OPEN = {
    'grapt_store::EventStore::governed_transaction': 'full',
    'grapt_store::EventStore::stream_transaction': 'normal',
    'grapt_store::EventStore::append': 'full',
    'grapt_store::EventStore::fixture_rows': 'fixture',
    'rusqlite::Connection::transaction': 'raw',
    'rusqlite::Connection::transaction_with_behavior': 'raw',
    'rusqlite::Connection::unchecked_transaction': 'raw',
    'rusqlite::Connection::savepoint': 'raw',
    'rusqlite::Connection::savepoint_with_name': 'raw',
    'rusqlite::Transaction::new': 'raw',
    'rusqlite::Transaction::new_unchecked': 'raw',
}
TX_END = ('rusqlite::Transaction::commit', 'rusqlite::Transaction::rollback',
          'rusqlite::Transaction::finish', 'grapt_store::GovernedTransaction::commit')
FIRST_CONTACT = ('grapt_store::migration::apply', 'grapt_store::migration::apply_with_clock',
                 'grapt_store::stamp_and_prepare', 'grapt_store::epoch::stamp_and_prepare')
HANDLE_SOURCE = ('grapt_store::EventStore::connection', 'grapt_store::EventStore::append_within')
SQL = ('rusqlite::Connection::execute', 'rusqlite::Connection::execute_batch',
       'rusqlite::Connection::prepare', 'rusqlite::Connection::prepare_cached',
       'rusqlite::Connection::prepare_with_flags', 'rusqlite::Connection::query_row',
       'rusqlite::Connection::query_row_and_then', 'rusqlite::Connection::pragma_update',
       'rusqlite::Connection::pragma_update_and_check', 'rusqlite::Connection::pragma_query',
       'rusqlite::Connection::pragma_query_value', 'rusqlite::Connection::pragma',
       'rusqlite::Connection::backup', 'rusqlite::Connection::restore',
       'grapt_store::governed::WriteGate::execute')
OPEN = ('rusqlite::Connection::open', 'rusqlite::Connection::open_with_flags',
        'rusqlite::Connection::open_with_flags_and_vfs', 'rusqlite::Connection::open_in_memory',
        'rusqlite::Connection::open_in_memory_with_flags')
HANDLE_TYPES = ('rusqlite::Connection', 'rusqlite::Transaction', 'grapt_store::GovernedTransaction')
CALLBACK_TYPES = ('grapt_store::Projector', 'grapt_store::commit_projection::CommittedReadProjection',
                  'grapt_store::migration::Migration')
# Text markers for the store pre-scan: a store function containing one of these
# opens or ends a transaction itself and is a candidate store-owned door.
OWNER_MARK = re.compile(
    r'\b(?:governed_transaction|stream_transaction|transaction_with_behavior|unchecked_transaction|'
    r'begin_with_policy|begin_with_projection)\s*\(|\.transaction\s*\(|Transaction::new|'
    r'FixtureRows::begin')
HANDLE_RETURN = re.compile(r'\b(?:GovernedTransaction|Transaction|FixtureRows|Savepoint)\b')
HANDLE_IMPLS = ('GovernedTransaction', 'FixtureRows')
BEGIN_SQL = re.compile(r'^\s*(BEGIN|SAVEPOINT)\b', re.I | re.M)
SQL_KIND = (('control-begin', re.compile(r'^\s*(BEGIN|SAVEPOINT)\b', re.I)),
            ('control-end', re.compile(r'^\s*(COMMIT|END|ROLLBACK|RELEASE)\b', re.I)),
            ('ddl', re.compile(r'^\s*(CREATE|ALTER|DROP|REINDEX|VACUUM|ATTACH|DETACH)\b', re.I)),
            ('pragma-write', re.compile(r'^\s*PRAGMA\s+[\w.]+\s*=', re.I)),
            ('dml', re.compile(r'^\s*(INSERT|UPDATE|DELETE|REPLACE)\b', re.I)),
            ('read', re.compile(r'^\s*(SELECT|WITH|PRAGMA|EXPLAIN|VALUES)\b', re.I)))
MUTATING_IN_READ = re.compile(r'\b(INSERT|UPDATE|DELETE|REPLACE)\b|\bRETURNING\b', re.I)
# ---------------------------------------------------------------- doors

def store_owners(sources, seeds=()):
    """Text pre-scan of grapt-store: functions that open or end a transaction
    themselves, closed transitively over same-crate bare-name calls, plus
    `seeds`: functions the compiler showed calling an owner (owner_fixpoint)."""
    owners, bodies = {}, {}
    for rel, src in sources.items():
        if not rel.startswith(STORE):
            continue
        for name, head, o, c, is_pub in src.fns:
            if src.in_cfg_test(head) or TESTISH.search(rel[len(STORE):]):
                continue
            body = src.code[o:c]
            key = (rel, name, head)
            bodies[key] = (body, src, is_pub)
            literal_begin = any(o <= a < c and BEGIN_SQL.search(v) for a, _b, v in src.strings)
            typ = src.enclosing_impl(head)
            qualified = '%s::%s' % (getattr(src, 'module', module_of(rel)), (typ + '::' if typ else '') + name)
            if OWNER_MARK.search(body) or literal_begin:
                owners[key] = 'direct'
            elif qualified in seeds:
                owners[key] = 'compiler'
    name_count = Counter(k[1] for k in bodies)
    changed = True
    while changed:
        changed = False
        # Public or private: a store function calling any owner opens a
        # transaction too. Unique names only; the compiler check in
        # verify_owners refuses any owner this text closure misses.
        names = {k[1] for k in owners if name_count[k[1]] == 1 and k[1] not in GENERIC_NAMES}
        for key, (body, _src, _pub) in bodies.items():
            if key in owners:
                continue
            if any(re.search(r'\b%s\s*\(' % re.escape(n), body) for n in names):
                owners[key] = 'transitive'
                changed = True
    doors, every = {}, set()
    for (rel, name, head), how in owners.items():
        body, src, is_pub = bodies[(rel, name, head)]
        typ = src.enclosing_impl(head)
        mod = getattr(src, 'module', module_of(rel))
        owner = '%s::%s' % (mod, (typ + '::' if typ else '') + name)
        every.add(owner)
        fn = next(f for f in src.fns if f[1] == head)
        returns = src.code[head:fn[2]].split('->', 1)[1] if '->' in src.code[head:fn[2]] else ''
        handle = HANDLE_RETURN.search(returns) or (typ in HANDLE_IMPLS and re.search(r'\bSelf\b', returns))
        # Private owners are doors too: clippy resolves them by path, and a
        # private wrapper's callers each open a transaction.
        paths = (['grapt_store::%s::%s' % (typ, name), '%s::%s::%s' % (mod, typ, name)] if typ else
                 ['grapt_store::%s' % name, '%s::%s' % (mod, name)])
        trait = src.enclosing_trait(head)
        if trait:
            # A trait-impl method is called through the trait, so its door is
            # the trait method (every impl of it, conservatively).
            paths += ['grapt_store::%s::%s' % (trait, name)] + [
                '%s::%s::%s' % (getattr(t, 'module', module_of(r)), trait, name)
                for r, t in sources.items() if r.startswith(STORE)
                and any(m.group(1) == trait for m in TRAIT_HEAD.finditer(t.code))]
        for p in paths:
            doors.setdefault(p, {'owner': owner, 'file': rel, 'line': src.line_of(head), 'how': how,
                                 'public': is_pub, 'kind': 'constructor' if handle else 'operation'})
    return doors, every


def door_table(owner_doors):
    table = {}
    for p, cls in TX_OPEN.items():
        table[p] = ('tx-open', cls)
    for p in TX_END:
        table[p] = ('tx-end', None)
    for p in FIRST_CONTACT:
        table[p] = ('first-contact', 'raw')
    for p in HANDLE_SOURCE:
        table[p] = ('handle-source', None)
    for p in SQL:
        table[p] = ('sql', None)
    for p in OPEN:
        table[p] = ('open', None)
    for p in owner_doors:
        table.setdefault(p, ('store-owner', None))
    return table


def canonical(path, table):
    """Clippy names a use by whichever configured path matched; fold a store
    module path (`grapt_store::write::event_log::EventStore::x`) onto the root
    re-export (`grapt_store::EventStore::x`) when that is a named door."""
    if not path.startswith('grapt_store::'):
        return path
    parts = path.split('::')
    for tail in (2, 1):
        root = '::'.join(['grapt_store'] + parts[-tail:])
        if root != path and root in table and table[root][0] != 'store-owner':
            return root
    return path


def clippy_config(owner_doors):
    base = (ROOT / 'clippy.toml').read_text()
    # allow-invalid stays off: clippy then names every door path it cannot
    # resolve, and the inventory refuses a door that matches nothing.
    methods = ''.join('    { path = "%s", reason = "%s" },\n' % (p, REASON)
                      for p in sorted(door_table(owner_doors)))
    types = ''.join('    { path = "%s", reason = "%s" },\n' % (p, REASON)
                    for p in HANDLE_TYPES + CALLBACK_TYPES)
    if 'disallowed-methods = [\n' not in base or 'disallowed-types' in base:
        raise InventoryError('clippy.toml shape changed; update clippy_config')
    return base.replace('disallowed-methods = [\n', 'disallowed-methods = [\n' + methods, 1) + \
        '\ndisallowed-types = [\n' + types + ']\n'


def run_clippy(config, target=None, fixtures=False):
    with tempfile.TemporaryDirectory() as conf:
        Path(conf, 'clippy.toml').write_text(config)
        cmd = ['cargo', 'clippy', '--workspace', '--all-targets', '--message-format=json']
        if target:
            cmd += ['--target', target]
        if fixtures:
            cmd += test_features.arguments(None)
        cmd += ['--', '-A', 'clippy::all', '-W', 'clippy::disallowed_methods',
                '-W', 'clippy::disallowed_types']
        env = dict(os.environ, CLIPPY_CONF_DIR=conf)
        done = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
        if done.returncode != 0:
            raise InventoryError('clippy failed:\n' + done.stderr[-4000:])
        return done.stdout.splitlines()


# ---------------------------------------------------------------- diagnostics

PATH_IN_MESSAGE = re.compile(r'use of a disallowed (method|type) `([^`]+)`')
UNREACHABLE = re.compile(r'^`([^`]+)` does not refer to (?:a|an) (?:reachable|existing) ')


def workspace_members():
    text = (ROOT / 'Cargo.toml').read_text()
    block = re.search(r'^\[workspace\][^\[]*?^members\s*=\s*\[(.*?)\]', text, re.S | re.M)
    if block is None:
        raise InventoryError('workspace members not found in Cargo.toml')
    return sorted(re.findall(r'"([^"]+)"', block.group(1)))


def require_complete(platform, lines, members):
    """A compiler stream counts only if cargo finished successfully and every
    workspace member produced an artifact: an empty, truncated or failed
    stream would otherwise freeze a baseline with doors silently missing."""
    finished, built = None, set()
    for line in lines:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get('reason') == 'build-finished':
            finished = msg.get('success')
        elif msg.get('reason') == 'compiler-artifact':
            manifest = msg.get('manifest_path', '')
            for member in members:
                if manifest.endswith('/' + member + '/Cargo.toml'):
                    built.add(member)
    if finished is not True:
        raise InventoryError('%s: compiler stream did not finish successfully (%s)' %
                             (platform, 'no build-finished message' if finished is None else 'success=false'))
    missing = [m for m in members if m not in built]
    if missing:
        raise InventoryError('%s: no compiler artifact for %d workspace members, e.g. %s' %
                             (platform, len(missing), ', '.join(missing[:5])))


def unresolved_paths(lines):
    """Door paths clippy could not resolve while linting the store itself."""
    found = set()
    for line in lines:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get('reason') != 'compiler-message' or msg.get('target', {}).get('name') != 'grapt_store':
            continue
        m = UNREACHABLE.search(msg['message'].get('message', ''))
        if m:
            found.add(m.group(1))
    return found


def workspace_span(span):
    """Follow macro expansions out to the first span inside the workspace."""
    while span is not None:
        name = span['file_name']
        if name.startswith('crates/') and not span['file_name'].startswith('/'):
            return span
        exp = span.get('expansion')
        span = exp['span'] if exp else None
    return None


def occurrences(lines, platform):
    """Parse clippy JSON into occurrence records keyed by exact span."""
    seen = Counter()
    info = {}
    for line in lines:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get('reason') != 'compiler-message':
            continue
        diag = msg['message']
        code = (diag.get('code') or {}).get('code')
        if code not in ('clippy::disallowed_methods', 'clippy::disallowed_types'):
            continue
        m = PATH_IN_MESSAGE.search(diag['message'])
        primary = [s for s in diag['spans'] if s['is_primary']]
        if not m or not primary:
            raise InventoryError('unreadable diagnostic: %s' % diag['message'][:200])
        span = workspace_span(primary[0])
        if span is None:
            continue  # a door used inside a dependency's own macro body
        target = msg['target']
        key = (posixpath.normpath(span['file_name']), span['line_start'], span['column_start'], m.group(2))
        seen[key] += 1
        info[key] = {'kind': m.group(1), 'target_kind': target['kind'][0],
                     'target_tests': target.get('test', False), 'platform': platform}
    return seen, info


def scope_of(rel, count, meta, src, offset):
    kind = meta['target_kind']
    if kind in ('test', 'bench'):
        return 'test'
    if kind == 'example':
        return 'example'
    if '/tests/probes/' in rel:
        return 'probe'
    by_text = bool(TESTISH.search(rel.split('/src/', 1)[-1])) or src.in_cfg_test(offset) or \
        getattr(src, 'test_file', False)
    if not meta['target_tests']:
        return 'test' if by_text else 'production'
    by_count = 'production' if count >= 2 else 'test'
    if by_count == 'test' and not by_text:
        return 'scope-review'
    if by_count == 'production' and by_text:
        return 'scope-review'
    return by_count


TRIGGER = re.compile(r'^\s*CREATE\s+(?:TEMP(?:ORARY)?\s+)?TRIGGER\b', re.I)


def statements_of(text):
    """Split on `;`, keeping a CREATE TRIGGER ... BEGIN ... END body whole."""
    out, pending = [], None
    for part in re.split(r';\s*', text):
        if not part.strip():
            continue
        if pending is not None:
            pending += '; ' + part
            if re.match(r'^\s*END\b', part, re.I):
                out.append(pending)
                pending = None
            continue
        if TRIGGER.search(part):
            pending = part
            continue
        out.append(part)
    if pending is not None:
        out.append(pending)
    return out


def classify_sql(text):
    if text is None:
        return 'unresolved'
    statements = statements_of(text)
    kinds = []
    for s in statements or [text]:
        kind = next((k for k, rx in SQL_KIND if rx.search(s)), 'unresolved')
        if kind == 'read' and MUTATING_IN_READ.search(s):
            kind = 'dml'
        kinds.append(kind)
    order = ('control-begin', 'control-end', 'ddl', 'pragma-write', 'dml', 'unresolved', 'read')
    return next(k for k in order if k in kinds)


# ---------------------------------------------------------------- assembly

def load_sources(paths):
    sources = {}
    for rel in paths:
        try:
            sources[rel] = Source(rel, (ROOT / rel).read_text())
        except (OSError, UnicodeDecodeError, InventoryError) as error:
            raise InventoryError('%s: %s' % (rel, error))
    mark_test_files(sources)
    mark_includes(sources)
    return sources


def rust_files():
    done = subprocess.run(['git', 'ls-files', 'crates/*.rs', 'crates/**/*.rs'], cwd=ROOT,
                          capture_output=True, text=True, check=True)
    return sorted(set(done.stdout.split()))


def build(diagnostic_sets, sources, owner_doors):
    table = door_table(owner_doors)
    merged, meta = Counter(), {}
    platforms = defaultdict(set)
    for platform, lines in diagnostic_sets:
        seen, info = occurrences(lines, platform)
        folded = Counter()
        for (rel, line, col, path), count in seen.items():
            key = (rel, line, col, canonical(path, table))
            folded[key] += count
            meta.setdefault(key, info[(rel, line, col, path)])
        for key, count in folded.items():
            merged[key] = max(merged[key], count)
            platforms[key].add(platform)
    records = []
    for key in sorted(merged):
        rel, line, col, path = key
        src = sources.get(rel)
        if src is None:
            raise InventoryError('diagnostic in a file outside the git tree: %s' % rel)
        offset = src.offset(line, col)
        fn = src.enclosing_fn(offset)
        typ = src.enclosing_impl(offset)
        rec = {'file': rel, 'line': line, 'path': path,
               'scope': scope_of(rel, merged[key], meta[key], src, offset),
               'platforms': sorted(platforms[key]),
               'function': '%s::%s' % (getattr(src, 'module', module_of(rel)),
                                       ((typ + '::') if typ and fn else '') + fn[0])
               if fn else getattr(src, 'module', module_of(rel)) + '::<item>'}
        if meta[key]['kind'] == 'type':
            rec['door'] = 'callback' if path in CALLBACK_TYPES else 'handle-type'
            rec['position'] = ('signature' if src.in_signature(offset) else
                               'impl-header' if fn is None else 'body')
        else:
            door, cls = table.get(path, ('unknown', None))
            rec['door'] = door
            if door == 'tx-open':
                rec['class'] = cls
            if door == 'sql':
                rec['sql'] = classify_sql(src.first_string_after(offset))
            if door == 'open':
                call = src.code[offset:offset + 300].split(';', 1)[0]
                # Read-only only on an explicit flag; in-memory and default opens write.
                rec['flags'] = 'read-only' if 'SQLITE_OPEN_READ_ONLY' in src.text[offset:offset + len(call)] \
                    else 'writable'
            if door == 'store-owner':
                rec['owner'] = owner_doors[path]['owner']
                rec['owner_kind'] = owner_doors[path].get('kind', 'operation')
        records.append(rec)
    return records


def sites_from(records):
    """One site per transaction entry: tx-open and first-contact uses, raw SQL
    `BEGIN`, and every call to a store function that opens a transaction
    itself (a constructor handing one back, or an operation committing inside),
    so no path into a transaction lacks an identity. Ordinals are per
    function, in source order."""
    entries = [r for r in records if r['door'] in ('tx-open', 'first-contact', 'store-owner') or
               (r['door'] == 'sql' and r.get('sql') == 'control-begin')]
    by_fn = defaultdict(list)
    for r in entries:
        by_fn[r['function']].append(r)
    ends = Counter(r['function'] for r in records if r['door'] == 'tx-end')
    sites = []
    for function in sorted(by_fn):
        rows = sorted(by_fn[function], key=lambda r: (r['file'], r['line']))
        classes = {r.get('class') for r in rows if r['door'] == 'tx-open'}
        for k, r in enumerate(rows, 1):
            shape = 'single' if len(rows) == 1 else 'multi:%d' % len(rows)
            if {'full', 'normal'} <= classes and r.get('class') in ('full', 'normal'):
                shape = 'branch'
            site = {
                'id': '%s#%d' % (function, k), 'file': r['file'], 'line': r['line'],
                'door': r['path'], 'submission': submission_of(r),
                'class': r.get('class', 'raw' if r['door'] != 'sql' else 'raw-sql'),
                'shape': shape, 'explicit_ends_in_fn': ends.get(function, 0),
                'scope': r['scope'], 'platforms': r['platforms'], 'needs_review': True}
            if r['door'] == 'store-owner':
                site['class'] = 'via-' + r.get('owner_kind', 'operation')
                site['via'] = r['owner']
            sites.append(site)
    return sites


def submission_of(r):
    if r['door'] == 'first-contact':
        return 'first-contact'
    if r['door'] == 'store-owner':
        return 'store-owner-call'
    if r['path'] == 'grapt_store::EventStore::fixture_rows':
        return 'fixture'
    if r['file'].startswith(STORE):
        return 'store-op'
    if r['door'] == 'sql' or r.get('class') == 'raw':
        return 'raw'
    return 'legacy'


def verify_owners(records, listed):
    """Fail closed if the compiler saw a store function open a transaction,
    or call a store function that does, without the pre-scan listing it as
    an owner itself: owners are transitive, to a fixpoint."""
    missed = sorted({r['function'] for r in records
                     if r['file'].startswith(STORE) and r['scope'] == 'production'
                     and (r['door'] in ('tx-open', 'store-owner', 'first-contact', 'unknown') or
                          (r['door'] == 'sql' and r.get('sql') == 'control-begin'))
                     and r['function'] not in listed
                     and not r['function'].endswith('<item>')})
    return missed


def summary(records, sites):
    def count(rows, *keys):
        c = Counter(tuple(r.get(k) for k in keys) for r in rows)
        return {'/'.join(str(x) for x in k): v for k, v in sorted(c.items())}
    return {'sites_by_submission_scope': count(sites, 'submission', 'scope'),
            'sites_by_class': count([s for s in sites if s['scope'] == 'production'], 'class'),
            'occurrences_by_door_scope': count(records, 'door', 'scope'),
            'sql_by_kind_scope': count([r for r in records if r['door'] == 'sql'], 'sql', 'scope'),
            'opens_by_flags_scope': count([r for r in records if r['door'] == 'open'], 'flags', 'scope')}


def unresolved_doors(diagnostic_sets, owner_doors):
    """Fail closed on a door clippy cannot resolve in the store: a fixed door
    must resolve, and every store owner needs at least one resolvable path."""
    bad = set()
    for _platform, lines in diagnostic_sets:
        bad |= unresolved_paths(lines)
    fixed = [p for p in door_table({}) if p in bad] + [p for p in HANDLE_TYPES + CALLBACK_TYPES if p in bad]
    by_owner = defaultdict(list)
    for p, d in owner_doors.items():
        by_owner[d['owner']].append(p)
    owners = [o for o, paths in by_owner.items() if all(p in bad for p in paths)]
    return sorted(fixed), sorted(owners)


# Door names too common to read as doors in text. Each counts only when
# written with its door's own qualifier, the last two segments of the door
# path: `Connection::open(`, `Transaction::new(`, `Transaction::finish(`,
# `Connection::backup(`, `migration::apply(`, `EventStore::open(` and so on.
GENERIC_NAMES = ('new', 'open', 'finish', 'append', 'apply', 'begin', 'restore', 'backup')


def compiler_spans(diagnostic_sets):
    """Every span the compiler reported a door at, across all runs."""
    spans = defaultdict(list)
    for _platform, lines in diagnostic_sets:
        for line in lines:
            try:
                msg = json.loads(line)
            except ValueError:
                continue
            if msg.get('reason') != 'compiler-message':
                continue
            diag = msg['message']
            if (diag.get('code') or {}).get('code') not in ('clippy::disallowed_methods',
                                                             'clippy::disallowed_types'):
                continue
            for span in diag['spans']:
                if span['is_primary']:
                    s = workspace_span(span)
                    if s is not None:
                        spans[posixpath.normpath(s['file_name'])].append(
                            (s['line_start'], s['column_start'], s['line_end'], s['column_end']))
    return spans


def uncompiled_review(sources, spans=None, owner_doors=None):
    """The backstop, with no cfg reading at all: every door-name token in a
    production file, minus every span the compiler reported in any run.
    What is left is either code no run compiled (another platform, an
    un-enabled feature, a macro) or another function sharing a door's name;
    each is listed for review, never dropped."""
    doors = door_table(owner_doors or {})
    names = sorted({p.split('::')[-1] for p in doors} - set(GENERIC_NAMES))
    qualified = sorted({'::'.join(p.split('::')[-2:]) for p in doors
                        if p.split('::')[-1] in GENERIC_NAMES and p.count('::') >= 1})
    token = re.compile(r'(?:\.|::|\b)(' + '|'.join(map(re.escape, names)) + r')\s*\(|'
                       r'\b(' + '|'.join(map(re.escape, qualified)) + r')\s*\(')
    spans = spans or {}
    found = []
    for rel, src in sorted(sources.items()):
        if '/src/' not in rel or src.test_file or TESTISH.search(rel.split('/src/', 1)[-1]):
            continue
        for t in token.finditer(src.code):
            group = t.lastindex
            at = t.start(group)
            if re.search(r'\bfn\s*$', src.code[max(0, at - 12):at]) or src.in_cfg_test(at):
                continue  # a definition, or test code inside a production file
            line = src.line_of(at)
            col = at - src.line_at[line - 1] + 1
            if any((a, b) <= (line, col) <= (c, d) for a, b, c, d in spans.get(rel, ())):
                continue
            fn = src.enclosing_fn(at)
            found.append({'file': rel, 'line': line, 'token': t.group(group).split('::')[-1],
                          'function': fn[0] if fn else None, 'needs_review': True})
    return found


uncompiled_cfg = uncompiled_macos = uncompiled_review


def owner_fixpoint(diagnostic_sets, sources):
    """Owners are transitive: a store function the compiler shows calling an
    owner (or opening a transaction) is an owner too. Iterate over the same
    compiler output until no new owner appears. A new owner's own callers are
    only reported once it is a door, which is what configured_owners checks."""
    seeds = set()
    while True:
        owner_doors, every = store_owners(sources, seeds)
        records = build(diagnostic_sets, sources, owner_doors)
        missed = set(verify_owners(records, every))
        if not missed - seeds:
            if missed:
                raise InventoryError('owners the compiler found but the pre-scan cannot place: ' +
                                     ', '.join(sorted(missed)))
            return owner_doors, every, records
        seeds |= missed


ENTRY_DOORS = ('tx-open', 'first-contact', 'store-owner')
# The fixed first-contact entry points of r2 §2.4: opening a store (which
# stamps and prepares its schema) and the migration paths. Named here so the
# classification is reviewable, not inferred.
FIRST_CONTACT_ENTRY = re.compile(
    r'::(?:EventStore|BoundDatabaseFile|ProductionMigrationDriver)::'
    r'(?:open\w*|migrate\w*|recover_hot_journal_and_open|initialize_and_open)$|'
    r'::VmBudgetGuard::stamp_and_prepare$|::MigrationDriver::migrate$|::Migration::up$|'
    r'^grapt_store::migration(?:::authority)?::apply\w*$|^grapt_store::migrations::\w+::up$')


def first_contact_owners(records, owner_doors):
    """Owners whose transactions are first contact (schema stamping and
    migration when a store is opened, r2 §2.4): the first-contact doors
    themselves, and any owner whose every transaction entry goes through
    first contact. To a fixpoint."""
    owners = {d['owner'] for d in owner_doors.values()}
    first = (owners & set(FIRST_CONTACT)) | {o for o in owners if FIRST_CONTACT_ENTRY.search(o)}
    entries = defaultdict(list)
    for r in records:
        if r['scope'] == 'production' and (r['door'] in ENTRY_DOORS or
                                           (r['door'] == 'sql' and r.get('sql') == 'control-begin')):
            entries[r['function']].append(r)
    changed = True
    while changed:
        changed = False
        for owner in owners - first:
            rows = entries.get(owner)
            if rows and all(r['door'] == 'first-contact' or r.get('owner') in first for r in rows):
                first.add(owner)
                changed = True
    return first


def classify_first_contact(sites, first):
    """A site is first contact when it calls a first-contact owner or sits in
    one: phase 2 routes these through the fixed open paths, not
    LegacyTransition, so they stay out of the legacy ceiling."""
    for site in sites:
        function = site['id'].rsplit('#', 1)[0]
        if site.get('via') in first or function in first:
            site['submission'] = 'first-contact'
    return sites


CONFIG_PATH = re.compile(r'\{ path = "([^"]+)", reason = "%s" \}' % re.escape(REASON))


def configured_owners(config_text, owner_doors):
    """Every owner must have had a door in the clippy configuration the
    diagnostics were produced with; otherwise its callers were never reported."""
    configured = set(CONFIG_PATH.findall(config_text))
    by_owner = defaultdict(list)
    for path, door in owner_doors.items():
        by_owner[door['owner']].append(path)
    return sorted(o for o, paths in by_owner.items() if not configured & set(paths))


def inventory(diagnostic_sets, config_text=None):
    members = workspace_members()
    for platform, lines in diagnostic_sets:
        require_complete(platform, lines, members)
    if config_text is None:
        raise InventoryError('the clippy configuration the diagnostics were produced with is required')
    sources = load_sources(rust_files())
    owner_doors, every_owner, records = owner_fixpoint(diagnostic_sets, sources)
    fixed, owners = unresolved_doors(diagnostic_sets, owner_doors)
    if fixed or owners:
        raise InventoryError('door paths clippy cannot resolve: ' + ', '.join(fixed + owners))
    unconfigured = configured_owners(config_text, owner_doors)
    if unconfigured:
        raise InventoryError('owners that were not doors in the clippy run (regenerate the config with '
                             '--print-config and these --diagnostics, then rerun clippy): ' +
                             ', '.join(unconfigured))
    unknown = [r for r in records if r.get('door') == 'unknown']
    if unknown:
        raise InventoryError('uses of unclassified door paths: ' +
                             ', '.join(sorted({r['path'] for r in unknown})))
    first = first_contact_owners(records, owner_doors)
    sites = classify_first_contact(sites_from(records), first)
    if not any(s['scope'] == 'production' for s in sites):
        raise InventoryError('no production sites: the compiler stream cannot be complete')
    unplaced = [r for r in records if r['function'].endswith('<item>') and r['door'] not in
                ('handle-type', 'callback')]
    if unplaced:
        raise InventoryError('occurrences outside any function: ' +
                             ', '.join('%s:%d' % (r['file'], r['line']) for r in unplaced[:10]))
    production = [r for r in records if r['scope'] == 'production']
    return {
        'comment': __doc__.split('\n\n', 2)[1].replace('\n', ' '),
        'platforms': sorted({p for p, _ in diagnostic_sets}),
        'store_owners': [dict(path=p, first_contact=d['owner'] in first, **d)
                         for p, d in sorted(owner_doors.items())],
        'summary': summary(records, sites),
        'sites': [s for s in sites if s['scope'] == 'production'],
        'occurrences': production,
        'non_production_sites': aggregate(s for s in sites if s['scope'] != 'production'),
        'non_production_occurrences': aggregate(r for r in records if r['scope'] != 'production'),
        'retired': [],
        'uncompiled_review': uncompiled_review(sources, compiler_spans(diagnostic_sets), owner_doors),
    }


def aggregate(rows):
    """Test, probe and fixture uses, counted per file, door and scope: step 5
    adapts their setup, so the inventory names where they are, not each line."""
    c = Counter((r['file'], r.get('door'), r['path'] if 'path' in r else r['door'], r['scope'])
                for r in rows)
    return [{'file': f, 'door': d, 'path': p, 'scope': sc, 'count': n}
            for (f, d, p, sc), n in sorted(c.items())]


def render(result):
    """Stable JSON with one record per line, so diffs show sites, not braces."""
    out = ['{']
    keys = sorted(result)
    for i, key in enumerate(keys):
        value = result[key]
        tail = ',' if i < len(keys) - 1 else ''
        if isinstance(value, list) and value and isinstance(value[0], dict):
            out.append(' %s: [' % json.dumps(key))
            for j, item in enumerate(value):
                out.append('  %s%s' % (json.dumps(item, sort_keys=True), ',' if j < len(value) - 1 else ''))
            out.append(' ]' + tail)
        else:
            out.append(' %s: %s%s' % (json.dumps(key), json.dumps(value, sort_keys=True), tail))
    out.append('}')
    return '\n'.join(out) + '\n'


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n', 1)[0])
    parser.add_argument('--diagnostics', action='append', default=[],
                        help='PLATFORM=FILE of captured clippy JSON')
    parser.add_argument('--config', help='the clippy.toml the diagnostics were produced with')
    parser.add_argument('--run-cargo', action='append', default=[], metavar='PLATFORM[=TARGET]')
    parser.add_argument('--print-config', action='store_true')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args(argv)
    try:
        sets = []
        for item in args.diagnostics:
            platform, _, path = item.partition('=')
            sets.append((platform, Path(path).read_text().splitlines()))
        if args.print_config:
            # With earlier diagnostics, owners the compiler found become doors
            # for the next run; repeat until the inventory stops asking.
            sources = load_sources(rust_files())
            doors = owner_fixpoint(sets, sources)[0] if sets else store_owners(sources)[0]
            print(clippy_config(doors), end='')
            return 0
        config = Path(args.config).read_text() if args.config else None
        if args.run_cargo:
            if sets:
                raise InventoryError('give either --run-cargo or --diagnostics, not both')
            config = clippy_config(store_owners(load_sources(rust_files()))[0])
            for item in args.run_cargo:
                platform, _, target = item.partition('=')
                sets.append((platform, run_clippy(config, target or None, platform.endswith('+fixtures'))))
        if not sets:
            raise InventoryError('give --diagnostics PLATFORM=FILE (with --config) or --run-cargo PLATFORM')
        result = inventory(sets, config)
    except InventoryError as error:
        print('mutation inventory: %s' % error, file=sys.stderr)
        return 2
    rendered = render(result)
    if args.check:
        if not BASELINE.exists() or BASELINE.read_text() != rendered:
            print('mutation inventory: stale; regenerate with scripts/build/mutation_inventory.py')
            return 1
        print('mutation inventory: current (%d sites)' % len(result['sites']))
        return 0
    BASELINE.write_text(rendered)
    print('mutation inventory: %d production sites, %d production occurrences' %
          (len(result['sites']), len(result['occurrences'])))
    return 0


if __name__ == '__main__':
    sys.exit(main())
