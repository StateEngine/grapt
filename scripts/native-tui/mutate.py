"""Run a mutation and prove it actually happened.

A mutant that "survives" is a claim about the code. If the edit never applied,
the run says nothing at all — it is INVALID, not survived. That failure is not
hypothetical: two runs in this campaign reported survival while a restore in the
same command chain had already put the file back, and the conclusion drawn from
them would have deleted a guard that was doing real work.

So this refuses to report anything it cannot show:

1. **Exact-once target check.** The `old` text must occur EXACTLY ONCE in the
   file. Zero occurrences means the mutation did not target what it claimed;
   more than one means it is ambiguous about which site it changed. Either way
   the run stops before executing anything.
2. **Applied-diff proof, read back FROM DISK.** The digest recorded for the
   mutated file is re-read from the filesystem immediately before the witness
   runs — not computed from the bytes this process meant to write. What the
   witness saw is the only thing worth recording, and an in-memory digest
   asserts the write rather than observing it.
3. **The named witness must actually fail**, and its FULL output is preserved
   for both the mutant and the restoration, with the command, the exit status,
   and the log's own digest. A tail in a JSON field is a summary; a reader
   checking this needs the run.
4. **Restoration must return the file to its exact original digest, and the
   witness must pass again.** A mutation run that left the tree dirty or the
   suite red proves nothing about the mutant either.

Everything is done in BYTES. Reading a file as text and writing it back
translates newlines, and on a file with mixed endings — which this repo has,
because an editor writes LF into a CRLF file — the "restore" silently rewrites
every line that disagreed.

Usage:
    python mutate.py --spec <mutations.json> --out receipts.json --logs <dir>
    python mutate.py --file <path> --old <text> --new <text> --test <name>

A spec entry names `file`, `old`, `new` and either `test` (a cargo witness) or
`command` (an argv list, for a witness that is not a Rust test — the evidence
readers are Python, and their boundaries need proving too).
"""

from __future__ import annotations

import argparse
import difflib
import hashlib
import json
import subprocess
import sys
from pathlib import Path


class MutationError(RuntimeError):
    """A mutation run that cannot support a conclusion."""


def digest_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def digest_on_disk(path: Path) -> str:
    """Digest what is ACTUALLY on the filesystem, not what we meant to put there."""
    return digest_bytes(path.read_bytes())


def bind_source(cwd: Path) -> dict:
    """Pin the source once, at run start, and say whether it was already dirty."""
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], capture_output=True, text=True, cwd=str(cwd)
    )
    status = subprocess.run(
        ["git", "status", "--porcelain"], capture_output=True, text=True, cwd=str(cwd)
    )
    tracked_dirty = [
        line
        for line in status.stdout.splitlines()
        if line.strip() and not line.startswith("??")
    ]
    return {
        "head": head.stdout.strip() or None,
        "tracked_modifications_at_start": tracked_dirty,
        "clean_at_start": not tracked_dirty,
    }


def run_witness(
    command: list[str], cwd: Path, log_dir: Path, label: str, cargo: bool
) -> dict:
    """Run one witness, PRESERVE its whole output, and report how it exited.

    The full log is written to a file and hashed. Truncating to a tail in the
    receipt makes the receipt unauditable: a reader cannot tell a witness that
    failed for the intended reason from one that failed for another.
    """
    result = subprocess.run(
        command,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        cwd=str(cwd),
    )
    output = (result.stdout or "") + (result.stderr or "")

    log_dir.mkdir(parents=True, exist_ok=True)
    log_path = log_dir / f"{label}.log"
    log_path.write_text(output, encoding="utf-8", newline="")

    record = {
        "label": label,
        "command": command,
        "cwd": str(cwd),
        "exit_code": result.returncode,
        "passed": result.returncode == 0,
        "log_path": str(log_path),
        "log_sha256": digest_bytes(output.encode("utf-8")),
        "log_bytes": len(output.encode("utf-8")),
        "tail": output.strip()[-800:],
    }

    if cargo:
        # A filter that matches nothing EXITS ZERO. Read as a pass, that is the
        # same class of error this module exists to prevent: a green result
        # describing no execution.
        #
        # The signal is `running 0 tests` and only that. A genuinely FAILING
        # test prints `0 passed; 1 failed`, so keying on `0 passed` would turn
        # every real kill into a refusal.
        if "running 0 tests" in output:
            raise MutationError(
                f"the witness matched NO test, so neither its failure nor its "
                f"success means anything (log {log_path})"
            )
        if "running 1 test\n" not in output and "running 1 test\r\n" not in output:
            raise MutationError(
                f"the witness did not resolve to exactly one test; a filter "
                f"matching several cannot say which one reacted (log {log_path})"
            )
        # A build failure also exits non-zero and would otherwise read as a
        # kill — the mutant must be rejected by the WITNESS, not by rustc.
        if "error: could not compile" in output or "error[E" in output:
            raise MutationError(
                f"the mutated tree did not COMPILE, so the witness never ran; a "
                f"compile error is not a kill (log {log_path})"
            )
    return record


def anchor_bytes(text: str, original: bytes) -> tuple[bytes, str]:
    """Encode a spec anchor in the newline form the FILE actually uses.

    Spec anchors are written with `\\n`, so a multiline anchor matches nothing
    in a CRLF file. Both forms are tried and the receipt records which hit.
    """
    lf = text.encode("utf-8")
    if original.count(lf) > 0:
        return lf, "LF"
    crlf = text.replace("\n", "\r\n").encode("utf-8")
    if original.count(crlf) > 0:
        return crlf, "CRLF"
    return lf, "LF"


def mutate(spec: dict, package: str, cwd: Path, log_dir: Path, source: dict) -> dict:
    name = spec.get("name") or spec.get("test") or "unnamed"
    label = "".join(c if c.isalnum() else "-" for c in name)[:80]

    file = Path(spec["file"])
    resolved = (cwd / file).resolve()
    # The tested file must belong to the tree the source pin describes.
    if not str(resolved).startswith(str(cwd.resolve())):
        raise MutationError(
            f"{file} resolves outside the bound source tree {cwd}, so the pin "
            f"does not describe what was tested"
        )

    if spec.get("command"):
        command, cargo = list(spec["command"]), False
    else:
        command, cargo = (
            [
                "cargo",
                "test",
                "-p",
                spec.get("package", package),
                "--lib",
                spec["test"],
                "--",
                "--exact",
            ],
            True,
        )

    original = resolved.read_bytes()
    before_digest = digest_bytes(original)

    old_bytes, newline_form = anchor_bytes(spec["old"], original)
    new_text = spec.get("new", "")
    new_bytes = (
        new_text.replace("\n", "\r\n").encode("utf-8")
        if newline_form == "CRLF"
        else new_text.encode("utf-8")
    )

    # 1. EXACT-ONCE TARGET CHECK, before anything is executed.
    occurrences = original.count(old_bytes)
    if occurrences != 1:
        raise MutationError(
            f"the mutation target occurs {occurrences} times in {file.name} "
            f"(anchor matched as {newline_form}); it must occur exactly once, "
            f"or the run cannot say which site it changed"
        )

    mutated = original.replace(old_bytes, new_bytes, 1)
    if mutated == original:
        raise MutationError("the replacement produced a byte-identical file")

    try:
        resolved.write_bytes(mutated)
        # 2. APPLIED-DIFF PROOF, OBSERVED FROM DISK immediately before the
        #    witness runs. An in-memory digest asserts the write; this one
        #    reads back what the witness is actually about to compile.
        after_digest = digest_on_disk(resolved)
        if after_digest == before_digest:
            raise MutationError("the file digest did not change; the edit did not land")
        if after_digest != digest_bytes(mutated):
            raise MutationError(
                "the bytes on disk are not the bytes this run wrote; something "
                "else is touching the file and no result here is trustworthy"
            )

        diff = "\n".join(
            difflib.unified_diff(
                original.decode("utf-8", "replace").splitlines(),
                mutated.decode("utf-8", "replace").splitlines(),
                fromfile=f"{file.name} (original)",
                tofile=f"{file.name} (mutated)",
                lineterm="",
                n=1,
            )
        )

        # 3. THE WITNESS MUST FAIL.
        mutant_run = run_witness(command, cwd, log_dir, f"{label}--mutant", cargo)
        killed = not mutant_run["passed"]
    finally:
        # Byte-exact restoration. Anything else rewrites newlines.
        resolved.write_bytes(original)

    # 4. RESTORATION MUST BE EXACT, AND GREEN AGAIN.
    restored_digest = digest_on_disk(resolved)
    if restored_digest != before_digest:
        raise MutationError(
            "the file was not restored to its original bytes; every later result "
            "in this tree is now suspect"
        )
    restored_run = run_witness(command, cwd, log_dir, f"{label}--restored", cargo)
    if not restored_run["passed"]:
        raise MutationError(
            f"the witness does not pass on the RESTORED tree, so its failure "
            f"under the mutant proves nothing (log {restored_run['log_path']})"
        )

    return {
        "name": name,
        "file": str(file),
        "witness": spec.get("test") or " ".join(command),
        "source_pin": source["head"],
        "source_clean_at_start": source["clean_at_start"],
        "digest_before": before_digest,
        "digest_mutated_on_disk": after_digest,
        "digest_restored": restored_digest,
        "applied": True,
        "occurrences_of_target": occurrences,
        "anchor_newline_form": newline_form,
        "diff": diff,
        "mutant_run": mutant_run,
        "restored_run": restored_run,
        "killed": killed,
        "verdict": "KILLED" if killed else "SURVIVED",
        "note": spec.get("note"),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--file", type=Path)
    parser.add_argument("--old")
    parser.add_argument("--new", default="")
    parser.add_argument("--test")
    parser.add_argument("--package", default="grapt-tui")
    parser.add_argument("--spec", type=Path, help="a JSON list of mutations")
    parser.add_argument("--out", type=Path)
    parser.add_argument("--logs", type=Path, default=Path("mutation-logs"))
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--scope", default="", help="what this batch is FOR")
    arguments = parser.parse_args()

    if arguments.spec:
        specs = json.loads(arguments.spec.read_text(encoding="utf-8"))
    else:
        if not (arguments.file and arguments.old and arguments.test):
            print("mutate: --file, --old and --test are required", file=sys.stderr)
            return 2
        specs = [
            {
                "file": str(arguments.file),
                "old": arguments.old,
                "new": arguments.new,
                "test": arguments.test,
            }
        ]

    # Bind the source ONCE, at run start, so every receipt in the batch names
    # the same pin and a reader can see whether the tree was already dirty.
    source = bind_source(arguments.cwd)
    if not source["clean_at_start"]:
        print(
            "mutate: the tree has tracked modifications at start; receipts will "
            "say so and cannot be read as describing the pinned source",
            file=sys.stderr,
        )

    receipts = []
    failures = 0
    for spec in specs:
        name = spec.get("name") or spec.get("test") or "unnamed"
        try:
            receipt = mutate(spec, arguments.package, arguments.cwd, arguments.logs, source)
            receipts.append(receipt)
            print(f"  {receipt['verdict']:<9} {name}")
            if not receipt["killed"]:
                failures += 1
        except MutationError as error:
            receipts.append(
                {
                    "name": name,
                    "verdict": "INVALID",
                    "reason": str(error),
                    "source_pin": source["head"],
                }
            )
            print(f"  INVALID   {name}: {error}")
            failures += 1

    if arguments.out:
        arguments.out.write_text(
            json.dumps(
                {
                    "scope": arguments.scope,
                    "source": source,
                    "receipts": receipts,
                },
                indent=2,
            ),
            encoding="utf-8",
        )
        print(f"wrote {arguments.out}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
