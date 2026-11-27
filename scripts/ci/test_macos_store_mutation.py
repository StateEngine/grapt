"""Exercise CI refusal, attribution, cleanup and restoration without native Cargo."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import macos_store_mutation as harness


def transcript(failing=False):
    lines = [f"test authority::tests::{name} ... {'FAILED' if failing else 'ok'}"
             for name in sorted(harness.EXPECTED)]
    if failing:
        lines += [f"---- authority::tests::{name} stdout ----"
                  for name in sorted(harness.EXPECTED)]
    return "\n".join(lines)


class StoreMutationTests(unittest.TestCase):
    def test_requires_execution_and_attributed_failure(self):
        harness.check_result(transcript(), 0, False)
        harness.check_result(transcript(True), 101, True)
        cases = [("test result: ok. 0 passed; 0 failed;", 0, False),
                 ("error: compile failed", 101, True),
                 (transcript(), 101, False),
                 (transcript(), 0, True),
                 (transcript(True).replace("fresh_open stdout", "unrelated stdout"), 101, True),
                 (transcript() + "\n" + transcript(), 0, False)]
        for output, code, failing in cases:
            with self.subTest(code=code, failing=failing, output=output[:50]):
                with self.assertRaises(RuntimeError):
                    harness.check_result(output, code, failing)

    def test_restores_source_when_mutant_phase_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/grapt-store/src/migration/authority.rs"
            source.parent.mkdir(parents=True)
            original = ("before\r\n" + harness.ANCHOR + "\nafter\n").encode()
            source.write_bytes(original)
            calls = []

            def phase(*args):
                calls.append(args)
                if len(calls) == 2:
                    self.assertNotIn(harness.ANCHOR.encode(), source.read_bytes())
                    raise RuntimeError("mutation failed")

            with patch.object(harness, "run_phase", side_effect=phase):
                with self.assertRaisesRegex(RuntimeError, "mutation failed"):
                    harness.exercise(root, root / "logs", ["cargo"])
            self.assertEqual(source.read_bytes(), original)

    def test_refuses_missing_or_duplicate_anchor_before_running(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "crates/grapt-store/src/migration/authority.rs"
            source.parent.mkdir(parents=True)
            for contents in ("missing", harness.ANCHOR * 2):
                source.write_text(contents)
                with patch.object(harness, "run_phase") as phase:
                    with self.assertRaisesRegex(RuntimeError, "exactly once"):
                        harness.exercise(root, root / "logs", ["cargo"])
                    phase.assert_not_called()

    def test_phase_records_output_and_refuses_forged_green_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            program = f"print({transcript()!r}); raise SystemExit(1)"
            with self.assertRaises(RuntimeError):
                harness.run_phase(root, [sys.executable, "-c", program], root / "phase.log", False)
            self.assertIn("existing_open", (root / "phase.log").read_text())

    def test_timeout_kills_the_owned_child_group(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "child-survived"
            child = "import pathlib,time; time.sleep(0.5); pathlib.Path('child-survived').touch()"
            parent = f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',{child!r}]); time.sleep(10)"
            with self.assertRaisesRegex(RuntimeError, "timed out"):
                harness.run_phase(root, [sys.executable, "-c", parent], root / "phase.log", False,
                                  timeout=0.2)
            # A separate bounded process gives an orphaned child time to expose a leak.
            subprocess.run([sys.executable, "-c", "import time; time.sleep(0.6)"],
                           check=True, timeout=2)
            self.assertFalse(marker.exists())

    def test_output_bound_stops_the_phase(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(harness, "MAX_LOG_BYTES", 1024):
                with self.assertRaisesRegex(RuntimeError, "log bound"):
                    harness.run_phase(root, [sys.executable, "-c", "print('x' * 10000)"],
                                      root / "phase.log", False)
            self.assertLessEqual(os.stat(root / "phase.log").st_size, 1024)


if __name__ == "__main__":
    unittest.main()
