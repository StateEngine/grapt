import io
import ntpath
import os
from pathlib import Path
from contextlib import contextmanager
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import gate_scratch


class ScratchTests(unittest.TestCase):
    def test_subprocess_temp_files_are_removed_on_success_and_failure(self):
        for failure in (False, True):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as parent:
                with patch.dict(os.environ, {'TMPDIR': parent, 'TMP': 'old-tmp'}, clear=False):
                    previous = dict(os.environ)
                    directory = None
                    try:
                        with gate_scratch.scratch() as directory:
                            subprocess.run([sys.executable, '-c',
                                'import tempfile; from pathlib import Path; '
                                'Path(tempfile.mkdtemp()) .joinpath("fixture").write_text("leftover")'],
                                check=True, timeout=10)
                            self.assertTrue(list(Path(directory).iterdir()))
                            if failure:
                                raise KeyboardInterrupt
                    except KeyboardInterrupt:
                        pass
                    self.assertFalse(Path(directory).exists())
                    self.assertEqual(dict(os.environ), previous)

    def test_residue_warning_is_bounded_and_keeps_a_passing_status(self):
        with tempfile.TemporaryDirectory() as directory:
            for number in range(65):
                (Path(directory) / f'secode-fixture-{number}').mkdir()
            (Path(directory) / 'unrelated').mkdir()
            with patch('sys.stderr', new_callable=io.StringIO) as warning:
                self.assertEqual(gate_scratch.residue(directory), 0)
            self.assertEqual(warning.getvalue().count(directory), 64)
            self.assertIn('1 more', warning.getvalue())
            self.assertNotIn('unrelated', warning.getvalue())

    def test_residue_of_none_and_clean_directories_pass_both_modes(self):
        self.assertEqual(gate_scratch.residue(None), 0)
        self.assertEqual(gate_scratch.residue(None, require_clean=True), 0)
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(gate_scratch.residue(directory), 0)
            self.assertEqual(gate_scratch.residue(directory, require_clean=True), 0)

    def test_residue_fails_the_gate_when_clean_is_required(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / 'secode-leak-1-0').mkdir()
            with patch('sys.stderr', new_callable=io.StringIO) as warning:
                self.assertEqual(gate_scratch.residue(directory, require_clean=True), 1)
        self.assertIn('secode-leak-1-0', warning.getvalue())

    def test_other_owner_prefixes_are_not_test_residue(self):
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / 'gate-internal').mkdir()
            (Path(directory) / 'other-thing').mkdir()
            self.assertEqual(gate_scratch.residue(directory, require_clean=True), 0)

    def test_unreadable_directory_keeps_a_pass_even_when_clean_is_required(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(gate_scratch.os, 'listdir', side_effect=PermissionError('fixture denied')):
                self.assertEqual(gate_scratch.residue(directory, require_clean=True), 0)

    def test_cleanup_failure_preserves_system_exit(self):
        with tempfile.TemporaryDirectory() as parent:
            temporary = tempfile.TemporaryDirectory(dir=parent)
            with patch.object(gate_scratch.tempfile, 'TemporaryDirectory', return_value=temporary), \
                    patch.object(temporary, 'cleanup', side_effect=OSError('Directory not empty')), \
                    patch('sys.stderr') as warning:
                with self.assertRaises(SystemExit) as raised:
                    with gate_scratch.scratch():
                        raise SystemExit(130)
                self.assertEqual(raised.exception.code, 130)
                self.assertTrue(warning.write.called)
            temporary.cleanup()


class HeadlessBaseTests(unittest.TestCase):
    @contextmanager
    def _absent_override(self):
        previous = os.environ.pop(gate_scratch.HEADLESS_BASE_ENV, None)
        try:
            yield
        finally:
            if previous is not None:
                os.environ[gate_scratch.HEADLESS_BASE_ENV] = previous

    def test_governed_absolute_requires_rooted_and_drive_qualified_paths(self):
        with patch.object(gate_scratch.os, 'name', 'nt'), \
                patch.object(gate_scratch.os.path, 'isabs', ntpath.isabs), \
                patch.object(gate_scratch.os.path, 'splitdrive', ntpath.splitdrive):
            for value in ('C:temp', 'C:', r'\temp', 'temp', ''):
                self.assertFalse(gate_scratch.governed_absolute(value), value)
            for value in (r'\\server\share\temp', r'\\?\C:\temp'):
                self.assertFalse(gate_scratch.governed_absolute(value), value)
            self.assertTrue(gate_scratch.governed_absolute(r'C:\temp'))
        self.assertFalse(gate_scratch.governed_absolute('relative/path'))
        if os.name != 'nt':
            self.assertTrue(gate_scratch.governed_absolute('/tmp/headless'))
        else:
            self.assertTrue(gate_scratch.governed_absolute(r'C:\temp'))

    def test_override_must_be_nonempty_absolute_existing_directory(self):
        with tempfile.TemporaryDirectory() as base:
            (Path(base) / 'file').write_text('not a directory')
            cases = [('', 'empty'), ('relative/dir', 'absolute'),
                     (str(Path(base) / 'missing'), 'existing'),
                     (str(Path(base) / 'file'), 'existing')]
            for value, phrase in cases:
                with self.subTest(value=value), \
                        patch.dict(os.environ, {gate_scratch.HEADLESS_BASE_ENV: value}):
                    with self.assertRaises(gate_scratch.HeadlessBaseRefused) as raised:
                        gate_scratch.resolve_headless_base(base)
                self.assertIn(phrase, str(raised.exception))
            with patch.dict(os.environ, {gate_scratch.HEADLESS_BASE_ENV: base}):
                self.assertEqual(gate_scratch.resolve_headless_base('ignored'), base)

    def test_default_base_is_the_run_scratch_on_unix(self):
        if os.name == 'nt':
            self.skipTest('unix default')
        with self._absent_override(), tempfile.TemporaryDirectory() as base:
            self.assertEqual(gate_scratch.resolve_headless_base(base), base)

    def test_windows_default_is_the_drive_temp(self):
        with self._absent_override(), tempfile.TemporaryDirectory() as base, \
                patch.object(gate_scratch.os, 'name', 'nt'), \
                patch.object(gate_scratch, '_is_directory',
                             side_effect=lambda path: path == gate_scratch.WINDOWS_HEADLESS_BASE):
            self.assertEqual(gate_scratch.resolve_headless_base(base), gate_scratch.WINDOWS_HEADLESS_BASE)

    def test_missing_default_base_refuses(self):
        with self._absent_override(), tempfile.TemporaryDirectory() as parent:
            missing = str(Path(parent) / 'gone')
            if os.name == 'nt':
                with patch.object(gate_scratch.os, 'name', 'nt'), \
                        patch.object(gate_scratch, '_is_directory', return_value=False):
                    with self.assertRaises(gate_scratch.HeadlessBaseRefused) as raised:
                        gate_scratch.resolve_headless_base(missing)
            else:
                with self.assertRaises(gate_scratch.HeadlessBaseRefused) as raised:
                    gate_scratch.resolve_headless_base(missing)
            self.assertIn('does not exist', str(raised.exception))

    def test_scratch_publishes_and_restores_the_headless_base(self):
        previous = os.environ.pop(gate_scratch.HEADLESS_BASE_ENV, None)
        try:
            with gate_scratch.scratch() as directory:
                self.assertEqual(os.environ.get(gate_scratch.HEADLESS_BASE_ENV),
                                 gate_scratch.active_headless_base)
                if os.name != 'nt':
                    self.assertEqual(os.environ.get(gate_scratch.HEADLESS_BASE_ENV), directory)
            self.assertNotIn(gate_scratch.HEADLESS_BASE_ENV, os.environ)
            with patch.dict(os.environ, {gate_scratch.HEADLESS_BASE_ENV: ''}):
                with self.assertRaises(gate_scratch.HeadlessBaseRefused):
                    with gate_scratch.scratch():
                        pass
                self.assertEqual(os.environ.get(gate_scratch.HEADLESS_BASE_ENV), '')
        finally:
            if previous is not None:
                os.environ[gate_scratch.HEADLESS_BASE_ENV] = previous

    def test_judge_flags_only_the_headless_family(self):
        with tempfile.TemporaryDirectory() as base:
            (Path(base) / 'secode-live-headless1-4242-case').mkdir()
            (Path(base) / 'secode-live-headless2-4243-case').mkdir()
            (Path(base) / 'keep-secode-live-headless1-4242').mkdir()
            (Path(base) / 'unrelated').mkdir()
            with patch('sys.stderr', new_callable=io.StringIO) as out:
                self.assertEqual(gate_scratch.judge_headless_residue(base), 0)
            text = out.getvalue()
            self.assertIn('secode-live-headless1-4242-case', text)
            self.assertNotIn('secode-live-headless2-4243-case', text)
            self.assertNotIn('keep-secode-live-headless1-4242', text)
            self.assertNotIn('unrelated', text)
            with patch('sys.stderr', new_callable=io.StringIO) as out:
                self.assertEqual(gate_scratch.judge_headless_residue(base, require_clean=True), 1)
            self.assertIn('secode-live-headless1-4242-case', out.getvalue())

    def test_judge_passes_none_and_clean_and_fails_unobservable(self):
        self.assertEqual(gate_scratch.judge_headless_residue(None, require_clean=True), 0)
        with tempfile.TemporaryDirectory() as base:
            self.assertEqual(gate_scratch.judge_headless_residue(base, require_clean=True), 0)
        with tempfile.TemporaryDirectory() as parent:
            missing = str(Path(parent) / 'gone')
            with patch('sys.stderr', new_callable=io.StringIO) as out:
                self.assertEqual(gate_scratch.judge_headless_residue(missing), 0)
            self.assertIn('cannot observe', out.getvalue())
            with patch('sys.stderr', new_callable=io.StringIO):
                self.assertEqual(gate_scratch.judge_headless_residue(missing, require_clean=True), 1)
            with patch.object(gate_scratch.os, 'listdir', side_effect=PermissionError('denied')):
                with patch('sys.stderr', new_callable=io.StringIO):
                    self.assertEqual(gate_scratch.judge_headless_residue(base), 0)
                with patch('sys.stderr', new_callable=io.StringIO):
                    self.assertEqual(gate_scratch.judge_headless_residue(base, require_clean=True), 1)

    def test_judge_listing_is_bounded(self):
        with tempfile.TemporaryDirectory() as base:
            for number in range(65):
                (Path(base) / f'secode-live-headless1-4242-{number}').mkdir()
            with patch('sys.stderr', new_callable=io.StringIO) as out:
                self.assertEqual(gate_scratch.judge_headless_residue(base, require_clean=True), 1)
            text = out.getvalue()
            self.assertEqual(text.count('secode-live-headless1-4242-'), 64)
            self.assertIn('1 more', text)

if __name__ == '__main__':
    unittest.main()
