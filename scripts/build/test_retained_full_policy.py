"""Regression for the verifier side of two conflicting refusals."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import gate_policy
import receipt


class RetainedFullPolicyTests(unittest.TestCase):
    def refusal(self, mode, require_full=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'composed.log'
            log.write_text('# secode-gate-run 0123456789abcdef0123456789abcdef\nSynthetic receipt fixture; no full-gate claim.\n', newline="\n")
            source = dict(commit='a' * 40, clean=True)
            with patch.object(receipt, 'source', return_value=source):
                receipt.write(log, mode, source, [], 1, base='b' * 40)
            return subprocess.run([sys.executable, receipt.__file__, str(log), source['commit'],
                '--base', 'b' * 40, '--retained-commit', 'c' * 40,
                '--retained-base', 'd' * 40, '--retained-log', str(root / 'full.log'),
                *(['--require-full'] if require_full else [])],
                capture_output=True, text=True, timeout=15)

    def test_affected_receipt_plus_retained_full_explains_the_tier_boundary(self):
        result = self.refusal('affected')
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(result.stderr.rstrip().endswith('ValueError: ' + gate_policy.RETAINED_FULL_POLICY), result.stderr)

    def test_tier1_reviewed_receipt_requires_a_full_gate_on_the_composed_commit(self):
        result = self.refusal('reviewed', require_full=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(result.stderr.rstrip().endswith('ValueError: ' + gate_policy.RETAINED_FULL_POLICY), result.stderr)


if __name__ == '__main__':
    unittest.main()
