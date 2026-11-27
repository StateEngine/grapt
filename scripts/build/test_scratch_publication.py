"""Exercise scratch refusal through the gate's success-evidence boundary."""
from contextlib import ExitStack, nullcontext, redirect_stderr, redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import gate
import gate_scratch
import receipt


class ScratchPublicationTests(unittest.TestCase):
    def run_gate(self, *, leak, strict, list_only=False, warm=False,
                 preparation_refusal=False, swap_previous=False, windows_text=False):
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            root = Path(directory)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            subprocess.run(['git', '-c', 'user.name=Fixture', '-c',
                            'user.email=fixture@users.noreply.github.com',
                            'commit', '--allow-empty', '-qm', 'Fixture'], cwd=root, check=True)
            previous = Path.cwd()
            os.chdir(root)
            stack.callback(os.chdir, previous)
            commit = receipt.source()['commit']
            log = root / 'result.log'
            target = root / 'target'
            target.mkdir()
            output = subprocess.check_output
            command = [sys.executable, '-c', 'print("fixture passed")']
            path_open = Path.open

            def platform_open(path, mode='r', *args, **kwargs):
                # Windows text files translate newlines unless the writer
                # deliberately chooses a platform-independent line ending.
                if windows_text and path == log and mode == 'w' and kwargs.get('newline') is None:
                    kwargs['newline'] = '\r\n'
                return path_open(path, mode, *args, **kwargs)

            stack.enter_context(patch.object(Path, 'open', platform_open))

            def execute(command, log, deadline, **kwargs):
                subprocess.run(command, cwd=kwargs['cwd'], stdout=log,
                               stderr=subprocess.STDOUT, check=True, timeout=10)
                return 0.0

            def metadata_or_git(command, **kwargs):
                if list(command[:2]) == ['cargo', 'metadata']:
                    return json.dumps(dict(workspace_members=[], packages=[])).encode()
                return output(command, **kwargs)

            stack.enter_context(patch.object(subprocess, 'check_output', side_effect=metadata_or_git))
            stack.enter_context(patch.object(gate, 'output', side_effect=lambda *cmd: metadata_or_git(cmd)))
            stack.enter_context(patch.dict(os.environ, dict(CARGO_TARGET_DIR=str(target),
                SECODE_TARGET_ROOT=str(root), SECODE_FIXTURES=str(root))))
            stack.enter_context(patch.object(gate.publication_scan, 'check', return_value=[]))
            stack.enter_context(patch.object(gate.gate_plan, 'required_mode', return_value='affected'))
            stack.enter_context(patch.object(gate.gate_plan, 'commands', return_value=[command]))
            stack.enter_context(patch.object(gate, 'execute', side_effect=execute))
            stack.enter_context(patch.object(gate.evict_targets, 'prepare',
                side_effect=lambda *args, **kwargs: (root / 'lease').open('a')))
            stack.enter_context(patch.object(gate.evict_targets, 'inherit'))
            stack.enter_context(patch.object(gate.evict_targets, 'retain'))
            stack.enter_context(patch.object(gate, 'claim_target'))
            stack.enter_context(patch.object(gate.gate_parallel, 'native', return_value=nullcontext(None)))
            (root / '.git/info/exclude').write_text('result.log*\ntarget/\nlease\n')
            arguments = ['affected', '--base', commit, '--log', str(log)]
            if strict:
                arguments.append('--fail-scratch-residue')
            if list_only:
                arguments.append('--list')
            stack.enter_context(patch.object(sys, 'argv', ['gate.py', *arguments]))
            stdout, stderr = io.StringIO(), io.StringIO()
            with gate_scratch.scratch() as scratch:
                # A passing real child leaves its test-owned directory behind.
                if leak:
                    subprocess.run([sys.executable, '-c',
                        'import os; from pathlib import Path; '
                        'Path(os.environ["TMPDIR"], "secode-unreleased-fixture").mkdir()'],
                        check=True, timeout=10)
                with redirect_stdout(stdout), redirect_stderr(stderr):
                    if warm:
                        ordinary = [arg for arg in sys.argv if arg != '--fail-scratch-residue']
                        with patch.object(sys, 'argv', ordinary), \
                                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                            self.assertEqual(gate.main(), 0)
                        receipt.verify(log, commit, base=commit)
                        previous_receipt = log.with_suffix('.log.receipt.json').read_bytes()
                    if preparation_refusal:
                        with patch.object(gate.evict_targets, 'prepare',
                                side_effect=RuntimeError('fixture admission unavailable')):
                            status = gate.main()
                    else:
                        status = gate.main()
                record_path = log.with_suffix('.log.receipt.json')
                record = json.loads(record_path.read_text()) if record_path.exists() else None
                if record is not None:
                    self.assertEqual(record['source_before'], dict(commit=commit, clean=True))
                    self.assertEqual(record['source_after'], dict(commit=commit, clean=True))
                    self.assertEqual(record['log_sha256'], receipt.digest(log))
                    self.assertEqual(record['exit'], 0)
                    receipt.verify(log, commit, base=commit)
                    if swap_previous:
                        current = record_path.read_bytes()
                        try:
                            record_path.write_bytes(previous_receipt)
                            with self.assertRaises(ValueError):
                                receipt.verify(log, commit, base=commit)
                        finally:
                            record_path.write_bytes(current)
                        receipt.verify(log, commit, base=commit)
                self.assertEqual(gate_scratch.active, scratch)
            self.assertIsNone(gate_scratch.active)
            self.assertFalse(Path(scratch).exists())
            return status, stdout.getvalue(), stderr.getvalue(), record

    def test_strict_residue_refuses_without_reusable_success_evidence(self):
        status, output, warning, record = self.run_gate(leak=True, strict=True)
        self.assertEqual(status, 1)
        self.assertIn('secode-unreleased-fixture', warning)
        self.assertNotIn('PASS', output)
        self.assertIsNone(record)

    def test_strict_clean_run_publishes_bound_success(self):
        status, output, warning, record = self.run_gate(leak=False, strict=True)
        self.assertEqual(status, 0)
        self.assertIn('PASS affected:', output)
        self.assertEqual(warning, '')
        self.assertIsNotNone(record)

    def test_clean_run_publishes_success_with_windows_text_file_newlines(self):
        status, output, warning, record = self.run_gate(
            leak=False, strict=True, windows_text=True)
        self.assertEqual(status, 0)
        self.assertIn('PASS affected:', output)
        self.assertEqual(warning, '')
        self.assertIsNotNone(record)

    def test_strict_refusal_invalidates_an_earlier_success_at_the_same_log_path(self):
        status, output, warning, record = self.run_gate(leak=True, strict=True, warm=True)
        self.assertEqual(status, 1)
        self.assertIn('secode-unreleased-fixture', warning)
        self.assertNotIn('PASS', output)
        self.assertIsNone(record, 'earlier success still verifies after the strict refusal')

    def test_preparation_refusal_invalidates_success_before_any_new_log_bytes(self):
        status, output, warning, record = self.run_gate(
            leak=False, strict=True, warm=True, preparation_refusal=True)
        self.assertEqual(status, 1)
        self.assertIn('fixture admission unavailable', warning)
        self.assertNotIn('PASS', output)
        self.assertIsNone(record, 'earlier success still verifies after admission refuses')

    def test_success_receipt_from_an_earlier_run_cannot_qualify_the_next_run(self):
        status, output, warning, record = self.run_gate(
            leak=False, strict=True, warm=True, swap_previous=True)
        self.assertEqual(status, 0)
        self.assertIn('PASS affected:', output)
        self.assertEqual(warning, '')
        self.assertIsNotNone(record)

    def test_default_residue_warns_and_keeps_success(self):
        status, output, warning, record = self.run_gate(leak=True, strict=False)
        self.assertEqual(status, 0)
        self.assertIn('secode-unreleased-fixture', warning)
        self.assertIn('PASS affected:', output)
        self.assertIsNotNone(record)

    def test_listing_never_qualifies_or_judges_scratch(self):
        status, output, warning, record = self.run_gate(leak=True, strict=True, list_only=True)
        self.assertEqual(status, 0)
        self.assertNotIn('PASS', output)
        self.assertEqual(warning, '')
        self.assertIsNone(record)


if __name__ == '__main__':
    unittest.main()
