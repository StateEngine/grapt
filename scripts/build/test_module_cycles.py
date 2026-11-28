import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import module_cycles


def graph(edges, modules=('c::a', 'c::b', 'c::d', 'c::d::inner')):
    lines = ['    "c" [label="crate|c"];']
    lines += [f'    "{m}" [label="pub mod|{m}"];' for m in modules]
    lines += [f'    "{a}" -> "{b}" [label="uses", color="#7f7f7f"];' for a, b in edges]
    return '\n'.join(lines)


class ModuleCycleTests(unittest.TestCase):
    def test_root_owned_type_does_not_hide_a_sibling_cycle(self):
        text = graph([('c::Bridge', 'c::b::Item'), ('c::a::f', 'c::Bridge'),
                      ('c::b::f', 'c::a::Item')])
        self.assertTrue(module_cycles.cycles(text))

    def test_record_refuses_a_new_cycle_without_changing_baseline(self):
        text = graph([('c::a::Item', 'c::b::Item'), ('c::b::f', 'c::a::Item')])
        groups, edges = module_cycles.cycles_and_edges(text)
        with tempfile.TemporaryDirectory() as temporary:
            baseline = Path(temporary) / 'baseline.json'
            baseline.write_text(json.dumps({'cycles': {}, 'edges': {}}), encoding='utf-8')
            before = baseline.read_bytes()
            with patch.object(module_cycles, 'BASELINE', baseline), \
                    patch.object(module_cycles.sys, 'argv', ['module_cycles.py', 'record']), \
                    patch.object(module_cycles, 'tool', return_value='cargo-modules'), \
                    patch.object(module_cycles, 'measure',
                                 return_value=({'sample': groups}, {'sample': edges}, ['sample'])):
                self.assertNotEqual(module_cycles.main(), 0)
            self.assertEqual(baseline.read_bytes(), before)

    def test_items_collapse_to_modules_and_root_reexports_are_ignored(self):
        text = graph([('c::a::Item', 'c::b::f'), ('c::b::Other', 'c::a'), ('c', 'c::d::inner::X'),
                      ('c::d::inner::Y', 'c')])
        self.assertEqual(module_cycles.cycles(text), [['c::a', 'c::b']])

    def test_deep_prefixes_split_a_module_into_its_children(self):
        text = graph([('c::d::inner::X', 'c::d::Y'), ('c::d::Y', 'c::d::inner')])
        self.assertEqual(module_cycles.cycles(text), [])
        self.assertEqual(module_cycles.cycles(text, ['c::d']), [['c::d', 'c::d::inner']])

    def test_only_shrinking_is_allowed_and_must_be_recorded(self):
        old = [['c::a', 'c::b', 'c::d']]
        old_edges = [['c::a', 'c::b'], ['c::b', 'c::d'], ['c::d', 'c::a']]
        self.assertEqual(module_cycles.differences('t', old, old, old_edges, old_edges), [])
        grown = module_cycles.differences('t', [['c::a', 'c::b', 'c::d', 'c::e']], old)
        self.assertEqual(len(grown), 1)
        self.assertIn('new or larger cycle', grown[0])
        self.assertIn('shrank or split', module_cycles.differences('t', [['c::a', 'c::b']], old)[0])
        self.assertIn('new or larger cycle', module_cycles.differences('t', [['c::e', 'c::f']], [])[0])
        # Adding a new direct cyclic edge between modules already in the same SCC is rejected.
        new_edge = module_cycles.differences('t', old, old, old_edges + [['c::a', 'c::d']], old_edges)
        self.assertEqual(len(new_edge), 1)
        self.assertIn('new cyclic edge c::a -> c::d', new_edge[0])

    def test_targets_discover_lib_bin_and_independent_feature_configurations(self):
        metadata = json.dumps({
            'packages': [
                {
                    'name': 'demo-bin',
                    'features': {'extra': []},
                    'targets': [{'name': 'demo-bin', 'kind': ['bin']}],
                },
                {
                    'name': 'demo-multi',
                    'features': {'default': [], 'alpha': [], 'beta': []},
                    'targets': [{'name': 'demo_multi', 'kind': ['lib']}],
                },
                {
                    'name': 'demo-both',
                    'features': {},
                    'targets': [
                        {'name': 'demo_both', 'kind': ['lib']},
                        {'name': 'zeta', 'kind': ['bin']},
                        {'name': 'alpha', 'kind': ['bin']},
                    ],
                },
            ]
        })
        with patch.object(module_cycles.subprocess, 'check_output', return_value=metadata):
            discovered = list(module_cycles.targets({}))
        self.assertEqual(discovered, [
            ('demo-bin:demo-bin', 'demo-bin', ['--bin', 'demo-bin'], []),
            ('demo-bin:demo-bin[all-features]', 'demo-bin', ['--bin', 'demo-bin'], ['--all-features']),
            ('demo-both', 'demo-both', ['--lib'], []),
            ('demo-both:alpha', 'demo-both', ['--bin', 'alpha'], []),
            ('demo-both:zeta', 'demo-both', ['--bin', 'zeta'], []),
            ('demo-multi', 'demo-multi', ['--lib'], []),
            ('demo-multi[all-features]', 'demo-multi', ['--lib'], ['--all-features']),
            ('demo-multi[feature:alpha]', 'demo-multi', ['--lib'], ['--features', 'alpha']),
            ('demo-multi[feature:beta]', 'demo-multi', ['--lib'], ['--features', 'beta']),
        ])

    def test_independent_configs_avoid_false_union_cycles_and_catch_opt_only_cycles(self):
        # Default has a -> b; opt-in feature has mutually exclusive b -> a (no cycle in either alone).
        # A third config has both a -> b and b -> a (real cycle in that config only).
        dots = {
            (): graph([('c::a::f', 'c::b::g')]),
            ('--all-features',): graph([('c::b::h', 'c::a::x')]),
            ('--features', 'both'): graph([('c::a::f', 'c::b::g'), ('c::b::h', 'c::a::x')]),
        }
        fake_targets = [
            ('c', 'c', ['--lib'], []),
            ('c[all-features]', 'c', ['--lib'], ['--all-features']),
            ('c[feature:both]', 'c', ['--lib'], ['--features', 'both']),
        ]

        class Completed:
            returncode = 0
            stderr = ''

            def __init__(self, stdout):
                self.stdout = stdout

        def fake_run(cmd, **_kwargs):
            key = tuple(arg for arg in cmd if arg in ('--all-features', '--features', 'both'))
            return Completed(dots[key])

        with patch.object(module_cycles, 'targets', return_value=fake_targets), \
                patch.object(module_cycles.subprocess, 'run', side_effect=fake_run):
            found_cycles, found_edges, checked = module_cycles.measure('cargo-modules', {})
        self.assertEqual(checked, ['c', 'c[all-features]', 'c[feature:both]'])
        self.assertEqual(found_cycles, {'c[feature:both]': [['c::a', 'c::b']]})
        self.assertEqual(found_edges, {'c[feature:both]': [['c::a', 'c::b'], ['c::b', 'c::a']]})

    def test_record_allows_new_feature_config_matching_base_and_refuses_growth(self):
        c3, e3 = [['c::a', 'c::b', 'c::d']], [['c::a', 'c::b'], ['c::b', 'c::d'], ['c::d', 'c::a']]
        c2, e2 = [['c::a', 'c::b']], [['c::a', 'c::b'], ['c::b', 'c::a']]
        initial = {'configs': ['sample', 'sample[feature:existing]', 'sample:cli', 'other[all-features]'],
                   'cycles': {'sample': c3, 'sample[feature:existing]': c2, 'sample:cli': c2, 'other[all-features]': c3},
                   'edges': {'sample': e3, 'sample[feature:existing]': e2, 'sample:cli': e2, 'other[all-features]': e3}}
        cases = [
            ('clean_new_features', {'sample[all-features]': (c3, e3), 'sample[feature:frozen]': (c3, e3), 'sample:cli[feature:extra]': (c2, e2)}, True),
            ('extra_edge', {'sample[feature:frozen]': (c3, e3 + [['c::a', 'c::d']])}, False),
            ('existing_regrowth', {'sample[feature:existing]': (c3, e3)}, False),
            ('nested_suffix', {'other[all-features][feature:unused]': (c3, e3)}, False),
        ]
        for name, updates, should_pass in cases:
            with self.subTest(case=name), tempfile.TemporaryDirectory() as temporary:
                baseline = Path(temporary) / 'baseline.json'
                baseline.write_text(json.dumps(initial), encoding='utf-8')
                before = baseline.read_bytes()
                measured = ({**initial['cycles'], **{k: v[0] for k, v in updates.items()}},
                            {**initial['edges'], **{k: v[1] for k, v in updates.items()}},
                            sorted(set(initial['configs']) | set(updates)))
                with patch.object(module_cycles, 'BASELINE', baseline), \
                        patch.object(module_cycles.sys, 'argv', ['module_cycles.py', 'record']), \
                        patch.object(module_cycles, 'tool', return_value='cargo-modules'), \
                        patch.object(module_cycles, 'measure', return_value=measured):
                    self.assertEqual(module_cycles.main() == 0, should_pass)
                self.assertEqual(json.loads(baseline.read_text(encoding='utf-8'))['configs'], measured[2]) if should_pass else self.assertEqual(baseline.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
