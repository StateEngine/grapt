import json
from pathlib import Path
import subprocess
import tempfile
import unittest

import ratchet_monotonic as mono


class RatchetMonotonicTests(unittest.TestCase):
    def test_include_ratchet_refuses_new_carrier_or_fragment_and_allows_shrink(self):
        base = {'carriers': {'crates/a/src/lib.rs': ['crates/a/src/p1.rs', 'crates/a/src/p2.rs']}}
        shrank = {'carriers': {'crates/a/src/lib.rs': ['crates/a/src/p1.rs']}}
        self.assertEqual(mono.check_include_ratchet(base, shrank), [])

        added_carrier = {'carriers': {
            'crates/a/src/lib.rs': ['crates/a/src/p1.rs'],
            'crates/b/src/lib.rs': ['crates/b/src/x.rs'],
        }}
        self.assertTrue(any('new carrier' in p for p in mono.check_include_ratchet(base, added_carrier)))

        added_frag = {'carriers': {
            'crates/a/src/lib.rs': ['crates/a/src/p1.rs', 'crates/a/src/p3.rs'],
        }}
        self.assertTrue(any('new fragment' in p for p in mono.check_include_ratchet(base, added_frag)))

    def test_legacy_ratchet_refuses_new_file_growth_or_lint_swap(self):
        base = {'files': {'crates/a/src/lib.rs': {'clippy::too_many_lines': 2}}}
        shrank = {'files': {'crates/a/src/lib.rs': {'clippy::too_many_lines': 1}}}
        self.assertEqual(mono.check_legacy_ratchet(base, shrank), [])

        swapped = {'files': {'crates/a/src/lib.rs': {
            'clippy::too_many_lines': 1,
            'clippy::too_many_arguments': 1,
        }}}
        self.assertTrue(any('new lint' in p for p in mono.check_legacy_ratchet(base, swapped)))

        grew = {'files': {'crates/a/src/lib.rs': {'clippy::too_many_lines': 3}}}
        self.assertTrue(any('grew from 2 to 3' in p for p in mono.check_legacy_ratchet(base, grew)))

        added_file = {'files': {
            'crates/a/src/lib.rs': {'clippy::too_many_lines': 2},
            'crates/b/src/lib.rs': {'clippy::too_many_lines': 1},
        }}
        self.assertTrue(any('new file' in p for p in mono.check_legacy_ratchet(base, added_file)))

    def test_cycles_ratchet_refuses_new_cycle_or_new_edge_within_existing_scc(self):
        base = {
            'cycles': {'t': [['c::a', 'c::b', 'c::d']]},
            'edges': {'t': [['c::a', 'c::b'], ['c::b', 'c::d'], ['c::d', 'c::a']]},
        }
        shrank = {
            'cycles': {'t': [['c::a', 'c::b']]},
            'edges': {'t': [['c::a', 'c::b']]},
        }
        self.assertEqual(mono.check_cycles_ratchet(base, shrank), [])

        new_edge_in_scc = {
            'cycles': {'t': [['c::a', 'c::b', 'c::d']]},
            'edges': {'t': [['c::a', 'c::b'], ['c::b', 'c::d'], ['c::d', 'c::a'], ['c::a', 'c::d']]},
        }
        problems = mono.check_cycles_ratchet(base, new_edge_in_scc)
        self.assertTrue(any('new cyclic edge c::a -> c::d' in p for p in problems))

        new_target = {
            'cycles': {'t': [['c::a', 'c::b', 'c::d']], 'u': [['u::x', 'u::y']]},
            'edges': {'t': base['edges']['t'], 'u': [['u::x', 'u::y'], ['u::y', 'u::x']]},
        }
        self.assertTrue(any('new or larger cycle' in p for p in mono.check_cycles_ratchet(base, new_target)))

    def test_end_to_end_git_check_catches_candidate_ledger_inflation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            subprocess.run(['git', 'init', '-q'], cwd=root, check=True)
            subprocess.run(['git', 'config', 'user.name', 'Test'], cwd=root, check=True)
            subprocess.run(['git', 'config', 'user.email', 'test@example.invalid'], cwd=root, check=True)
            for rel, doc in (
                (mono.INCLUDE_RATCHET, {'carriers': {'crates/a/src/lib.rs': ['crates/a/src/p.rs']}}),
                (mono.LEGACY_RATCHET, {'files': {'crates/a/src/lib.rs': {'clippy::too_many_lines': 1}}}),
                (mono.CYCLES_RATCHET, {'cycles': {}, 'edges': {}}),
            ):
                p = root / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(json.dumps(doc) + '\n', encoding='utf-8')
            subprocess.run(['git', 'add', '.'], cwd=root, check=True)
            subprocess.run(['git', 'commit', '-q', '-m', 'base'], cwd=root, check=True)
            base = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()

            self.assertEqual(mono.check(root, base, live_configs=set()), [])
            # Inflate candidate legacy ratchet
            (root / mono.LEGACY_RATCHET).write_text(
                json.dumps({'files': {'crates/a/src/lib.rs': {'clippy::too_many_arguments': 1}}}) + '\n',
                encoding='utf-8',
            )
            problems = mono.check(root, base, live_configs=set())
            self.assertTrue(any('new lint clippy::too_many_arguments' in p for p in problems))

            # Unknown/invalid base commit must fail closed (never silently pass).
            bad_base = mono.check(root, '0000000000000000000000000000000000000000', live_configs=set())
            self.assertTrue(any('unknown or invalid base commit' in p for p in bad_base))

    def test_cycles_ratchet_locks_deep_prefixes_and_live_workspace_configs(self):
        base = {
            'deep': ['grapt_product::dogfood'],
            'configs': [
                'grapt-api',
                'grapt-product',
                'grapt-product[all-features]',
                'grapt-product[feature:legacy-ui]',
            ],
            'cycles': {'grapt-api': [['a::x', 'a::y']], 'grapt-product': [['c::a', 'c::b']]},
            'edges': {
                'grapt-api': [['a::x', 'a::y'], ['a::y', 'a::x']],
                'grapt-product': [['c::a', 'c::b'], ['c::b', 'c::a']],
            },
        }
        live = {
            'grapt-api',
            'grapt-product',
            'grapt-product[all-features]',
            'grapt-product[feature:legacy-ui]',
        }
        broadened_deep = dict(base, deep=['grapt_product'])
        self.assertTrue(any('deep prefixes' in p for p in mono.check_cycles_ratchet(base, broadened_deep, live)))

        dropped_config = dict(base, configs=['grapt-api', 'grapt-product', 'grapt-product[feature:legacy-ui]'])
        self.assertTrue(any('checked config grapt-product[all-features] was dropped' in p
                            for p in mono.check_cycles_ratchet(base, dropped_config, live)))

        # Deleting ALL entries for a still-live target (grapt-api) must fail closed.
        deleted_live_target = {
            'deep': ['grapt_product::dogfood'],
            'configs': ['grapt-product', 'grapt-product[all-features]', 'grapt-product[feature:legacy-ui]'],
            'cycles': {'grapt-product': [['c::a', 'c::b']]},
            'edges': {'grapt-product': [['c::a', 'c::b'], ['c::b', 'c::a']]},
        }
        self.assertTrue(any('checked config grapt-api was dropped' in p
                            for p in mono.check_cycles_ratchet(base, deleted_live_target, live)))

        # Deleting entries for a genuinely removed workspace target is allowed.
        self.assertEqual(
            mono.check_cycles_ratchet(
                base,
                deleted_live_target,
                {'grapt-product', 'grapt-product[all-features]', 'grapt-product[feature:legacy-ui]'},
            ),
            [],
        )

        # Deleting a removed Cargo feature's config while the library target still exists is allowed.
        deleted_feature = dict(base, configs=['grapt-api', 'grapt-product', 'grapt-product[all-features]'])
        live_without_feature = {'grapt-api', 'grapt-product', 'grapt-product[all-features]'}
        self.assertEqual(mono.check_cycles_ratchet(base, deleted_feature, live_without_feature), [])
        # Dropping that feature config while it is still live must still fail closed.
        self.assertTrue(any('checked config grapt-product[feature:legacy-ui] was dropped' in p
                            for p in mono.check_cycles_ratchet(base, deleted_feature, live)))

    def test_cycles_ratchet_new_feature_configs_inherit_base_target_only_when_new(self):
        c3, e3 = [['c::a', 'c::b', 'c::d']], [['c::a', 'c::b'], ['c::b', 'c::d'], ['c::d', 'c::a']]
        c2, e2 = [['c::a', 'c::b']], [['c::a', 'c::b'], ['c::b', 'c::a']]
        base = {'deep': ['grapt_product::dogfood'],
                'configs': ['grapt-product', 'grapt-product[feature:existing]', 'grapt-cli:grapt', 'grapt-api[all-features]'],
                'cycles': {'grapt-product': c3, 'grapt-product[feature:existing]': c2, 'grapt-cli:grapt': c2, 'grapt-api[all-features]': c3},
                'edges': {'grapt-product': e3, 'grapt-product[feature:existing]': e2, 'grapt-cli:grapt': e2, 'grapt-api[all-features]': e3}}
        cases = [
            ('new_feature_and_bin', {'grapt-product[all-features]': (c3, e3), 'grapt-product[feature:frozen]': (c2, [['c::a', 'c::b']]), 'grapt-cli:grapt[feature:extra]': (c2, e2)}, None, True, None),
            ('extra_edge', {'grapt-product[feature:frozen]': (c3, e3 + [['c::a', 'c::d']])}, None, True, 'new cyclic edge c::a -> c::d'),
            ('larger_cycle', {'grapt-product[all-features]': ([['c::a', 'c::b', 'c::d', 'c::e']], e3)}, None, True, 'new or larger cycle'),
            ('existing_regrowth', {'grapt-product[feature:existing]': (c3, e3)}, None, True, 'grapt-product[feature:existing] new cyclic edge c::b -> c::d'),
            ('brand_new_crate', {'grapt-new': (c2, e2), 'grapt-new[all-features]': (c2, e2)}, None, True, 'grapt-new[all-features] new or larger cycle'),
            ('nested_suffix', {'grapt-api[all-features][feature:unused]': (c3, e3)}, None, True, 'grapt-api[all-features][feature:unused] new or larger cycle'),
            ('trailing_newline', {'grapt-product[all-features]\n': (c3, e3)}, None, True, 'new or larger cycle'),
            ('non_live_feature', {'grapt-product[feature:ghost]': (c3, e3)}, set(), True, 'grapt-product[feature:ghost] new or larger cycle'),
            ('missing_base_configs', {'grapt-product[all-features]': (c3, e3)}, None, False, 'grapt-product[all-features] new or larger cycle'),
        ]
        for name, adds, live_override, keep_configs, expected in cases:
            with self.subTest(case=name):
                b = base if keep_configs else {k: v for k, v in base.items() if k != 'configs'}
                head = {'deep': base['deep'], 'configs': sorted(set(base['configs']) | set(adds)),
                        'cycles': {**base['cycles'], **{k: v[0] for k, v in adds.items()}},
                        'edges': {**base['edges'], **{k: v[1] for k, v in adds.items()}}}
                problems = mono.check_cycles_ratchet(b, head, set(base['configs']) | (set(adds) if live_override is None else live_override))
                self.assertEqual(problems, []) if expected is None else self.assertTrue(any(expected in p for p in problems), problems)

    def test_git_show_json_fails_closed_on_git_errors(self):
        from unittest.mock import patch

        class Failed:
            returncode = 128
            stdout = ''
            stderr = 'fatal: unable to read tree'

        with patch.object(mono.subprocess, 'run', return_value=Failed()):
            doc, err = mono.git_show_json(Path('.'), 'HEAD', mono.CYCLES_RATCHET)
        self.assertIsNone(doc)
        self.assertIsNotNone(err)
        self.assertIn('failed to inspect base commit', err)


if __name__ == '__main__':
    unittest.main()
