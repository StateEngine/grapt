#!/usr/bin/env python3
"""grapt-event mutation harness.

Guards, in order, none skippable:
  1. The manifest's SHA-256 (CRLF-canonicalized) must equal the pin below,
     verified BEFORE the JSON is parsed or any worktree exists — frozen ids
     freeze labels, not laws, so the pin covers the WHOLE manifest.
  2. The candidate tree must be clean and every targeted source file's digest
     is recorded before the run and re-verified after: the candidate is
     never mutated, only disposable detached worktrees are.
  3. Each mutation's `old` snippet must occur EXACTLY once in its file — zero
     is a dead pattern, two is ambiguous attribution; both refuse.
  4. Each declared witness must actually RUN (>= 1 test parsed from the
     summary line — an empty cargo filter exits 0 and would report a fake
     red) and must FAIL under the mutant, and the harness records the panic
     MESSAGE (not only the location) as the mutation's evidence detail.
  5. Two mutations may not produce byte-identical (witness, detail) evidence
     — a duplicate means one law witnessed twice, not two laws witnessed.
"""

import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

CANONICAL_MANIFEST_SHA256 = "3c030c84ada79879b610bd264a68df4739baa25bc121f9bc517169672581f094"

ROOT = Path(__file__).resolve().parents[3]
MANIFEST = Path(__file__).resolve().parent / "manifest.json"


def run(args, cwd, check=True):
    proc = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if check and proc.returncode != 0:
        raise SystemExit(f"command failed ({args}): {proc.stderr[-2000:]}")
    return proc


def sha256_canonical(path: Path) -> str:
    data = path.read_bytes().replace(b"\r\n", b"\n")
    return hashlib.sha256(data).hexdigest()


def main() -> None:
    # Guard 1: manifest pin, before parse.
    actual = sha256_canonical(MANIFEST)
    if actual != CANONICAL_MANIFEST_SHA256:
        raise SystemExit(
            f"manifest pin mismatch: {actual} != {CANONICAL_MANIFEST_SHA256}; "
            "re-pin deliberately after reviewing the manifest edit"
        )
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    mutations = manifest["mutations"]

    # Guard 2a: clean candidate.
    status = run(["git", "status", "--porcelain"], ROOT).stdout.strip()
    if status:
        raise SystemExit(f"candidate worktree is not clean:\n{status}")
    head = run(["git", "rev-parse", "HEAD"], ROOT).stdout.strip()
    targeted = sorted({m["file"] for m in mutations})
    before = {f: sha256_canonical(ROOT / f) for f in targeted}

    # Guard 3: exactly-one occurrence, on the candidate bytes.
    for m in mutations:
        text = (ROOT / m["file"]).read_text(encoding="utf-8")
        count = text.count(m["old"])
        if count != 1:
            raise SystemExit(
                f"{m['id']}: snippet occurs {count} times in {m['file']} "
                "(need exactly 1 — dead or ambiguous pattern)"
            )

    evidence = {}
    for m in mutations:
        with tempfile.TemporaryDirectory(prefix=f"f2a-mut-{m['id']}-") as tmp:
            wt = Path(tmp) / "wt"
            run(["git", "worktree", "add", "--detach", str(wt), head], ROOT)
            try:
                target = wt / m["file"]
                text = target.read_text(encoding="utf-8")
                target.write_text(
                    text.replace(m["old"], m["new"], 1), encoding="utf-8", newline="\n"
                )
                proc = run(
                    ["cargo", "test", "-p", "grapt-event", "--lib", m["witness"]],
                    wt,
                    check=False,
                )
                out = proc.stdout + proc.stderr
                summary = re.search(
                    r"test result: (\w+)\. (\d+) passed; (\d+) failed", out
                )
                if not summary:
                    raise SystemExit(f"{m['id']}: no test summary parsed")
                ran = int(summary.group(2)) + int(summary.group(3))
                # Guard 4: the filter must have run at least one test.
                if ran == 0:
                    raise SystemExit(
                        f"{m['id']}: witness filter '{m['witness']}' ran ZERO "
                        "tests — an empty filter exits 0 and proves nothing"
                    )
                if int(summary.group(3)) == 0:
                    raise SystemExit(
                        f"{m['id']}: witness '{m['witness']}' stayed GREEN "
                        "under the mutant — the law is not witnessed"
                    )
                detail = ""
                message = re.search(r"panicked at [^\n]*\n([^\n]*)", out)
                if message:
                    detail = message.group(1).strip()
                if not detail:
                    detail = f"assertion in {m['witness']} failed"
                evidence[m["id"]] = (m["witness"], detail)
                print(f"{m['id']}: RED — {m['witness']}: {detail[:120]}")
            finally:
                run(["git", "worktree", "remove", "--force", str(wt)], ROOT, check=False)

    # Guard 5: unique (witness, detail) evidence.
    seen = {}
    for mid, pair in evidence.items():
        if pair in seen:
            raise SystemExit(
                f"duplicate evidence: {mid} and {seen[pair]} both produced "
                f"{pair!r} — one law witnessed twice is not two laws"
            )
        seen[pair] = mid

    # Guard 2b: candidate untouched.
    for f, digest in before.items():
        if sha256_canonical(ROOT / f) != digest:
            raise SystemExit(f"candidate file {f} changed during the run")
    if run(["git", "rev-parse", "HEAD"], ROOT).stdout.strip() != head:
        raise SystemExit("candidate HEAD moved during the run")
    if run(["git", "status", "--porcelain"], ROOT).stdout.strip():
        raise SystemExit("candidate worktree dirty after the run")

    print(f"mutation harness passed: {len(mutations)}/{len(mutations)} RED, "
          "evidence unique, candidate unchanged")


if __name__ == "__main__":
    main()
