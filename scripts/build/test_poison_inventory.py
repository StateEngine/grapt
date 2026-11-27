import os
import shutil
import tempfile
import unittest
from pathlib import Path

import poison_inventory

ALPHA_LIB = '''\
mod worker;

#[cfg(test)]
mod tests;
'''

WORKER = '''\
use std::sync::Mutex;

pub struct Worker {
    inbox: Mutex<Vec<String>>,
}

impl Worker {
    pub fn drain(&self) -> usize {
        let held = self.inbox.lock().unwrap_or_else(|error| error.into_inner());
        let count = held.len();
        drop(held);
        count
    }

    pub fn push(&self, item: String) {
        let mut held = self.inbox.lock().unwrap_or_else(|error| error.into_inner());
        held.push(item);
    }

    pub fn peek(&self) -> Option<String> {
        match self.inbox.try_lock() {
            Ok(held) => held.first().cloned(),
            Err(std::sync::TryLockError::Poisoned(error)) => {
                let mut held = error.into_inner();
                held.push("late".to_string());
                Some(held.pop()?)
            }
            Err(_) => None,
        }
    }
}

pub struct SessionId(u64);

impl SessionId {
    pub fn into_inner(self) -> u64 {
        self.0
    }
}

pub fn id_of(session_id: SessionId) -> u64 {
    SessionId::into_inner(session_id)
}

pub fn recover(error: std::sync::PoisonError<Vec<u8>>) -> Vec<u8> {
    std::sync::PoisonError::into_inner(error)
}

pub fn notes() -> String {
    // error.into_inner() and PoisonError::into_inner only appear here
    "error.into_inner() in a string literal"
}
'''

ALPHA_TESTS = '''\
use std::sync::Mutex;

pub struct Probe {
    inbox: Mutex<u32>,
}

impl Probe {
    pub fn peek(&self) -> u32 {
        let held = self.inbox.lock().unwrap_or_else(|error| error.into_inner());
        *held
    }
}
'''

INTEGRATION = '''\
use std::sync::Mutex;

static COUNTER: Mutex<u32> = Mutex::new(0);

fn count() -> u32 {
    let held = COUNTER.lock().unwrap_or_else(|error| error.into_inner());
    *held
}
'''

BETA_LIB = '''\
pub fn one() -> u32 {
    1
}
'''

BETA_TESTS = '''\
use std::sync::Mutex;

pub struct Cache {
    entries: Mutex<Vec<u8>>,
}

impl Cache {
    pub fn take(&self) -> Option<u8> {
        let mut held = self.entries.lock().unwrap_or_else(|error| error.into_inner());
        held.pop()
    }
}
'''

EXTRA_SITE = '''
pub struct Tail {
    inbox: Mutex<u8>,
}

impl Tail {
    pub fn last(&self) -> u8 {
        let held = self.inbox.lock().unwrap_or_else(|error| error.into_inner());
        *held
    }
}
'''


def write_tree(root):
    files = {
        'crates/alpha/src/lib.rs': ALPHA_LIB,
        'crates/alpha/src/worker.rs': WORKER,
        'crates/alpha/src/tests.rs': ALPHA_TESTS,
        'crates/alpha/tests/it.rs': INTEGRATION,
        'crates/beta/src/lib.rs': BETA_LIB,
        'crates/beta/src/tests.rs': BETA_TESTS,
    }
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding='utf-8')


def find_sites(sites, path, fn):
    return [s for s in sites if s['file'] == path and s['function'] == fn]


class PoisonInventoryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        write_tree(self.root)
        self.sites = poison_inventory.build(self.root)

    def tearDown(self):
        self.temporary.cleanup()

    def source_sites(self, source):
        relative = 'crates/sample/src/lib.rs'
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text('use std::sync::Mutex;\n' + source, encoding='utf-8')
        return [site for site in poison_inventory.build(self.root)
                if site['file'] == relative]

    def test_aliased_lock_results_are_not_silently_omitted(self):
        for recovery in (
            'let held = result.unwrap_or_else(|error| error.into_inner()); *held',
            'match result { Ok(held) => *held, Err(error) => *error.into_inner() }',
        ):
            with self.subTest(recovery=recovery):
                sites = self.source_sites('''
pub fn recover(lock: &Mutex<u32>) -> u32 {
    let result = lock.lock();
    ''' + recovery + '\n}\n')
                self.assertEqual(len(sites), 1, 'a local alias cannot hide the recovery')

    def test_a_poison_type_alias_inside_a_macro_is_inventoried(self):
        sites = self.source_sites('''
use std::sync::PoisonError as Fault;
macro_rules! recover {
    ($lock:expr) => { $lock.lock().unwrap_or_else(Fault::into_inner) };
}
pub fn read(lock: &Mutex<u32>) -> u32 { *recover!(lock) }
''')
        self.assertEqual(len(sites), 1, 'a type alias is still a poison recovery')

    def test_a_multiline_recovery_closure_is_inventoried(self):
        sites = self.source_sites('''
pub fn recover(lock: &Mutex<u32>) -> u32 {
    let held = lock.lock().unwrap_or_else(|error| {
        error.into_inner()
    });
    *held
}
''')
        self.assertEqual(len(sites), 1, 'the closure body need not share its parameter line')

    def test_check_rejects_a_new_site_through_an_aliased_lock_result(self):
        baseline = self.root / 'baseline.json'
        baseline.write_text(poison_inventory.render(self.sites), encoding='utf-8')
        self.source_sites('''
pub fn recover(lock: &Mutex<u32>) -> u32 {
    let result = lock.lock();
    let held = result.unwrap_or_else(|error| error.into_inner());
    *held
}
''')
        self.assertEqual(poison_inventory.check(self.root, baseline=baseline), 1,
                         'a new recovery must stale the checked-in inventory')

    def test_compound_guard_assignments_are_state_bearing(self):
        for value_type, mutation in (('u32', '*held += 1;'),
                                     ('Vec<u32>', 'held[0] += 1;')):
            with self.subTest(mutation=mutation):
                sites = self.source_sites('''
pub fn update(lock: &Mutex<''' + value_type + '''>) {
    let mut held = lock.lock().unwrap_or_else(|poison| poison.into_inner());
    ''' + mutation + '\n}\n')
                self.assertEqual(len(sites), 1)
                self.assertTrue(sites[0]['state_bearing'],
                                'compound assignment mutates the recovered guard')

    def test_a_guard_result_parameter_cannot_be_proven_not_poison(self):
        sites = self.source_sites('''

type Held<'a> = std::sync::LockResult<std::sync::MutexGuard<'a, u32>>;
pub fn recover(result: Held<'_>) -> u32 {
    let mut held = result.unwrap_or_else(|error| error.into_inner());
    *held += 1;
    *held
}
''')
        self.assertEqual(len(sites), 1)
        self.assertNotEqual(sites[0]['kind'], 'not-poison', sites[0])
        self.assertTrue(sites[0]['state_bearing'])

    def test_a_shadowed_std_type_name_is_not_a_non_poison_proof(self):
        sites = self.source_sites('''

type Rc<T> = std::sync::PoisonError<T>;
pub fn recover(error: Rc<u32>) -> u32 {
    Rc::into_inner(error)
}
''')
        self.assertEqual(len(sites), 1)
        self.assertNotEqual(sites[0]['kind'], 'not-poison', sites[0])
        self.assertTrue(sites[0]['needs_review'])

    def test_a_use_as_rename_of_a_lock_result_cannot_be_proven_not_poison(self):
        sites = self.source_sites('''

use std::sync::LockResult as Held;
pub fn recover(result: Held<u32>) -> u32 {
    let mut held = result.unwrap_or_else(|error| error.into_inner());
    *held += 1;
    *held
}
''')
        self.assertEqual(len(sites), 1)
        self.assertNotEqual(sites[0]['kind'], 'not-poison', sites[0])
        self.assertTrue(sites[0]['needs_review'])

    def test_a_generic_parameter_cannot_be_proven_not_poison(self):
        sites = self.source_sites('''

trait IntoParts {
    type Inner;
    fn into_inner(self) -> Self::Inner;
}
pub fn unpack<T: IntoParts>(value: T) -> T::Inner {
    value.into_inner()
}
''')
        self.assertEqual(len(sites), 1)
        self.assertNotEqual(sites[0]['kind'], 'not-poison', sites[0])
        self.assertTrue(sites[0]['needs_review'])

    def test_a_poison_field_is_not_a_lock_free_function_proof(self):
        sites = self.source_sites("""
pub struct Held<'a> {
    fault: std::sync::PoisonError<std::sync::MutexGuard<'a, u32>>,
}
pub fn recover(value: Held<'_>) -> u32 {
    let mut guard = value.fault.into_inner();
    *guard += 1;
    *guard
}
""")
        recovery = [site for site in sites if site['function'] == 'recover']
        self.assertEqual(len(recovery), 1)
        self.assertNotEqual(recovery[0]['kind'], 'not-poison', recovery[0])
        self.assertTrue(recovery[0]['needs_review'], recovery[0])

    def test_an_inferred_method_result_is_not_a_lock_free_function_proof(self):
        sites = self.source_sites("""
pub struct Held<'a> {
    fault: std::sync::PoisonError<std::sync::MutexGuard<'a, u32>>,
}
impl<'a> Held<'a> {
    pub fn take(self) -> std::sync::PoisonError<std::sync::MutexGuard<'a, u32>> {
        self.fault
    }
}
pub fn recover(value: Held<'_>) -> u32 {
    let mut guard = value.take().into_inner();
    *guard += 1;
    *guard
}
""")
        recovery = [site for site in sites if site['function'] == 'recover']
        self.assertEqual(len(recovery), 1)
        self.assertNotEqual(recovery[0]['kind'], 'not-poison', recovery[0])
        self.assertTrue(recovery[0]['needs_review'], recovery[0])

    def test_recovery_sites_are_inventoried(self):
        self.assertEqual(len(self.sites), 8)
        drain = find_sites(self.sites, 'crates/alpha/src/worker.rs', 'drain')[0]
        self.assertEqual((drain['guard'], drain['kind']), ('self.inbox', 'std-lock'))
        self.assertEqual(drain['guard_type'], 'Vec<String>')
        self.assertEqual((drain['test'], drain['state_bearing']), (False, False))
        self.assertTrue(drain['needs_review'])
        self.assertEqual(drain['poison_recovery'], 'certain')
        self.assertEqual((drain['scope'], drain['action']),
                         ('single-scope', 'needs-review'))

    def test_mutation_within_the_block_marks_state_bearing(self):
        push = find_sites(self.sites, 'crates/alpha/src/worker.rs', 'push')[0]
        self.assertTrue(push['state_bearing'])
        self.assertTrue(push['needs_review'])
        self.assertEqual(push['poison_recovery'], 'certain')
        peek = find_sites(self.sites, 'crates/alpha/src/worker.rs', 'peek')[0]
        self.assertEqual((peek['guard'], peek['kind']), ('self.inbox', 'std-lock'))
        self.assertTrue(peek['state_bearing'])

    def test_newtype_recovery_stays_needs_review_with_an_advisory(self):
        worker = 'crates/alpha/src/worker.rs'
        newtype = find_sites(self.sites, worker, 'id_of')
        self.assertEqual(len(newtype), 1)
        self.assertEqual(newtype[0]['kind'], 'unresolved')
        self.assertIsNone(newtype[0]['guard'])
        self.assertTrue(newtype[0]['state_bearing'])
        self.assertEqual(newtype[0]['poison_recovery'], 'unknown')
        self.assertTrue(newtype[0]['needs_review'])
        self.assertEqual(newtype[0]['suggested_exempt'],
                         'SessionId (into_inner defined for '
                         'SessionId in this repo)')
        self.assertTrue(newtype[0]['snippet'])
        self.assertEqual(newtype[0]['action'], 'needs-review')
        self.assertEqual(find_sites(self.sites, worker, 'notes'), [])

    def test_path_form_poison_recovery_is_a_site(self):
        recover = find_sites(self.sites, 'crates/alpha/src/worker.rs', 'recover')[0]
        self.assertIsNone(recover['guard'])
        self.assertEqual(recover['kind'], 'unresolved')
        self.assertTrue(recover['needs_review'])
        self.assertEqual(recover['poison_recovery'], 'certain')

    def test_cfg_test_modules_are_test_code(self):
        probe = find_sites(self.sites, 'crates/alpha/src/tests.rs', 'peek')[0]
        self.assertEqual((probe['test'], probe['scope'], probe['action']),
                         (True, 'test', 'needs-review'))
        counter = find_sites(self.sites, 'crates/alpha/tests/it.rs', 'count')[0]
        self.assertEqual(counter['guard'], 'COUNTER')
        self.assertEqual((counter['test'], counter['scope']), (True, 'test'))

    def test_stem_collision_keeps_the_production_file(self):
        take = find_sites(self.sites, 'crates/beta/src/tests.rs', 'take')[0]
        self.assertFalse(take['test'])
        self.assertEqual(take['scope'], 'single-scope')
        self.assertTrue(take['state_bearing'])
        self.assertTrue(take['needs_review'])
        self.assertEqual(take['poison_recovery'], 'certain')

    def collision_site(self, qualified):
        folder = self.root / 'crates/collision/src'
        folder.mkdir(parents=True)
        (folder / 'plain.rs').write_text('''pub struct Value(u32);
impl Value {
    pub fn into_inner(self) -> u32 { self.0 }
}
''', encoding='utf-8')
        (folder / 'fault.rs').write_text(
            'pub use std::sync::PoisonError as Value;\n', encoding='utf-8')
        binding = '' if qualified else 'use crate::fault::Value;\n'
        receiver = 'crate::fault::Value' if qualified else 'Value'
        source = ('mod plain;\nmod fault;\n' + binding +
                  "pub fn recover(value: std::sync::PoisonError<"
                  "std::sync::MutexGuard<'_, u32>>) -> u32 {\n"
                  '    let mut held = ' + receiver + '::into_inner(value);\n'
                  '    *held += 1;\n    *held\n}\n')
        (folder / 'lib.rs').write_text(source, encoding='utf-8')
        sites = find_sites(poison_inventory.build(self.root),
                           'crates/collision/src/lib.rs', 'recover')
        self.assertEqual(len(sites), 1, sites)
        return sites[0]

    def test_a_qualified_poison_alias_cannot_borrow_an_unrelated_type_proof(self):
        site = self.collision_site(qualified=True)
        self.assertTrue(site['needs_review'], site)
        self.assertNotEqual(site['action'], 'allowlist', site)

    def test_an_imported_poison_alias_cannot_borrow_an_unrelated_type_proof(self):
        site = self.collision_site(qualified=False)
        self.assertTrue(site['needs_review'], site)
        self.assertNotEqual(site['action'], 'allowlist', site)

    def test_a_read_only_label_cannot_exempt_a_recovery(self):
        sites = [site for site in self.sites
                 if site['state_bearing'] is False and site['kind'] != 'not-poison']
        self.assertTrue(sites, 'the read-only fixture must exercise this path')
        for site in sites:
            self.assertTrue(site['needs_review'], site)
            self.assertNotEqual(site['action'], 'allowlist', site)

    def test_no_site_can_escape_needs_review(self):
        # The output: every site of a full build is needs_review, and no
        # site carries an allowlist action.
        for site in self.sites:
            self.assertTrue(site['needs_review'], site)
            self.assertNotEqual(site['action'], 'allowlist', site)
        # The classifier paths, one per shape: the two shapes that were
        # once exempt by positive proof now stay needs_review, with only
        # an advisory suggestion.
        for name, source, kind, recovery, suggestion in (
            ('closure',
             '''
pub fn recover(lock: &Mutex<u32>) -> u32 {
    let held = lock.lock().unwrap_or_else(|error| error.into_inner());
    *held
}
''', 'std-lock', 'certain', None),
            ('arm',
             '''
pub fn recover(lock: &Mutex<u32>) -> u32 {
    match lock.try_lock() {
        Ok(held) => *held,
        Err(std::sync::TryLockError::Poisoned(error)) => {
            let held = error.into_inner();
            *held
        }
        Err(_) => 0,
    }
}
''', 'std-lock', 'certain', None),
            ('path-poison',
             '''
pub fn recover(error: std::sync::PoisonError<Vec<u8>>) -> Vec<u8> {
    std::sync::PoisonError::into_inner(error)
}
''', 'unresolved', 'certain', None),
            ('bare-unknown',
             '''
pub struct Boxed(u32);
pub fn open(value: Boxed) -> u32 {
    value.into_inner()
}
''', 'unresolved', 'unknown', None),
            ('bare-certain',
             '''
pub fn recover(lock: &Mutex<u32>) -> u32 {
    let result = lock.lock(); result.unwrap_or_else(|error| error.into_inner())
}
''', 'std-lock', 'certain', None),
            ('repo-newtype',
             '''
pub struct Id(u64);
impl Id {
    pub fn into_inner(self) -> u64 {
        self.0
    }
}
pub fn open(value: Id) -> u64 {
    Id::into_inner(value)
}
''', 'unresolved', 'unknown',
             'Id (into_inner defined for Id in this repo)'),
            ('std-path',
             '''
pub fn open(slot: std::mem::ManuallyDrop<u32>) -> u32 {
    std::mem::ManuallyDrop::into_inner(slot)
}
''', 'unresolved', 'unknown',
             'ManuallyDrop (std path written at the call site)'),
        ):
            with self.subTest(shape=name):
                sites = self.source_sites(source)
                self.assertEqual(len(sites), 1, source)
                site = sites[0]
                self.assertEqual((site['kind'], site['needs_review'],
                                  site['poison_recovery']),
                                 (kind, True, recovery), site)
                self.assertEqual(site['suggested_exempt'], suggestion, site)
                self.assertNotEqual(site['action'], 'allowlist', site)

    def test_crlf_line_endings_inventory_identically(self):
        lf = ('use std::sync::Mutex;\n'
              '\n'
              'pub struct Crlf {\n'
              '    inbox: Mutex<u32>,\n'
              '}\n'
              '\n'
              'impl Crlf {\n'
              '    pub fn drain(&self) -> u32 {\n'
              '        let held = self.inbox.lock()'
              '.unwrap_or_else(|error| error.into_inner());\n'
              '        *held\n'
              '    }\n'
              '}\n')
        self.root.joinpath('crates/alpha/src/crlf_lf.rs').write_text(
            lf, encoding='utf-8')
        self.root.joinpath('crates/alpha/src/crlf_crlf.rs').write_text(
            lf.replace('\n', '\r\n'), encoding='utf-8')
        sites = {'crates/alpha/src/crlf_lf.rs': [],
                 'crates/alpha/src/crlf_crlf.rs': []}
        for site in poison_inventory.build(self.root):
            if site['file'] in sites:
                sites[site['file']].append(
                    {k: v for k, v in site.items() if k != 'file'})
        self.assertEqual(len(sites['crates/alpha/src/crlf_lf.rs']), 1)
        self.assertEqual(sites['crates/alpha/src/crlf_lf.rs'],
                         sites['crates/alpha/src/crlf_crlf.rs'])

    def test_discover_fails_loudly_without_a_rust_tree(self):
        empty = self.root / 'no_rust'
        empty.mkdir()
        with self.assertRaises(SystemExit) as caught:
            poison_inventory.discover(str(empty))
        self.assertIn('no crates/**/*.rs found', str(caught.exception))

    def test_review_discover_does_not_follow_file_symlinks(self):
        outside = self.root / 'outside.rs'
        outside.write_text(WORKER, encoding='utf-8')
        linked = self.root / 'crates/alpha/src/escaped.rs'
        linked.symlink_to(outside)
        self.assertNotIn('crates/alpha/src/escaped.rs',
                         poison_inventory.discover(self.root))
        self.assertEqual(poison_inventory.build(self.root), self.sites)

    def test_discover_needs_no_rg_on_path(self):
        if shutil.which('rg') is None:
            self.skipTest('rg is already absent from PATH')
        expected = sorted(poison_inventory.discover(str(self.root)))
        self.assertTrue(expected)
        bare = self.root / 'no_rg_path'
        bare.mkdir()
        original = os.environ.get('PATH')
        os.environ['PATH'] = str(bare)
        try:
            self.assertEqual(poison_inventory.discover(str(self.root)), expected)
            self.assertEqual(poison_inventory.build(self.root), self.sites)
        finally:
            if original is None:
                del os.environ['PATH']
            else:
                os.environ['PATH'] = original

    def test_check_accepts_fresh_baseline_and_flags_drift(self):
        baseline = self.root / 'baseline.json'
        baseline.write_text(poison_inventory.render(self.sites), encoding='utf-8')
        self.assertEqual(poison_inventory.check(self.root, baseline=baseline), 0)
        worker = self.root / 'crates/alpha/src/worker.rs'
        worker.write_text(worker.read_text(encoding='utf-8') + EXTRA_SITE,
                          encoding='utf-8')
        self.assertEqual(poison_inventory.check(self.root, baseline=baseline), 1)


if __name__ == '__main__':
    unittest.main()
