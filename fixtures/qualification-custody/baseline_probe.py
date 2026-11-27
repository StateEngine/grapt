#!/usr/bin/env python3
"""Small, disposable probes of the PLR runner at accepted base 5391f018.

These expose the gaps J01 must close without running a product build. They also
record the guards that already reject ordinary dirty and wrong-diagnostic rows.
"""

import subprocess
import tempfile
from pathlib import Path
from types import ModuleType


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/qualification/plugin-linux-reap/run-mutations.py"
BASE = "5391f018b73aefd2d00227bf3d36a43b4823b9a2"
source = subprocess.run(
    ["git", "show", f"{BASE}:scripts/qualification/plugin-linux-reap/run-mutations.py"],
    cwd=ROOT, capture_output=True, text=True, check=True,
).stdout
runner = ModuleType("plr_baseline_runner")
runner.__file__ = str(RUNNER)
exec(compile(source, f"{BASE}:run-mutations.py", "exec"), runner.__dict__)


def git(root, *args):
    result = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=False
    )
    if result.returncode:
        raise RuntimeError(result.stderr)
    return result.stdout.strip()


def main():
    with tempfile.TemporaryDirectory(prefix="qcu-j01-") as directory:
        root = Path(directory)
        deps = root / "target/debug/deps"
        deps.mkdir(parents=True)
        (deps / "bridge-000good").write_bytes(b"right source, not executed")
        (deps / "bridge-999stale").write_bytes(b"stale executable, actually run")
        selected = Path(runner.bridge_test_binary(str(root / "target"))).name
        assert selected == "bridge-000good", selected
        print("STALE BINARY GAP: sorted-first inspection selects bridge-000good; no executed-path binding")

        repo = root / "source"
        repo.mkdir()
        git(repo, "init", "-q")
        source = repo / "CONTRACT.md"
        source.write_text("accepted\n", encoding="utf-8")
        git(repo, "add", "--", "CONTRACT.md")
        git(
            repo, "-c", "user.name=QCU", "-c", "user.email=qcu@example.invalid",
            "commit", "-q", "-m", "fixture", "--", "CONTRACT.md",
        )
        assert git(repo, "status", "--porcelain") == ""
        source.write_text("changed during execution\n", encoding="utf-8")
        assert git(repo, "status", "--porcelain") != ""
        print("DIRTY GUARD WORKS: a persistent changed candidate is visible to the existing status check")
        source.write_text("accepted\n", encoding="utf-8")
        assert git(repo, "status", "--porcelain") == ""
        print("MID-RUN GAP: clean before/after snapshots miss an edit restored during the run")

        wrong = runner.failure_detail(
            "running 1 test\nthread 'witness' panicked at source.rs:1\n"
            "unrelated assertion fired\ntest witness ... FAILED\n"
            "test result: FAILED. 0 passed; 1 failed\n"
        )
        assert "intended diagnostic" not in wrong
        print("WRONG-DIAGNOSTIC GUARD WORKS: unrelated panic does not match expected_diagnostic")

        assert not (root / "receipt.json").exists()
        print("ABSENT RECEIPT GAP: the legacy stdout-only probe has no durable receipt to verify")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
