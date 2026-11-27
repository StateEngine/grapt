"""Judge the controller's public reader of exact retry policy."""
from datetime import date
from pathlib import Path
import tempfile
import unittest

import batch_report as report


TODAY = date(2030, 1, 10)


def entry(name='module::keeps_receipt', owner='fixture-owner', expiry='2030-01-17'):
    return (f'# flaky-quarantine: owner={owner} scorecard=Q1 expiry={expiry}\n'
            '[[profile.gate.overrides]]\n'
            f'filter = "test(={name})"\nretries = 1\n')


def policy(text):
    with tempfile.TemporaryDirectory(prefix='batch-policy-') as temporary:
        root = Path(temporary)
        (root / '.config').mkdir()
        (root / '.config/nextest.toml').write_text(text, encoding='utf-8')
        return report.quarantine(root, today=TODAY)


class BatchQuarantine(unittest.TestCase):
    def test_missing_policy_and_valid_profiles_without_overrides_are_empty(self):
        with tempfile.TemporaryDirectory(prefix='batch-policy-') as temporary:
            self.assertEqual(report.quarantine(Path(temporary), today=TODAY), {})
        for text in ['', '[profile.gate]\nretries = 0\n', '[profile.other]\nretries = 0\n']:
            with self.subTest(text=text):
                self.assertEqual(policy(text), {})

    def test_single_component_and_qualified_names_remain_raw_exact_test_names(self):
        for name in ['keeps_receipt', 'module::keeps_receipt', 'module::nested::keeps_receipt']:
            with self.subTest(name=name):
                self.assertEqual(policy(entry(name)), {
                    name: {'owner': 'fixture-owner', 'scorecard': 'Q1', 'expiry': '2030-01-17'}})

    def test_every_override_requires_its_immediately_preceding_annotation(self):
        bare = '[[profile.gate.overrides]]\nfilter = "test(=module::other)"\nretries = 1\n'
        for text in [bare, entry() + '\n' + bare,
                     entry().replace('[[profile', '\n[[profile', 1),
                     entry().replace('# flaky-quarantine:', '# unrelated:', 1)]:
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    policy(text)

    def test_quarantine_has_an_inclusive_sixty_four_entry_bound(self):
        text = '\n'.join(entry(f'module::case_{index}') for index in range(64))
        self.assertEqual(len(policy(text)), 64)
        with self.assertRaises(ValueError):
            policy(text + '\n' + entry('module::case_64'))

    def test_owner_identifiers_cannot_carry_paths_or_punctuation(self):
        for owner in ['fixture.owner', 'fixture/owner', '-fixture', 'fixture=owner']:
            with self.subTest(owner=owner):
                with self.assertRaises(ValueError):
                    policy(entry(owner=owner))

    def test_expiry_is_inclusive_today_through_seven_days(self):
        for expiry in ['2030-01-10', '2030-01-17']:
            with self.subTest(expiry=expiry):
                self.assertEqual(policy(entry(expiry=expiry))['module::keeps_receipt']['expiry'], expiry)
        for expiry in ['2030-01-09', '2030-01-18', '2030-02-30']:
            with self.subTest(expiry=expiry):
                with self.assertRaises(ValueError):
                    policy(entry(expiry=expiry))

    def test_malformed_profile_gate_and_override_shapes_are_policy_errors(self):
        for text in ['profile = 1\n', 'profile = []\n', '[profile]\ngate = 1\n',
                     '[profile]\ngate = []\n', '[profile.gate]\noverrides = 1\n',
                     '[profile.gate]\noverrides = [1]\n']:
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    policy(text)

    def test_filters_retries_and_duplicate_names_cannot_broaden_the_policy(self):
        for text in [entry().replace('test(=module::keeps_receipt)', 'all()'),
                     entry().replace('retries = 1', 'retries = 2'),
                     entry().replace('retries = 1', 'retries = true'),
                     entry().replace('retries = 1', 'retries = 1\nthreads-required = 1'),
                     entry() + '\n' + entry()]:
            with self.subTest(text=text):
                with self.assertRaises(ValueError):
                    policy(text)


if __name__ == '__main__':
    unittest.main()
