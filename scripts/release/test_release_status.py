"""A failed release run records what happened and tells the coordinator."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'release'))
import release_status as status


class ReleaseStatusTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name) / 'status'

    def test_a_failed_run_names_the_night_commit_step_and_tail(self):
        failure = status.failed('licenses.py check', ['one', 'two', 'FAIL full: licenses'], 1)
        body = status.announcement('20261118T090000Z', '5d213cee997a', failure, self.root)
        self.assertIn('20261118T090000Z', body)
        self.assertIn('5d213cee997a', body)
        self.assertIn('licenses.py check', body)
        self.assertIn('FAIL full: licenses', body)
        self.assertIn('Exit: 1', body)
        self.assertIn(str(self.root), body)

    def test_the_tail_is_bounded(self):
        noisy = ['x' * 5000 for _ in range(50)]
        failure = status.failed('step', noisy, 1)
        self.assertLessEqual(len(failure['tail']), status.TAIL_BYTES)
        self.assertLessEqual(len(failure['tail'].splitlines()), status.TAIL_LINES)

    def test_a_record_survives_and_the_newest_is_read_back(self):
        status.write_status(self.root, 'run-a', {'run': 'run-a', 'outcome': 'failed'})
        status.write_status(self.root, 'run-b', {'run': 'run-b', 'outcome': 'failed'})
        latest = status.latest_status(self.root)
        self.assertIsNotNone(latest)
        self.assertEqual(latest['schema'], status.STATUS_SCHEMA)
        self.assertIn(latest['run'], {'run-a', 'run-b'})

    def test_a_malformed_record_is_ignored_rather_than_trusted(self):
        status.write_status(self.root, 'good', {'run': 'good', 'outcome': 'failed'})
        (self.root / 'bad.json').write_text('{')
        (self.root / 'wrong.json').write_text(json.dumps({'schema': 'other', 'updated': 1}))
        latest = status.latest_status(self.root)
        self.assertIsNotNone(latest)
        self.assertEqual(latest['run'], 'good')

    def test_no_record_reads_as_none_not_as_success(self):
        self.assertIsNone(status.latest_status(self.root))

    def test_a_mail_failure_is_reported_and_never_raised(self):
        def broken(_):
            raise RuntimeError('transport down')
        reason = status.mail('agt-test', 'body', send=broken)
        self.assertIsNotNone(reason)
        self.assertIn('transport down', reason)

    def test_a_successful_mail_reports_no_error(self):
        sent = []
        self.assertIsNone(status.mail('agt-test', 'body', send=sent.append))
        self.assertEqual(sent, ['body'])

    def test_an_oversized_record_is_refused(self):
        with self.assertRaises(ValueError):
            status.write_status(self.root, 'huge', {'run': 'huge', 'blob': 'x' * status.STATUS_BYTES})
        self.assertFalse((self.root / 'huge.json').exists())


if __name__ == '__main__':
    unittest.main()
