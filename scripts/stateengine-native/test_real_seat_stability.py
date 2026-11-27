"""Prove idle observation rejects flapping, dead services and missing evidence."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

from real_seat_report import Journal
from real_seat_stability import observe_stability, subscription_lines


class StabilityTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.journal = self.enterContext(Journal(self.root))
        self.path = self.root / 'product.stderr'
        self.opened = 'native mail subscription opened session=ses_fixture'
        self.path.write_text(self.opened + '\n')
        self.control = Mock(root=self.root)
        self.child = Mock()
        self.child.poll.return_value = None
        self.checks = {}
        self.now = 0

    def observe(self, change=None):
        def tick(_seconds):
            self.now += 60
            if change:
                change()
        with patch('real_seat_stability.time.monotonic', side_effect=lambda: self.now), \
             patch('real_seat_stability.time.sleep', side_effect=tick):
            return observe_stability(self.control, 'ses_fixture', self.child,
                                     self.journal, self.checks)

    def test_unchanged_live_subscription_needs_the_whole_two_minutes(self):
        self.assertTrue(self.observe())
        row = self.journal.rows[-1][0]
        self.assertEqual(row['idle_seconds'], 120)
        self.assertTrue(self.checks['native_mail_subscription_stable'][0])

    def test_drop_and_reopen_between_samples_still_fails_with_exact_lines(self):
        dropped = 'native mail subscription dropped session=ses_fixture confirmed=true'
        def flap():
            self.path.write_text(self.opened + '\n' + dropped + '\n' + self.opened + '\n')
        self.assertFalse(self.observe(flap))
        changes = self.journal.rows[-1][0]['changes']
        self.assertEqual(changes[0], {'source_line': 2, 'exact_line': dropped})
        self.assertEqual(changes[1]['exact_line'], self.opened)

    def test_repeated_open_without_drop_also_fails(self):
        self.assertFalse(self.observe(lambda: self.path.write_text((self.opened + '\n') * 2)))

    def test_unavailable_notice_during_idle_fails(self):
        def unavailable():
            self.path.write_text(self.opened + '\nnative mail unavailable '
                                 'session=ses_fixture stage=advance cause=Offline\n')
        self.assertFalse(self.observe(unavailable))

    def test_initially_closed_or_absent_subscription_cannot_pass(self):
        for content in ('', self.opened + '\nnative mail subscription dropped '
                        'session=ses_fixture confirmed=true\n'):
            with self.subTest(content=content):
                self.path.write_text(content)
                self.assertFalse(self.observe())

    def test_product_exit_cannot_leave_a_stale_open_as_success(self):
        self.child.poll.return_value = 1
        self.assertFalse(self.observe())
        self.assertIn('product exited', self.journal.rows[-1][0]['reason'])

    def test_another_sessions_churn_does_not_fail_this_subscription(self):
        def foreign():
            self.path.write_text(self.opened + '\nnative mail subscription dropped '
                                 'session=ses_fixture_other confirmed=true\n')
        self.assertTrue(self.observe(foreign))

    def test_log_replacement_and_missing_or_oversized_evidence_refuse(self):
        self.assertFalse(self.observe(lambda: self.path.write_text('unrelated\n')))
        self.path.unlink()
        with self.assertRaisesRegex(ValueError, 'absent'):
            subscription_lines(self.root, 'ses_fixture')
        self.path.write_bytes(b'x' * (2 * 1024 * 1024 + 1))
        with self.assertRaisesRegex(ValueError, 'exceeded'):
            subscription_lines(self.root, 'ses_fixture')


if __name__ == '__main__':
    unittest.main()
