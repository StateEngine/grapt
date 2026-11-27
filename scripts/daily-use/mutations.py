"""RESURRECTION BATTERY. Remove each fix; the battery must go RED.

A passing test suite says nothing about whether the tests test anything. This
puts each defect back, one at a time, by exact source substitution, runs
`test_provenance.py`, and requires a FAILURE. A row whose mutation still passes
is a survivor: the code it names is unwitnessed, and the row says so by name
rather than being quietly dropped.

RUN: python scripts/daily-use/mutations.py
"""

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent

#: (id, file, exact original, replacement, what the resurrected defect is)
ROWS = [
    (
        "HP01",
        "assemble_candidate.py",
        '    attested_sha = required_sha256(receipt, receipt_path, "host", "sha256")\n'
        '    attested_bytes = required_bytes(receipt, receipt_path, "host", "bytes")',
        '    attested_sha = receipt.get("sha256") or digest(source)[0]\n'
        '    attested_bytes = receipt.get("bytes") or digest(source)[1]',
        "THE ORIGINAL DEFECT: read a top-level key the governed receipt does "
        "not write, and fall back to the artifact itself, so the receipt "
        "verifies nothing",
    ),
    (
        "HP02",
        "assemble_candidate.py",
        '    source_commit = required_commit(receipt, receipt_path, "source_commit")',
        '    source_commit = receipt.get("source_commit")',
        "a receipt with no source identity writes null into the manifest",
    ),
    (
        "HP03",
        "assemble_candidate.py",
        "    if attested_sha not in approved:\n"
        "        raise ProvenanceError(\n"
        '            f"the host receipt attests {attested_sha}, which is not an approved host identity"\n'
        "        )",
        "    del approved",
        "any internally consistent artifact is reusable as the host",
    ),
    (
        "HP04",
        "assemble_candidate.py",
        '    must_match_artifact("host copy", receipt_path, attested_sha, attested_bytes, observed_copy, copied)',
        "    pass",
        "a truncated or raced copy is packaged under the source's receipt",
    ),
    (
        "HP05",
        "assemble_candidate.py",
        '    must_match_artifact("host source", receipt_path, attested_sha, attested_bytes, observed_source, source)',
        "    pass",
        "the receipt need not describe the artifact we were pointed at",
    ),
    (
        "MM01",
        "receipts.py",
        "    if receipt_bytes != observed_bytes:",
        "    if False:",
        "length is not compared, only the digest",
    ),
    (
        "MM02",
        "receipts.py",
        "    if receipt_sha != observed_sha:",
        "    if False:",
        "the digest is not compared at all",
    ),
    (
        "SC01",
        "receipts.py",
        "    if not isinstance(value, str) or not _SHA256.match(value):",
        "    if not isinstance(value, str):",
        "a short or non-hex string passes as a sha256",
    ),
    (
        "SC02",
        "receipts.py",
        "    if isinstance(value, bool) or not isinstance(value, int) or value < 0:",
        "    if not isinstance(value, int) or value < 0:",
        "True passes as a one-byte length",
    ),
    (
        "FD01",
        "receipts.py",
        "        if not isinstance(current, dict) or key not in current:\n"
        "            raise ProvenanceError(f\"{path} has no {'.'.join(walked)}\")",
        "        if not isinstance(current, dict) or key not in current:\n"
        "            return None",
        "a missing required field becomes None for a later comparison to accept",
    ),
    (
        "LR01",
        "receipts.py",
        "    if not isinstance(parsed, dict):\n"
        '        raise ProvenanceError(f"{path} is a {type(parsed).__name__}, not a receipt object")',
        "    if not isinstance(parsed, dict):\n        parsed = {}",
        "a non-object receipt becomes an empty dict whose every field is absent",
    ),
    (
        "BD01",
        "receipts.py",
        "    declared = _bounded_size(path, bound)\n"
        "    hasher = hashlib.sha256()",
        "    payload = path.read_bytes()\n"
        "    if len(payload) > bound:\n"
        f"        raise ProvenanceError('over bound')\n"
        "    declared = len(payload)\n"
        "    hasher = hashlib.sha256()",
        "THE ORIGINAL BOUNDS DEFECT: the whole file is allocated, and the "
        "bound is checked afterwards",
    ),
    (
        "BD02",
        "receipts.py",
        "    payload = read_bounded(path, MAX_RECEIPT_BYTES)",
        "    payload = path.read_bytes()",
        "THE DEFECT ALPHA EXECUTED: stat is checked, then the read is "
        "unbounded, so a file that grows after its size is checked is accepted",
    ),
    (
        "BD03",
        "receipts.py",
        "    if len(payload) > bound:",
        "    if False:",
        "the bytes actually read are never measured against the bound",
    ),
    (
        "BD04",
        "receipts.py",
        "        payload = handle.read(bound + 1)",
        "        payload = handle.read()",
        "the read itself is unbounded, so an oversized file is fully allocated",
    ),
    (
        "OF01",
        "receipts.py",
        "    _ordinary_file(path)\n    size = path.stat().st_size",
        "    size = path.stat().st_size",
        "links, reparse points and directories are accepted as receipts",
    ),
    (
        "BP01",
        "assemble_candidate.py",
        """    if attested_commit != head:
        raise ProvenanceError(
            f"the build receipt attests {attested_commit}, the candidate checkout is {head}"
        )""",
        "    del head",
        "THE ORIGINAL DEFECT: the build claim is unbound to the candidate "
        "commit, as it was when the assembler composed the string itself",
    ),
    (
        "BP02",
        "assemble_candidate.py",
        '    if receipt.get("tree_dirty") is not False:',
        '    if receipt.get("tree_dirty") is True:',
        "an ABSENT tree state reads as clean",
    ),
    (
        "BP03",
        "assemble_candidate.py",
        '    if required_text(receipt, receipt_path, "profile") != profile:',
        "    if False:",
        "bytes attested for one profile are packaged as another",
    ),
    (
        "BP04",
        "assemble_candidate.py",
        '    if required_text(receipt, receipt_path, "target") != target:',
        "    if False:",
        "bytes attested for one target are packaged as another",
    ),
    (
        "BP05",
        "assemble_candidate.py",
        "        must_match_artifact(name, receipt_path, attested_sha, attested_bytes, observed, path)",
        "        pass",
        "the packaged bytes need not be the attested bytes",
    ),
    (
        "BP06",
        "assemble_candidate.py",
        "        if not produced:",
        "        if False:",
        "no recorded command need account for a packaged binary",
    ),
    (
        "BP07",
        "assemble_candidate.py",
        "    if not isinstance(commands, list) or not commands:",
        "    if False:",
        "a receipt with no commands at all is accepted",
    ),
    (
        "BP08",
        "assemble_candidate.py",
        "    if isinstance(produces, list):",
        "    if False:",
        "a single invocation producing several binaries accounts for none of "
        "them, making the check unsatisfiable against the governed evidence",
    ),
    (
        "BP09",
        "assemble_candidate.py",
        "        return any(isinstance(entry, str) and entry == name for entry in produces)",
        "        return len(produces) > 0",
        "any non-empty produces list accounts for every binary",
    ),
]


def run(row) -> tuple[str, bool, str]:
    row_id, filename, original, replacement, _ = row
    with tempfile.TemporaryDirectory() as temp:
        work = Path(temp) / "daily-use"
        shutil.copytree(HERE, work)
        target = work / filename
        source = target.read_text(encoding="utf-8")
        if source.count(original) != 1:
            return row_id, False, f"ANCHOR NOT UNIQUE ({source.count(original)} matches)"
        target.write_text(source.replace(original, replacement, 1), encoding="utf-8")
        completed = subprocess.run(
            [sys.executable, str(work / "test_provenance.py")],
            capture_output=True,
            text=True,
        )
        if completed.returncode == 0:
            return row_id, False, "SURVIVOR: the battery still passed"
        tail = (completed.stderr or completed.stdout).strip().splitlines()
        return row_id, True, tail[-1] if tail else "failed"


def main() -> int:
    baseline = subprocess.run(
        [sys.executable, str(HERE / "test_provenance.py")], capture_output=True, text=True
    )
    if baseline.returncode != 0:
        print("BASELINE IS NOT GREEN; a mutation battery over a red baseline proves nothing")
        print(baseline.stderr)
        return 1
    print("baseline GREEN\n")

    survivors = []
    for row in ROWS:
        row_id, red, detail = run(row)
        print(f"{row_id} {'RED  ' if red else 'ALIVE'} {row[4]}")
        if not red:
            survivors.append((row_id, detail))
            print(f"       -> {detail}")
    print()
    print(f"{len(ROWS) - len(survivors)}/{len(ROWS)} RED, {len(survivors)} survivors")
    return 1 if survivors else 0


if __name__ == "__main__":
    raise SystemExit(main())
