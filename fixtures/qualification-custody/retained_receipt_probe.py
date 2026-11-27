#!/usr/bin/env python3
"""Recheck the retained PLR101 receipt and the review's stale-path counterexample."""

import copy
import os
import hashlib
import importlib.util
import json
import subprocess
from pathlib import Path


REPO = Path(__file__).resolve().parents[2]
MODULE = REPO / "scripts/qualification/custody/receipt.py"
RECEIPT_SHA256 = "976131354ca22b9842e4d6bfec678d13abf252e061ed9b0e78d6a92174cbb88d"
EXECUTED_COMMIT = "5b4038a632269283752d821a3cbd0de84ecbb8eb"
MANIFEST = "scripts/qualification/plugin-linux-reap/mutation-manifest.json"


def executed_manifest(custody):
    """The PLR manifest bytes at the executed commit, the ones its harness pinned."""
    encoded = subprocess.run(
        ["git", "show", f"{EXECUTED_COMMIT}:{MANIFEST}"],
        cwd=REPO, capture_output=True, check=True,
    ).stdout
    return custody.load_manifest(encoded)


def main():
    encoded = Path(os.environ["GRAPT_RETAINED_CUSTODY_RECEIPT"]).read_bytes()  # Missing evidence must fail loudly.
    if hashlib.sha256(encoded).hexdigest() != RECEIPT_SHA256:
        raise RuntimeError("retained PLR101 receipt digest differs")
    receipt = json.loads(encoded)
    if receipt["candidate"]["after"]["commit"] != EXECUTED_COMMIT:
        raise RuntimeError("retained PLR101 candidate differs")
    spec = importlib.util.spec_from_file_location("qcu_corrected_receipt", MODULE)
    custody = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(custody)
    manifest = executed_manifest(custody)
    custody.verify_receipt(receipt, ["PLR101"], manifest)
    print("retained PLR101 receipt: accepted under corrected validator")

    for phase in ("red", "green"):
        stale = copy.deepcopy(receipt)
        entry = stale["entries"][0]
        artifact = entry["artifact"] if phase == "red" else entry["green"]["artifact"]
        command = entry["red"]["command"] if phase == "red" else entry["green"]["command"]
        artifact["path"] = "/tmp/stale-secode-plugin-binary"
        command[0] = artifact["path"]
        try:
            custody.verify_receipt(stale, ["PLR101"], manifest)
        except custody.CustodyError as error:
            if "compiler row executable differs" not in str(error):
                raise RuntimeError(f"{phase} refused for wrong reason: {error}") from error
        else:
            raise RuntimeError(f"{phase} stale binary receipt was accepted")
        print(f"retained PLR101 {phase} stale path: refused by Cargo row binding")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
