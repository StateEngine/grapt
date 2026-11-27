from pathlib import Path
import tempfile
import unittest

from budget import Budget, BudgetStop, amount, key_material


class BudgetTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.key = Path(self.temp.name)/'key'
        self.key.write_text('synthetic-key')
        self.key.chmod(0o600)

    def test_weekly_and_run_usage_both_stop_and_cannot_restart(self):
        for rows in [[(200, 0), (230, 30)], [(200, 69), (201, 70)]]:
            observed = iter([{'usage': a, 'usage_weekly': b} for a, b in rows])
            budget = Budget(self.key, observer=lambda _: next(observed))
            budget.check()
            with self.assertRaises(BudgetStop): budget.check()
            with self.assertRaises(BudgetStop): budget.check()

    def test_unknown_negative_or_nonfinite_usage_does_not_become_zero(self):
        for value in [None, True, -1, 'NaN', 'Infinity', 'bad']:
            with self.subTest(value=value), self.assertRaises(BudgetStop): amount(value)

    def test_public_or_linked_key_is_refused_without_printing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / 'key'
            key.write_text('test-secret')
            key.chmod(0o644)
            with self.assertRaises(BudgetStop): key_material(key)
            key.chmod(0o600)
            self.assertEqual(key_material(key), 'test-secret')
            alias = key.with_name('alias'); alias.symlink_to(key)
            with self.assertRaises(BudgetStop): key_material(alias)

    def test_missing_key_and_malformed_counter_stop_only_once(self):
        with tempfile.TemporaryDirectory() as directory:
            budget = Budget(Path(directory)/'absent')
            with self.assertRaisesRegex(BudgetStop, 'could not be verified'): budget.check()
            self.assertTrue(budget.stopped)
        observations = []
        def malformed(_):
            observations.append(True)
            return {}
        budget = Budget(self.key, observer=malformed)
        with self.assertRaises(BudgetStop): budget.check()
        with self.assertRaises(BudgetStop): budget.check()
        self.assertEqual(len(observations), 1)

    def test_key_rotation_refuses_before_observing_another_key(self):
        observations = []
        def observer(_):
            observations.append(True)
            return {'usage': 1, 'usage_weekly': 1}
        budget = Budget(self.key, observer=observer)
        budget.check()
        self.key.write_text('rotated-synthetic-key')
        with self.assertRaisesRegex(BudgetStop, 'key changed'):
            budget.check()
        self.assertEqual(len(observations), 1)
        self.assertTrue(budget.stopped)
