"""The cleanup journal records a pass, bounded, and refuses a damaged one."""
import fcntl
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

import clean_journal


class CleanJournalTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        # The default journal lives under the operator's data root. Pin it into
        # this fixture so no test can ever write there.
        pinned = patch.object(clean_journal, 'journal_path',
                              lambda directory=None: self.directory/'target-clean.json'
                              if directory is None else Path(directory)/'target-clean.json')
        pinned.start()
        self.addCleanup(pinned.stop)

    def rows(self, path=None):
        path = clean_journal.journal_path(self.directory) if path is None else path
        return json.loads(path.read_text())['samples']

    def test_a_removal_keeps_its_before_and_witnessed_after_size(self):
        path = clean_journal.append([
            clean_journal.row('/target', 'removed', bytes_before=4096, bytes_after=0,
                              detail='idle'),
            clean_journal.row('/other', 'retained', bytes_before=8192, detail='idle 12s'),
        ], self.directory)
        rows = self.rows(path)
        self.assertEqual([row['outcome'] for row in rows], ['removed', 'retained'])
        self.assertEqual((rows[0]['bytes_before'], rows[0]['bytes_after']), (4096, 0))
        # A kept target has no after size: nothing observed it shrink.
        self.assertEqual(rows[1]['bytes_before'], 8192)
        self.assertIsNone(rows[1]['bytes_after'])

    def test_successive_passes_append_and_stay_within_both_bounds(self):
        with patch.object(clean_journal, 'JOURNAL_ROWS', 3), \
                patch.object(clean_journal, 'JOURNAL_BYTES', 4096):
            for index in range(6):
                clean_journal.append([clean_journal.row(f'/t{index}', 'retained')], self.directory)
        rows = self.rows()
        self.assertEqual([row['target'] for row in rows], ['/t3', '/t4', '/t5'])
        self.assertLessEqual(clean_journal.journal_path(self.directory).stat().st_size, 4096)

    def test_a_damaged_journal_is_refused_rather_than_overwritten(self):
        path = clean_journal.journal_path(self.directory)
        path.parent.mkdir(parents=True, exist_ok=True)
        for damaged in ('{', json.dumps({'version': 2, 'samples': []}),
                        json.dumps({'version': 1, 'samples': [{'target': 'x'}]})):
            with self.subTest(damaged=damaged):
                path.write_text(damaged)
                with self.assertRaises(RuntimeError):
                    clean_journal.append([clean_journal.row('/target', 'retained')], self.directory)
                self.assertEqual(path.read_text(), damaged)

    def test_an_empty_pass_writes_nothing(self):
        self.assertIsNone(clean_journal.append([], self.directory))
        self.assertFalse(clean_journal.journal_path(self.directory).exists())

    def test_an_unknown_outcome_is_refused(self):
        with self.assertRaises(ValueError):
            clean_journal.row('/target', 'deleted')

    def test_a_busy_journal_never_waits_for_another_writer(self):
        path = clean_journal.journal_path(self.directory)
        with path.with_name(path.name + '.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            child = subprocess.run([
                sys.executable, '-B', '-c',
                "import clean_journal,sys; clean_journal.append("
                "[clean_journal.row('/target','retained')],sys.argv[1])",
                str(self.directory),
            ], env=dict(os.environ, PYTHONPATH=str(Path(__file__).parent)),
                capture_output=True, text=True, timeout=2)
        self.assertEqual(child.returncode, 0, child.stderr)

    def test_a_fifo_lock_cannot_block_cleanup_telemetry(self):
        path = clean_journal.journal_path(self.directory)
        os.mkfifo(path.with_name(path.name + '.lock'))
        child = subprocess.run([
            sys.executable, '-B', '-c',
            "import clean_journal,sys\n"
            "try: clean_journal.append([clean_journal.row('/target','retained')],sys.argv[1])\n"
            "except (OSError,ValueError,RuntimeError): pass\n",
            str(self.directory),
        ], env=dict(os.environ, PYTHONPATH=str(Path(__file__).parent)),
            capture_output=True, text=True, timeout=2)
        self.assertEqual(child.returncode, 0, child.stderr)


if __name__ == '__main__':
    unittest.main()
