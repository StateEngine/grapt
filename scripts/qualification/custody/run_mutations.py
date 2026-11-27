#!/usr/bin/env python3
"""Prove QCU guards red under exact resurrection and green when restored."""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


HERE = Path(__file__).resolve().parent
SOURCE = HERE / "receipt.py"
TEST = HERE / "test_custody.py"
MANIFEST = HERE / "mutation-manifest.json"


def run_test(test_name, module_path):
    env = dict(os.environ)
    env["QCU_MODULE_PATH"] = str(module_path)
    return subprocess.run(
        [sys.executable, str(TEST), f"CustodyControls.{test_name}"],
        cwd=HERE, env=env, capture_output=True, text=True, check=False,
    )


def main():
    source = SOURCE.read_text(encoding="utf-8")
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    rows = manifest["mutations"]
    if manifest["prefix"] != "QCU" or not rows:
        raise RuntimeError("QCU manifest is empty or names another prefix")
    ids = [row["id"] for row in rows]
    if len(ids) != len(set(ids)) or any(not item.startswith("QCU") for item in ids):
        raise RuntimeError("QCU mutation ids are duplicate or foreign")
    control_env = dict(os.environ)
    control_env["QCU_MODULE_PATH"] = str(SOURCE)
    controls = subprocess.run(
        [sys.executable, str(TEST)], cwd=HERE, env=control_env,
        capture_output=True, text=True, check=False,
    )
    if controls.returncode:
        raise RuntimeError(f"QCU control suite failed: {controls.stderr[-500:]}")
    counts = [line for line in controls.stderr.splitlines()
              if line.startswith("Ran ") or line.startswith("OK")]
    if len(counts) != 2:
        raise RuntimeError("QCU control suite emitted no count and success witness")
    print(f"QCU controls: {counts[0]}; {counts[1]}")
    for row in rows:
        count = source.count(row["old"])
        if count != 1 or row["old"] == row["new"]:
            raise RuntimeError(f"{row['id']}: mutation target count {count}, expected exactly 1")
        baseline = run_test(row["test"], SOURCE)
        if baseline.returncode:
            raise RuntimeError(f"{row['id']}: baseline not green: {baseline.stderr[-300:]}")
        with tempfile.TemporaryDirectory(prefix=f"qcu-{row['id']}-") as directory:
            mutant = Path(directory) / "receipt.py"
            mutant.write_text(source.replace(row["old"], row["new"], 1), encoding="utf-8")
            red = run_test(row["test"], mutant)
            if red.returncode == 0 or row["expected_diagnostic"] not in red.stderr:
                raise RuntimeError(
                    f"{row['id']}: expected named RED, got exit {red.returncode}: "
                    f"{red.stderr[-300:]}"
                )
        restored = run_test(row["test"], SOURCE)
        if restored.returncode:
            raise RuntimeError(f"{row['id']}: restored-green failed: {restored.stderr[-300:]}")
        print(f"{row['id']}: intended RED {row['expected_diagnostic']!r}; restored GREEN")
    print(f"QCU {len(rows)}/{len(rows)} intended RED and restored GREEN")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
