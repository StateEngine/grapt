"""Focused DUB producer-bound mutants in the exclusively owned author tree."""
import argparse
import hashlib
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "crates/grapt-platform/src/model_inventory.rs"
ROWS = [
    ("DUB-CB01", "        check_account_count(accounts.len())?;\n        let public = read",
     "        let public = read", "account_count_is_refused_before_opening_any_snapshot"),
    ("DUB-CB02", "            if available.len() >= MAX_ISSUABLE_RECORDS {",
     "            if false {", "model_admission_refuses_before_retaining_an_excess_row"),
    ("DUB-CB03", "                .filter(|count| *count <= MAX_ISSUABLE_RECORDS)",
     "                .filter(|_| true)", "variant_expansion_counts_against_the_record_bound"),
]


def run(log, test):
    command = ["cargo", "test", "-p", "grapt-platform", "--lib", test,
               "--no-fail-fast", "-j", "2"]
    with log.open("wb") as output:
        result = subprocess.run(command, cwd=ROOT, stdout=output,
                                stderr=subprocess.STDOUT, timeout=180)
    return result.returncode, log.read_text(encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    original = SOURCE.read_bytes()
    source = original.decode("utf-8").replace("\r\n", "\n")
    for name, anchor, _, _ in ROWS:
        if source.count(anchor) != 1:
            raise RuntimeError(f"{name}: expected exactly one source anchor")
    receipt = [f"source_sha256={hashlib.sha256(original).hexdigest()}"]
    try:
        for name, anchor, replacement, test in ROWS:
            SOURCE.write_text(source.replace(anchor, replacement), encoding="utf-8", newline="\n")
            code, log = run(args.output / f"{name}-red.log", test)
            witness = f"model_inventory::bounds_tests::{test} ... FAILED"
            if code != 101 or witness not in log:
                raise RuntimeError(f"{name}: did not fail its named assertion; exit={code}")
            receipt.append(f"{name}: RED, named witness {test}")
    finally:
        SOURCE.write_bytes(original)
        if SOURCE.read_bytes() != original:
            raise RuntimeError("source restoration failed")
    code, log = run(args.output / "restored-green.log", "model_inventory")
    if code != 0 or "8 passed; 0 failed" not in log:
        raise RuntimeError("restored model inventory suite did not pass all eight tests")
    receipt.append("restored: GREEN, 8 passed; original source bytes restored")
    (args.output / "receipt.txt").write_text("\n".join(receipt) + "\n", encoding="utf-8")
    print("\n".join(receipt))


if __name__ == "__main__":
    main()
