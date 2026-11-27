#!/usr/bin/env python3
"""Re-runs the R3b-2A mutation witnesses for `grapt-mail-transport`.

Each entry in `manifest.json` names one load-bearing behaviour, the exact source
edit that removes it, and the witness that must turn red once it is gone. A
witness that stays green under its own mutation attributes nothing, and the ten
contract vectors passing on their first run proved exactly nothing until this
runner showed each of them failing for its own named reason.

No mutant is ever written into the calling worktree. Every mutation is applied
inside its own disposable `git worktree` at the exact candidate commit, so no
restore step can leave a stale build behind.

# The RM allocation, and why this runner is stricter than the one it learned from

The whole token `RM` is allocated to this lane; the allocation refused
`Q`, which the live R3c1 candidate already owns. The ruling requires the guards
here to match ONLY the full token `RM[0-9]+`, to fail closed on a malformed or
unreadable manifest, and to refuse an empty or cross-token selection. So there
are two distinct guards, not one:

* the ALLOCATION guard checks every row in this manifest is an `RM` row. A
  runner that only filtered on request would happily execute a row belonging to
  another lane's prefix if one were ever pasted in;
* the SELECTION guard checks `--prefix` names this lane's token exactly. Passing
  another lane's prefix here is refused rather than silently matching nothing.

That second refusal matters because an empty filter is the classic silent pass:
`cargo test` with a filter matching no test exits ZERO, so an empty selection
scored by exit code alone reads as a clean run. Both an empty selection and a
zero-test witness are infrastructure failures here, never passes.

This runner was adapted from `crates/grapt-store/mutations/run.py`, whose
`--prefix` accepts any uppercase token because it hosts many inherited lanes.
Copying it unchanged would have left this lane's guard matching prefixes it does
not own -- a recorded defect in this workspace, and the reason both guards below
name `RM` explicitly rather than accepting whatever is passed.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "manifest.json")
PACKAGE = "grapt-mail-transport"
LANE_PREFIX = "RM"
WHOLE_TOKEN = re.compile(rf"{LANE_PREFIX}[0-9]+")
REQUIRED_KEYS = ("id", "file", "old", "new", "test", "law")


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def evidence_key(test, detail):
    """The (test, detail) pair with the process id removed.

    Rust prints `thread '...' (12345) panicked at ...`. The pid differs on every
    process, so two rows whose evidence is otherwise byte-identical never
    collided and the duplicate guard below passed while witnessing one law
    twice. That is exactly what RM24 and RM25 did: one trybuild message, two
    seal laws, two pids, no complaint.

    The pattern requires the literal ` panicked` after the parenthesised number,
    so it normalises the recognised thread/pid prefix and nothing else. An
    arbitrary parenthesised number elsewhere in a diagnostic -- a count, an
    index, a byte offset -- is substantive evidence and is left alone.
    """
    return test, re.sub(r"\((\d+)\) panicked", "(pid) panicked", detail)


def failure_detail(output):
    """The line that says WHICH assertion fired, not merely where it lives."""
    lines = output.splitlines()
    for index, line in enumerate(lines):
        if "panicked at" in line:
            location = line.strip()
            message = lines[index + 1].strip() if index + 1 < len(lines) else ""
            return f"{location} {message}".strip()
    return next(
        (line.strip() for line in lines if "assertion" in line),
        "failed without a captured diagnostic",
    )


def ran_at_least_one_test(output):
    """A cargo filter matching NO test exits 0 with `0 passed; 0 failed`."""
    total = 0
    for match in re.finditer(r"(\d+) passed; (\d+) failed", output):
        total += int(match.group(1)) + int(match.group(2))
    return total > 0


def load_manifest():
    """Reads the manifest, failing CLOSED on anything malformed.

    A manifest that cannot be read or does not have the shape this runner
    expects must stop the gate. The alternative -- skipping what could not be
    parsed -- would report a green run over a manifest nobody could read.
    """
    try:
        with open(MANIFEST, "r", encoding="utf-8") as handle:
            manifest = json.load(handle)
    except OSError as error:
        return None, [f"cannot read {MANIFEST}: {error}"]
    except json.JSONDecodeError as error:
        return None, [f"{MANIFEST} is not valid JSON: {error}"]

    if not isinstance(manifest, dict) or not isinstance(manifest.get("mutations"), list):
        return None, ["manifest has no `mutations` array"]

    mutations = manifest["mutations"]
    if not mutations:
        return None, ["manifest `mutations` array is empty"]

    problems = []
    seen = set()
    for index, entry in enumerate(mutations):
        if not isinstance(entry, dict):
            problems.append(f"row {index} is not an object")
            continue
        missing = [key for key in REQUIRED_KEYS if key not in entry]
        if missing:
            problems.append(f"row {index} is missing {', '.join(missing)}")
            continue
        # The ALLOCATION guard: every row must belong to this lane's token.
        if WHOLE_TOKEN.fullmatch(entry["id"]) is None:
            problems.append(
                f"{entry['id']}: not a whole-token {LANE_PREFIX} id "
                f"(expected {LANE_PREFIX} followed by digits)"
            )
        if entry["id"] in seen:
            problems.append(f"{entry['id']}: duplicate id")
        seen.add(entry["id"])
    return mutations, problems


def preflight(mutations, repo):
    """Every snippet present exactly once, and actually a change."""
    problems = []
    cache = {}
    for entry in mutations:
        path = os.path.join(repo, entry["file"].replace("/", os.sep))
        if path not in cache:
            try:
                with open(path, "r", encoding="utf-8", newline="") as handle:
                    cache[path] = handle.read().replace("\r\n", "\n")
            except OSError as error:
                cache[path] = None
                problems.append(f"{entry['id']}: cannot read {entry['file']}: {error}")
        source = cache[path]
        if source is None:
            continue
        found = source.count(entry["old"])
        if found == 0:
            problems.append(
                f"{entry['id']}: SNIPPET NOT FOUND in {entry['file']} "
                f"-- {entry['old'][:60]!r}"
            )
        elif found > 1:
            problems.append(
                f"{entry['id']}: SNIPPET AMBIGUOUS ({found} matches) in {entry['file']} "
                f"-- {entry['old'][:60]!r}"
            )
        if entry["old"] == entry["new"]:
            problems.append(f"{entry['id']}: the mutation changes nothing")
    return problems


def run_one(entry, repo, candidate, run_root):
    workspace = os.path.join(run_root, entry["id"])
    git("worktree", "add", "--detach", "--quiet", workspace, candidate, cwd=repo)
    try:
        target = os.path.join(workspace, entry["file"].replace("/", os.sep))
        with open(target, "r", encoding="utf-8", newline="") as handle:
            source = handle.read()
        # `core.autocrlf=true` checks these sources out with CRLF while the
        # manifest snippets are LF. Matching raw bytes would report SNIPPET NOT
        # FOUND for every entry and look like an infrastructure failure rather
        # than the newline mismatch it is.
        source = source.replace("\r\n", "\n")
        if entry["old"] not in source:
            return False, "SNIPPET NOT FOUND", entry["old"][:60]
        if source.count(entry["old"]) != 1:
            return False, "SNIPPET AMBIGUOUS", entry["old"][:60]
        with open(target, "w", encoding="utf-8", newline="") as handle:
            handle.write(source.replace(entry["old"], entry["new"], 1))

        command = ["cargo", "test", "-p", entry.get("package", PACKAGE)]
        if "test_target" in entry:
            command += ["--test", entry["test_target"]]
        else:
            command += ["--lib"]
        command += ["--", "--exact", entry["test"]]

        result = subprocess.run(
            command, cwd=workspace, capture_output=True, text=True, check=False
        )
        output = result.stdout + result.stderr

        if "error[" in output or "error:" in output and "test failed" not in output:
            if result.returncode != 0 and not ran_at_least_one_test(output):
                return False, "COMPILE KILL (not a witness)", output.strip()[-200:]
        if not ran_at_least_one_test(output):
            return False, "NO TEST RAN", "the filter matched nothing"
        if result.returncode == 0:
            return (
                False,
                "GREEN -- BROKEN WITNESS",
                "the witness did not notice the mutation",
            )
        return True, "RED (witness fired)", failure_detail(output)
    finally:
        git("worktree", "remove", "--force", workspace, cwd=repo, check=False)


def parse_args():
    parser = argparse.ArgumentParser(
        description=(
            f"Run every {LANE_PREFIX} mutation for {PACKAGE}, or select this "
            f"lane's whole prefix explicitly."
        )
    )
    parser.add_argument(
        "--prefix",
        help=(
            f"must be exactly {LANE_PREFIX}; any other token is refused rather "
            f"than silently matching nothing"
        ),
    )
    return parser.parse_args()


def main():
    args = parse_args()
    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: the candidate worktree is not clean")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)

    mutations, malformed = load_manifest()
    if malformed:
        print(f"GATE FAILED: {len(malformed)} manifest problem(s):")
        for problem in malformed:
            print(f"  {problem}")
        return 1

    if args.prefix is not None:
        # The SELECTION guard. Refusing a cross-token prefix outright is the
        # point: silently matching nothing would exit zero and read as a pass.
        if args.prefix != LANE_PREFIX:
            print(
                f"GATE FAILED: --prefix must be exactly {LANE_PREFIX}; "
                f"{args.prefix!r} belongs to another lane's allocation"
            )
            return 1
        selected = [
            entry for entry in mutations if WHOLE_TOKEN.fullmatch(entry["id"]) is not None
        ]
        if not selected:
            print(f"GATE FAILED: whole prefix {LANE_PREFIX} has no manifest rows")
            return 1
        mutations = selected
        print(f"selection: whole prefix {LANE_PREFIX}")

    print(f"candidate {candidate}")

    rotted = preflight(mutations, repo)
    if rotted:
        print(f"\nPREFLIGHT FAILED: {len(rotted)} entry/entries do not match the candidate:")
        for problem in rotted:
            print(f"  {problem}")
        print(
            "\nA rotted entry witnesses nothing. Re-pin it against the current "
            "source rather than deleting it."
        )
        return 1
    print(f"preflight: all {len(mutations)} snippets present exactly once")
    print(f"{len(mutations)} mutations, each in its own disposable worktree\n")

    run_root = tempfile.mkdtemp(prefix=f"grapt-mail-transport-mutations-{os.getpid()}-")
    unaccounted = []
    evidence = {}
    try:
        for entry in mutations:
            accounted, label, detail = run_one(entry, repo, candidate, run_root)
            print(f"  {entry['id']}  {label}")
            if detail:
                print(f"      {detail}")
            if not accounted:
                unaccounted.append(f"{entry['id']}: {label}")
            else:
                key = evidence_key(entry["test"], detail)
                if key in evidence:
                    unaccounted.append(
                        f"{entry['id']}: DUPLICATE EVIDENCE with {evidence[key]} "
                        f"-- two laws cannot share one record"
                    )
                evidence[key] = entry["id"]
    finally:
        shutil.rmtree(run_root, ignore_errors=True)
        git("worktree", "prune", cwd=repo, check=False)

    print()
    if git("status", "--porcelain", cwd=repo) or git("rev-parse", "HEAD", cwd=repo) != candidate:
        print("GATE FAILED: the candidate worktree changed during the run")
        return 1
    print("candidate verified unchanged after the run")

    if unaccounted:
        print(f"\n{len(unaccounted)} unaccounted mutation(s):")
        for failure in unaccounted:
            print(f"  {failure}")
        return 1
    print(f"all {len(mutations)} mutations accounted for, every one RED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
