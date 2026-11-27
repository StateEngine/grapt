import json
import unittest
from unittest.mock import patch

import mutation_inventory as inv


def diag(file, line, col, path, kind='method', target_kind='lib', tests=True, expansion=None,
         code='clippy::disallowed_methods'):
    span = {'file_name': file, 'line_start': line, 'column_start': col, 'line_end': line,
            'column_end': col + 10, 'is_primary': True, 'expansion': expansion}
    return json.dumps({
        'reason': 'compiler-message',
        'target': {'kind': [target_kind], 'name': 'x', 'test': tests},
        'message': {'code': {'code': code},
                    'message': 'use of a disallowed %s `%s`' % (kind, path),
                    'spans': [span]}})


STORE_FILE = 'crates/grapt-store/src/thing.rs'
STORE_SRC = '''\
pub struct Thing;

impl Thing {
    pub fn outer(&mut self) -> Result<(), E> {
        self.inner()
    }

    fn inner(&mut self) -> Result<(), E> {
        let tx = self.conn.transaction_with_behavior(Immediate)?;
        tx.commit()
    }

    pub fn reader(&self) -> u32 {
        // governed_transaction( mentioned in a comment only
        7
    }

    pub fn raw(&self) {
        self.conn.execute_batch("BEGIN IMMEDIATE; COMMIT;");
    }
}
'''

CONSUMER_FILE = 'crates/grapt-demo/src/lib.rs'
CONSUMER_SRC = '''\
pub fn both(store: &mut EventStore, identity: Option<u8>) -> Result<(), E> {
    let tx = if identity.is_some() {
        store.governed_transaction()?
    } else {
        store.stream_transaction()?
    };
    tx.execute("INSERT INTO t VALUES (1)", [])?;
    tx.commit()
}

pub fn two(store: &mut EventStore) -> Result<(), E> {
    let a = store.governed_transaction()?;
    a.commit()?;
    let b = store.governed_transaction()?;
    b.commit()
}

#[cfg(test)]
mod tests {
    fn helper(store: &mut EventStore) {
        let _ = store.fixture_rows();
    }
}
'''


class MaskTest(unittest.TestCase):
    def test_comments_strings_and_chars_are_blanked_but_lines_survive(self):
        text = 'a // {\nb /* { /* } */ } */ "x{" r#"y"}"# \'{\' &\'a T\n'
        code, strings = inv.mask(text)
        self.assertEqual(len(code), len(text))
        self.assertEqual(code.count('\n'), text.count('\n'))
        self.assertNotIn('{', code)
        self.assertNotIn('}', code)
        self.assertIn("'a T", code)
        self.assertEqual([s[2] for s in strings], ['x{', 'y"}'])

    def test_unterminated_raw_string_fails_closed(self):
        with self.assertRaises(inv.InventoryError):
            inv.mask('let s = r#"never closed;')

    def test_unbalanced_braces_fail_closed(self):
        with self.assertRaises(inv.InventoryError):
            inv.Source('x.rs', 'fn a() {')


class SourceTest(unittest.TestCase):
    def setUp(self):
        self.src = inv.Source(CONSUMER_FILE, CONSUMER_SRC)

    def test_functions_impls_and_test_regions(self):
        names = [f[0] for f in self.src.fns]
        self.assertEqual(names, ['both', 'two', 'helper'])
        helper = self.src.fns[2]
        self.assertTrue(self.src.in_cfg_test(helper[2]))
        self.assertFalse(self.src.in_cfg_test(self.src.fns[0][2]))
        store = inv.Source(STORE_FILE, STORE_SRC)
        inner = [f for f in store.fns if f[0] == 'inner'][0]
        self.assertEqual(store.enclosing_impl(inner[2]), 'Thing')
        self.assertFalse(inner[4])

    def test_signature_versus_body(self):
        head = self.src.fns[0]
        self.assertTrue(self.src.in_signature(head[1] + 10))
        self.assertFalse(self.src.in_signature(head[2] + 1))

    def test_first_string_after_stops_at_statement_end(self):
        at = CONSUMER_SRC.index('tx.execute')
        self.assertEqual(self.src.first_string_after(at), 'INSERT INTO t VALUES (1)')
        self.assertIsNone(self.src.first_string_after(CONSUMER_SRC.index('a.commit')))


class ModuleTest(unittest.TestCase):
    def test_module_paths(self):
        self.assertEqual(inv.module_of('crates/grapt-store/src/lib.rs'), 'grapt_store')
        self.assertEqual(inv.module_of('crates/grapt-store/src/write/event_log.rs'),
                         'grapt_store::write::event_log')
        self.assertEqual(inv.module_of('crates/grapt-store/src/mailbox/mod.rs'), 'grapt_store::mailbox')
        self.assertEqual(inv.module_of('crates/grapt-store/tests/mailbox.rs'), 'grapt_store::tests::mailbox')


class SqlTest(unittest.TestCase):
    def test_kinds(self):
        cases = {
            'BEGIN IMMEDIATE': 'control-begin', 'SAVEPOINT s': 'control-begin',
            'COMMIT': 'control-end', 'rollback': 'control-end',
            'CREATE TABLE IF NOT EXISTS t (a)': 'ddl', 'PRAGMA user_version = 3': 'pragma-write',
            'PRAGMA table_info(t)': 'read', 'INSERT INTO t VALUES (1)': 'dml',
            'SELECT 1': 'read', 'WITH x AS (SELECT 1) DELETE FROM t': 'dml',
            'UPDATE t SET a = 1 RETURNING a': 'dml',
            'BEGIN IMMEDIATE; ALTER TABLE a RENAME TO b; COMMIT;': 'control-begin',
            'SELECT 1; INSERT INTO t VALUES (2)': 'dml', 'frobnicate': 'unresolved',
        }
        for sql, want in cases.items():
            self.assertEqual(inv.classify_sql(sql), want, sql)
        self.assertEqual(inv.classify_sql(None), 'unresolved')


class DiagnosticTest(unittest.TestCase):
    def test_counts_dedupe_and_macro_expansion(self):
        outer = {'file_name': CONSUMER_FILE, 'line_start': 3, 'column_start': 15, 'expansion': None}
        lines = [
            diag(CONSUMER_FILE, 3, 15, 'grapt_store::EventStore::governed_transaction'),
            diag(CONSUMER_FILE, 3, 15, 'grapt_store::EventStore::governed_transaction'),
            diag('/deps/registry/m.rs', 9, 1, 'rusqlite::Connection::execute',
                 expansion={'span': {'file_name': CONSUMER_FILE, 'line_start': 7, 'column_start': 5,
                                     'expansion': None}}),
            diag('/deps/registry/m.rs', 9, 1, 'rusqlite::Connection::execute'),
            json.dumps({'reason': 'compiler-message', 'target': {'kind': ['lib']},
                        'message': {'code': {'code': 'unused'}, 'message': 'x', 'spans': []}}),
            'not json',
        ]
        del outer
        seen, info = inv.occurrences(lines, 'linux')
        key = (CONSUMER_FILE, 3, 15, 'grapt_store::EventStore::governed_transaction')
        self.assertEqual(seen[key], 2)
        self.assertIn((CONSUMER_FILE, 7, 5, 'rusqlite::Connection::execute'), seen)
        self.assertEqual(len(seen), 2)
        self.assertEqual(info[key]['platform'], 'linux')

    def test_unreadable_diagnostic_fails_closed(self):
        bad = json.dumps({'reason': 'compiler-message', 'target': {'kind': ['lib']},
                          'message': {'code': {'code': 'clippy::disallowed_methods'},
                                      'message': 'something else', 'spans': []}})
        with self.assertRaises(inv.InventoryError):
            inv.occurrences([bad], 'linux')


class ScopeTest(unittest.TestCase):
    def setUp(self):
        self.src = inv.Source(CONSUMER_FILE, CONSUMER_SRC)
        self.prod_at = self.src.fns[0][2] + 1
        self.test_at = self.src.fns[2][2] + 1

    def meta(self, kind='lib', tests=True):
        return {'target_kind': kind, 'target_tests': tests}

    def test_scopes(self):
        self.assertEqual(inv.scope_of(CONSUMER_FILE, 2, self.meta(), self.src, self.prod_at), 'production')
        self.assertEqual(inv.scope_of(CONSUMER_FILE, 1, self.meta(), self.src, self.test_at), 'test')
        self.assertEqual(inv.scope_of(CONSUMER_FILE, 1, self.meta(), self.src, self.prod_at), 'scope-review')
        self.assertEqual(inv.scope_of(CONSUMER_FILE, 2, self.meta(), self.src, self.test_at), 'scope-review')
        self.assertEqual(inv.scope_of(CONSUMER_FILE, 1, self.meta(tests=False), self.src, self.prod_at),
                         'production')
        self.assertEqual(inv.scope_of('crates/grapt-demo/tests/a.rs', 1, self.meta('test'), self.src, 0), 'test')
        self.assertEqual(inv.scope_of('crates/grapt-demo/tests/probes/p.rs', 1, self.meta('bin'), self.src, 0),
                         'probe')


class OwnerTest(unittest.TestCase):
    def test_direct_transitive_and_comment_only(self):
        sources = {STORE_FILE: inv.Source(STORE_FILE, STORE_SRC)}
        doors, every = inv.store_owners(sources)
        self.assertIn('grapt_store::thing::Thing::inner', every)
        self.assertIn('grapt_store::thing::Thing::outer', every)
        self.assertIn('grapt_store::thing::Thing::raw', every)
        self.assertNotIn('grapt_store::thing::Thing::reader', every)
        self.assertIn('grapt_store::Thing::outer', doors)
        # Private owners are doors too: their callers each open a transaction.
        self.assertIn('grapt_store::Thing::inner', doors)
        self.assertFalse(doors['grapt_store::Thing::inner']['public'])
        self.assertTrue(doors['grapt_store::Thing::outer']['public'])
        self.assertEqual(doors['grapt_store::Thing::outer']['how'], 'transitive')


class AssemblyTest(unittest.TestCase):
    def setUp(self):
        self.sources = {CONSUMER_FILE: inv.Source(CONSUMER_FILE, CONSUMER_SRC),
                        STORE_FILE: inv.Source(STORE_FILE, STORE_SRC)}
        self.doors, self.every = inv.store_owners(self.sources)

    def at(self, file, needle, path, count=2, **kw):
        text = CONSUMER_SRC if file == CONSUMER_FILE else STORE_SRC
        off = text.index(needle)
        line = text.count('\n', 0, off) + 1
        col = off - (text.rfind('\n', 0, off) + 1) + 1
        return [diag(file, line, col, path, **kw)] * count

    def records(self):
        lines = []
        lines += self.at(CONSUMER_FILE, 'governed_transaction()?\n    } else', 'grapt_store::EventStore::governed_transaction')
        lines += self.at(CONSUMER_FILE, 'stream_transaction', 'grapt_store::EventStore::stream_transaction')
        lines += self.at(CONSUMER_FILE, 'execute("INSERT', 'rusqlite::Connection::execute')
        lines += self.at(CONSUMER_FILE, 'governed_transaction()?;\n    a.commit', 'grapt_store::EventStore::governed_transaction')
        lines += self.at(CONSUMER_FILE, 'governed_transaction()?;\n    b.commit', 'grapt_store::EventStore::governed_transaction')
        lines += self.at(CONSUMER_FILE, 'fixture_rows', 'grapt_store::EventStore::fixture_rows', count=1)
        lines += self.at(STORE_FILE, 'transaction_with_behavior', 'rusqlite::Connection::transaction_with_behavior')
        lines += self.at(STORE_FILE, 'execute_batch', 'rusqlite::Connection::execute_batch')
        lines += self.at(CONSUMER_FILE, 'EventStore, identity', 'grapt_store::EventStore', kind='type',
                         code='clippy::disallowed_types')
        return inv.build([('linux', lines)], self.sources, self.doors)

    def test_sites_shapes_and_submissions(self):
        records = self.records()
        sites = {s['id']: s for s in inv.sites_from(records)}
        both = [s for s in sites.values() if s['id'].startswith('grapt_demo::both#')]
        self.assertEqual(sorted(s['class'] for s in both), ['full', 'normal'])
        self.assertTrue(all(s['shape'] == 'branch' for s in both))
        two = [s for s in sites.values() if s['id'].startswith('grapt_demo::two#')]
        self.assertEqual([s['shape'] for s in two], ['multi:2', 'multi:2'])
        fixture = [s for s in sites.values() if s['submission'] == 'fixture']
        self.assertEqual(len(fixture), 1)
        self.assertEqual(fixture[0]['scope'], 'test')
        store = [s for s in sites.values() if s['submission'] == 'store-op']
        self.assertEqual(sorted(s['id'] for s in store),
                         ['grapt_store::thing::Thing::inner#1', 'grapt_store::thing::Thing::raw#1'])
        self.assertTrue(all(s['needs_review'] for s in sites.values()))
        sql = [r for r in records if r['door'] == 'sql']
        self.assertEqual(sorted(r['sql'] for r in sql), ['control-begin', 'dml'])
        handle = [r for r in records if r['door'] == 'handle-type']
        self.assertEqual(handle[0]['position'], 'signature')

    def test_missed_owner_fails_closed(self):
        records = self.records()
        self.assertEqual(inv.verify_owners(records, self.every), [])
        self.assertEqual(inv.verify_owners(records, set()),
                         ['grapt_store::thing::Thing::inner', 'grapt_store::thing::Thing::raw'])


class RegressionTest(unittest.TestCase):
    def test_cfg_test_on_a_statement_does_not_swallow_the_function(self):
        src = inv.Source('crates/a/src/x.rs', 'pub fn read() {\n    #[cfg(test)]\n    count();\n    work();\n}\n'
                         '#[cfg(test)]\nfn only_test() {}\n')
        self.assertFalse(src.in_cfg_test(src.text.index('work')))
        self.assertTrue(src.in_cfg_test(src.text.index('only_test')))

    def test_test_status_follows_module_declarations(self):
        sources = {
            'crates/a/src/lib.rs': inv.Source('crates/a/src/lib.rs', 'mod x;\n#[cfg(test)]\nmod checks;\n'),
            'crates/a/src/x.rs': inv.Source('crates/a/src/x.rs', 'pub fn f() {}\n'),
            'crates/a/src/checks.rs': inv.Source('crates/a/src/checks.rs',
                                                 '#[path = "helper_file.rs"]\nmod helper;\nmod deeper;\n'),
            'crates/a/src/helper_file.rs': inv.Source('crates/a/src/helper_file.rs', 'fn h() {}\n'),
            'crates/a/src/checks/deeper.rs': inv.Source('crates/a/src/checks/deeper.rs', 'fn d() {}\n'),
        }
        inv.mark_test_files(sources)
        status = {rel: src.test_file for rel, src in sources.items()}
        self.assertEqual(status, {'crates/a/src/lib.rs': False, 'crates/a/src/x.rs': False,
                                  'crates/a/src/checks.rs': True, 'crates/a/src/helper_file.rs': True,
                                  'crates/a/src/checks/deeper.rs': True})

    def test_one_line_reported_under_two_configured_paths_counts_once_per_build(self):
        sources = {CONSUMER_FILE: inv.Source(CONSUMER_FILE, CONSUMER_SRC)}
        off = CONSUMER_SRC.index('stream_transaction')
        line = CONSUMER_SRC.count('\n', 0, off) + 1
        col = off - (CONSUMER_SRC.rfind('\n', 0, off) + 1) + 1
        lines = [diag(CONSUMER_FILE, line, col, 'grapt_store::EventStore::stream_transaction'),
                 diag(CONSUMER_FILE, line, col, 'grapt_store::write::event_log::EventStore::stream_transaction')]
        doors = {'grapt_store::write::event_log::EventStore::stream_transaction':
                 {'owner': 'grapt_store::write::event_log::EventStore::stream_transaction'}}
        records = inv.build([('linux', lines)], sources, doors)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]['path'], 'grapt_store::EventStore::stream_transaction')
        self.assertEqual(records[0]['door'], 'tx-open')
        self.assertEqual(records[0]['scope'], 'production')


    def test_macos_only_door_tokens_are_listed_not_dropped(self):
        sources = {
            'crates/a/src/lib.rs': inv.Source('crates/a/src/lib.rs',
                                              'use rusqlite::Connection;\n#[cfg(target_os = "macos")]\nmod mac;\n'
                                              '#[cfg(target_os = "macos")]\nfn inline(c: &Connection) { c.execute("x", []); }\n'
                                              'fn everywhere(c: &Connection) { c.execute("y", []); }\n'),
            'crates/a/src/mac.rs': inv.Source('crates/a/src/mac.rs',
                                              'use rusqlite::Connection;\nfn m(c: &Connection) { c.prepare("z"); }\n'),
        }
        inv.mark_test_files(sources)
        found = {(f['file'], f['token']) for f in inv.uncompiled_macos(sources)}
        self.assertEqual(found, {('crates/a/src/lib.rs', 'execute'), ('crates/a/src/mac.rs', 'prepare')})



class ReviewCompletenessTest(unittest.TestCase):
    def test_review_store_constructor_call_keeps_its_transaction_identity(self):
        file = 'crates/grapt-store/src/write/election.rs'
        text = ('impl EventStore {\n'
                '    pub fn elect_writer(&mut self) {\n'
                '        GovernedTransaction::begin_with_projection(&mut self.conn);\n'
                '    }\n}\n')
        sources = {
            file: inv.Source(file, text),
            'crates/grapt-store/src/governed.rs': inv.Source(
                'crates/grapt-store/src/governed.rs',
                'impl GovernedTransaction {\n'
                '    pub(crate) fn begin_with_projection(c: &mut Connection) {\n'
                '        c.transaction_with_behavior(Immediate);\n'
                '    }\n}\n'),
        }
        doors, _ = inv.store_owners(sources)
        at = text.index('begin_with_projection')
        line = text.count('\n', 0, at) + 1
        col = at - text.rfind('\n', 0, at)
        rows = inv.build([('linux', [diag(
            file, line, col,
            'grapt_store::governed::GovernedTransaction::begin_with_projection',
            tests=False)])], sources, doors)
        self.assertTrue(any(site['id'].startswith(
            'grapt_store::write::election::EventStore::elect_writer#')
                            for site in inv.sites_from(rows)),
                        'a store-owned BEGIN caller needs its own site identity')

    def test_review_incomplete_compiler_transcripts_refuse_inventory(self):
        sources = {
            CONSUMER_FILE: inv.Source(CONSUMER_FILE, CONSUMER_SRC),
            STORE_FILE: inv.Source(STORE_FILE, STORE_SRC),
        }
        partial = [diag(CONSUMER_FILE, 3, 15,
                        'grapt_store::EventStore::governed_transaction')] * 2
        cases = [[], partial, partial + [json.dumps(
            {'reason': 'build-finished', 'success': False})]]
        with patch.object(inv, 'rust_files', return_value=list(sources)), \
                patch.object(inv, 'load_sources', return_value=sources):
            for lines in cases:
                with self.subTest(records=len(lines)):
                    with self.assertRaises(inv.InventoryError):
                        inv.inventory([('linux', lines)])

    def test_review_in_memory_open_is_not_classified_read_only(self):
        text = 'pub fn open() { let db = Connection::open_in_memory(); }\n'
        at = text.index('open_in_memory')
        source = inv.Source(CONSUMER_FILE, text)
        rows = inv.build([('linux', [diag(
            CONSUMER_FILE, 1, at + 1, 'rusqlite::Connection::open_in_memory',
            tests=False)])], {CONSUMER_FILE: source}, {})
        self.assertEqual(rows[0]['flags'], 'writable',
                         'an in-memory connection can still mutate its database')

    def test_review_compound_macos_cfg_cannot_hide_a_write_door(self):
        file = 'crates/grapt-demo/src/lib.rs'
        sources = {file: inv.Source(file,
            'use rusqlite::Connection;\n'
            '#[cfg(all(target_os = "macos", target_arch = "aarch64"))]\n'
            'fn write(c: &Connection) { c.execute_batch("BEGIN IMMEDIATE"); }\n')}
        inv.mark_test_files(sources)
        found = {(row['file'], row['token'])
                 for row in inv.uncompiled_macos(sources)}
        self.assertIn((file, 'execute_batch'), found,
                      'uncompiled target code must remain visible for review')

class RoundTwoTest(unittest.TestCase):
    MEMBERS = ['crates/grapt-demo', 'crates/grapt-store']

    def artifact(self, member):
        return json.dumps({'reason': 'compiler-artifact', 'manifest_path': '/w/%s/Cargo.toml' % member})

    def test_complete_stream_needs_success_and_every_member(self):
        done = json.dumps({'reason': 'build-finished', 'success': True})
        inv.require_complete('linux', [self.artifact(m) for m in self.MEMBERS] + [done], self.MEMBERS)
        with self.assertRaises(inv.InventoryError):
            inv.require_complete('linux', [self.artifact('crates/grapt-demo'), done], self.MEMBERS)
        with self.assertRaises(inv.InventoryError):
            inv.require_complete('linux', [self.artifact(m) for m in self.MEMBERS], self.MEMBERS)

    def test_workspace_members_are_read_from_the_manifest(self):
        members = inv.workspace_members()
        self.assertIn('crates/grapt-store', members)
        self.assertGreater(len(members), 10)

    def test_unresolvable_doors_fail_closed(self):
        warn = lambda path: json.dumps({'reason': 'compiler-message', 'target': {'name': 'grapt_store'},
                                        'message': {'message': '`%s` does not refer to a reachable function' % path,
                                                    'spans': [], 'code': None}})
        doors = {'grapt_store::A::f': {'owner': 'grapt_store::m::A::f'},
                 'grapt_store::m::A::f': {'owner': 'grapt_store::m::A::f'}}
        one = [('linux', [warn('grapt_store::A::f')])]
        self.assertEqual(inv.unresolved_doors(one, doors), ([], []))
        both = [('linux', [warn('grapt_store::A::f'), warn('grapt_store::m::A::f')])]
        self.assertEqual(inv.unresolved_doors(both, doors), ([], ['grapt_store::m::A::f']))
        fixed = [('linux', [warn('rusqlite::Connection::transaction')])]
        self.assertEqual(inv.unresolved_doors(fixed, {})[0], ['rusqlite::Connection::transaction'])
        other = [('linux', [json.dumps({'reason': 'compiler-message', 'target': {'name': 'grapt_shell'},
                                        'message': {'message': '`rusqlite::X::y` does not refer to a reachable '
                                                    'function', 'spans': [], 'code': None}})])]
        self.assertEqual(inv.unresolved_doors(other, {}), ([], []))

    def test_backstop_is_text_minus_compiler_spans(self):
        lib = ('use rusqlite::Connection;\n'
               '#[cfg(feature = "extra")]\nfn extra(c: &Connection) { c.execute_batch("BEGIN"); }\n'
               'fn mixed(c: &Connection) {\n    c.query_row("x", [], f);\n'
               '    let db = Connection::open("p");\n    let v = Vec::new();\n    v.append(w);\n}\n'
               'fn execute_batch(x: u8) {}\n'
               '#[cfg(test)]\nmod tests { fn t(c: &Connection) { c.execute_batch("y"); } }\n')
        sources = {'crates/a/src/lib.rs': inv.Source('crates/a/src/lib.rs', lib),
                   'crates/a/tests/it.rs': inv.Source('crates/a/tests/it.rs', 'fn i(c: &C) { c.execute_batch("z"); }\n')}
        inv.mark_test_files(sources)
        everything = {(f['line'], f['token']) for f in inv.uncompiled_review(sources)}
        self.assertEqual(everything, {(3, 'execute_batch'), (5, 'query_row'), (6, 'open')})
        query = lib.split('\n')[4].index('query_row') + 1
        spans = {'crates/a/src/lib.rs': [(5, query - 2, 5, query + 20)]}
        self.assertEqual({(f['line'], f['token']) for f in inv.uncompiled_review(sources, spans)},
                         {(3, 'execute_batch'), (6, 'open')})

    def test_compiler_spans_follow_macros_and_ignore_other_lints(self):
        lines = [diag('crates/a/src/lib.rs', 4, 2, 'rusqlite::Connection::execute'),
                 json.dumps({'reason': 'compiler-message', 'target': {'kind': ['lib']},
                             'message': {'code': {'code': 'unused'}, 'message': 'x',
                                         'spans': [{'file_name': 'crates/a/src/lib.rs', 'line_start': 9,
                                                    'column_start': 1, 'line_end': 9, 'column_end': 5,
                                                    'is_primary': True, 'expansion': None}]}})]
        spans = inv.compiler_spans([('linux', lines)])
        self.assertEqual(list(spans), ['crates/a/src/lib.rs'])
        self.assertEqual(spans['crates/a/src/lib.rs'][0][:2], (4, 2))

    def test_trigger_bodies_are_ddl(self):
        sql = 'CREATE TRIGGER t BEFORE UPDATE ON x BEGIN SELECT RAISE(ABORT, 1); END; INSERT INTO y VALUES (1)'
        self.assertEqual(inv.statements_of(sql)[0].split()[0], 'CREATE')
        self.assertEqual(inv.classify_sql(sql), 'ddl')
        self.assertEqual(inv.classify_sql('CREATE TRIGGER t AFTER INSERT ON x BEGIN DELETE FROM y; END'), 'ddl')

    def test_store_owner_calls_carry_the_owner(self):
        file = 'crates/grapt-demo/src/lib.rs'
        text = 'pub fn go(s: &mut EventStore) { s.admit_mailbox_message(m); }\n'
        sources = {file: inv.Source(file, text)}
        doors = {'grapt_store::EventStore::admit_mailbox_message':
                 {'owner': 'grapt_store::write::EventStore::admit_mailbox_message', 'kind': 'operation'}}
        at = text.index('admit_mailbox_message')
        rows = inv.build([('linux', [diag(file, 1, at + 1, 'grapt_store::EventStore::admit_mailbox_message',
                                          tests=False)])], sources, doors)
        site = inv.sites_from(rows)[0]
        self.assertEqual((site['submission'], site['class'], site['via']),
                         ('store-owner-call', 'via-operation', 'grapt_store::write::EventStore::admit_mailbox_message'))


    def test_included_files_take_the_includer_module(self):
        sources = {
            'crates/grapt-store/src/session_list.rs': inv.Source(
                'crates/grapt-store/src/session_list.rs', '// include!("nope.rs");\ninclude!("session_list/budget.rs");\n'),
            'crates/grapt-store/src/session_list/budget.rs': inv.Source(
                'crates/grapt-store/src/session_list/budget.rs', 'pub struct Guard;\n'),
            'crates/grapt-store/src/nope.rs': inv.Source('crates/grapt-store/src/nope.rs', ''),
        }
        inv.mark_test_files(sources)
        inv.mark_includes(sources)
        self.assertEqual(sources['crates/grapt-store/src/session_list/budget.rs'].module, 'grapt_store::session_list')
        self.assertEqual(sources['crates/grapt-store/src/nope.rs'].module, 'grapt_store::nope')


    def test_owners_close_over_public_names_but_not_generic_ones(self):
        file = 'crates/grapt-store/src/w.rs'
        text = ('impl EventStore {\n'
                '    pub fn opener(&mut self) { self.conn.transaction_with_behavior(Immediate); }\n'
                '    pub fn wrapper(&mut self) { self.opener(); }\n'
                '    pub fn outer(&mut self) { self.wrapper(); }\n'
                '    pub fn apply(&mut self) { self.conn.transaction_with_behavior(Immediate); }\n'
                '    pub fn phase(&self, p: P) { p.apply(); }\n'
                '}\n')
        _doors, every = inv.store_owners({file: inv.Source(file, text)})
        self.assertTrue({'grapt_store::w::EventStore::wrapper', 'grapt_store::w::EventStore::outer'} <= every)
        self.assertNotIn('grapt_store::w::EventStore::phase', every)


    def test_trait_impl_owners_get_the_trait_method_door(self):
        files = {
            'crates/grapt-store/src/migration.rs': 'pub trait Migration { fn up(&self, tx: &Tx); }\n',
            'crates/grapt-store/src/migrations.rs': ('impl Migration for Create {\n'
                                                     '    fn up(&self, c: &C) { c.transaction_with_behavior(I); }\n}\n'),
        }
        sources = {rel: inv.Source(rel, text) for rel, text in files.items()}
        inv.mark_includes(sources)
        doors, every = inv.store_owners(sources)
        self.assertIn('grapt_store::migrations::Create::up', every)
        self.assertIn('grapt_store::migration::Migration::up', doors)
        self.assertEqual(doors['grapt_store::migration::Migration::up']['owner'], 'grapt_store::migrations::Create::up')


    def test_compiler_found_owners_are_seeded_and_must_be_configured(self):
        file = 'crates/grapt-store/src/w.rs'
        text = ('impl EventStore {\n'
                '    pub fn opener(&mut self) { self.conn.transaction_with_behavior(Immediate); }\n'
                '    pub fn hidden(&mut self, g: &mut Guard) { g.opener(); }\n'
                '}\n'
                'impl Guard {\n    pub fn opener(&mut self) { self.c.transaction_with_behavior(I); }\n}\n')
        sources = {file: inv.Source(file, text)}
        inv.mark_includes(sources)
        _d, every = inv.store_owners(sources)
        self.assertNotIn('grapt_store::w::EventStore::hidden', every)  # `opener` is not a unique name
        at = text.index('g.opener') + 2
        lines = [diag(file, 3, at - text.rfind('\n', 0, at), 'grapt_store::w::Guard::opener')] * 2
        doors, every, records = inv.owner_fixpoint([('linux', lines)], sources)
        self.assertIn('grapt_store::w::EventStore::hidden', every)
        configured = '{ path = "grapt_store::EventStore::opener", reason = "%s" }' % inv.REASON
        missing = inv.configured_owners(configured, doors)
        self.assertIn('grapt_store::w::EventStore::hidden', missing)
        self.assertNotIn('grapt_store::w::EventStore::opener', missing)


    def test_open_and_migration_callers_are_first_contact_not_legacy(self):
        owner_doors = {
            'grapt_store::EventStore::open': {'owner': 'grapt_store::write::open::EventStore::open'},
            'grapt_store::EventStore::elect_writer': {'owner': 'grapt_store::write::event_log::EventStore::elect_writer'},
            'grapt_store::VmBudgetGuard::stamp_and_prepare': {
                'owner': 'grapt_store::session_list::VmBudgetGuard::stamp_and_prepare'},
            'grapt_store::helper': {'owner': 'grapt_store::m::helper'},
        }
        records = [{'function': 'grapt_store::m::helper', 'scope': 'production', 'door': 'first-contact',
                    'path': 'grapt_store::stamp_and_prepare'}]
        first = inv.first_contact_owners(records, owner_doors)
        self.assertEqual(first, {'grapt_store::write::open::EventStore::open',
                                 'grapt_store::session_list::VmBudgetGuard::stamp_and_prepare',
                                 'grapt_store::m::helper'})
        sites = [{'id': 'grapt_demo::a#1', 'submission': 'store-owner-call',
                  'via': 'grapt_store::write::open::EventStore::open'},
                 {'id': 'grapt_demo::b#1', 'submission': 'store-owner-call',
                  'via': 'grapt_store::write::event_log::EventStore::elect_writer'},
                 {'id': 'grapt_store::m::helper#1', 'submission': 'store-op'}]
        inv.classify_first_contact(sites, first)
        self.assertEqual([s['submission'] for s in sites], ['first-contact', 'store-owner-call', 'first-contact'])


    def test_generic_names_count_only_with_their_own_qualifier(self):
        rel = 'crates/a/src/lib.rs'
        src = inv.Source(rel, 'fn f() { rusqlite::Transaction::finish(x); Connection::finish(y);\n'
                              '    grapt_store::migration::apply(z); v.apply(w); let s = Vec::new(); }\n')
        inv.mark_test_files({rel: src})
        self.assertEqual([r['token'] for r in inv.uncompiled_review({rel: src})], ['finish', 'apply'])


class ReviewRoundTwoCompletenessTest(unittest.TestCase):
    def test_review_public_wrapper_retains_its_store_owner_door(self):
        file = 'crates/grapt-store/src/write.rs'
        text = ('impl EventStore {\n'
                '    pub fn begin_write(&mut self) { self.conn.transaction_with_behavior(Immediate); }\n'
                '    pub fn write_record(&mut self) { self.begin_write(); }\n'
                '}\n')
        doors, _ = inv.store_owners({file: inv.Source(file, text)})
        self.assertIn('grapt_store::EventStore::write_record', doors,
                      'callers of a public transaction wrapper also need a door')

    def test_review_cfg_conditions_are_combined_for_the_same_region(self):
        cases = {
            'stacked': ('#[cfg(unix)]\n#[cfg(not(target_os = "linux"))]\n'
                        'fn write(c: &Connection) { c.execute_batch("BEGIN"); }\n'),
            'enclosing': ('#[cfg(unix)]\nmod platform {\n'
                          '#[cfg(not(target_os = "linux"))]\n'
                          'fn write(c: &Connection) { c.execute_batch("BEGIN"); }\n}\n'),
        }
        file = 'crates/grapt-demo/src/lib.rs'
        for label, text in cases.items():
            with self.subTest(shape=label):
                sources = {file: inv.Source(file, 'use rusqlite::Connection;\n' + text)}
                inv.mark_test_files(sources)
                found = {(row['file'], row['token']) for row in inv.uncompiled_cfg(sources)}
                self.assertIn((file, 'execute_batch'), found,
                              'the combined region is unseen by Linux and Windows')


class ReviewRoundThreeQualifiedDoorTest(unittest.TestCase):
    def qualified_door(self, path):
        self.assertIn(path, inv.door_table({}), 'the operation is an inventory door')
        file = 'crates/grapt-demo/src/lib.rs'
        text = ('fn compiled(mut db: Connection) { let tx = db.transaction(); tx.commit(); }\n'
                '#[cfg(target_os = "macos")]\n'
                'fn platform(value: Any) { ' + path + '(value); }\n')
        sources = {file: inv.Source(file, text)}
        inv.mark_test_files(sources)
        lines = [diag(file, 1, text.index('transaction') + 1,
                      'rusqlite::Connection::transaction', tests=False),
                 diag(file, 1, text.index('commit') + 1,
                      'rusqlite::Transaction::commit', tests=False),
                 json.dumps({'reason': 'compiler-artifact',
                             'manifest_path': '/workspace/crates/grapt-demo/Cargo.toml'}),
                 json.dumps({'reason': 'build-finished', 'success': True})]
        with patch.object(inv, 'workspace_members', return_value=['crates/grapt-demo']), \
                patch.object(inv, 'rust_files', return_value=list(sources)), \
                patch.object(inv, 'load_sources', return_value=sources):
            manifest = inv.inventory([('linux', lines)], inv.clippy_config({}))
        self.assertTrue(manifest['sites'], 'the compiled operation stays visible')
        found = {row['token'] for row in manifest['uncompiled_review']}
        self.assertIn(path.rsplit('::', 1)[1], found,
                      'a qualified generic door unseen by every compiler run needs review')

    def test_review_qualified_transaction_finish_is_not_lost(self):
        self.qualified_door('rusqlite::Transaction::finish')

    def test_review_qualified_connection_backup_is_not_lost(self):
        self.qualified_door('rusqlite::Connection::backup')

    def test_review_qualified_connection_restore_is_not_lost(self):
        self.qualified_door('rusqlite::Connection::restore')

    def test_review_qualified_migration_apply_is_not_lost(self):
        self.qualified_door('grapt_store::migration::apply')


if __name__ == '__main__':
    unittest.main()
