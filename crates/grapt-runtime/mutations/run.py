#!/usr/bin/env python3
"""Re-runs the R2A mutation witnesses for `grapt-runtime` in disposable isolation.

Each entry in `manifest.json` names one load-bearing behaviour, the exact source
edit that removes it, and the witness that must turn red once it is gone. A
witness that stays green under its own mutation attributes nothing.

**No mutant is ever written into the calling worktree.** Every mutation is
applied inside its own disposable `git worktree` checked out at the exact
candidate commit, so there is no restore step that could leave a stale build
behind — the trap where an mtime-preserving restore makes cargo reuse the
previous mutant's artifacts cannot arise here.

Three properties this runner enforces that a bare "did it go red" loop does not:

* an empty test filter EXITS ZERO, so a typo in a witness name would score a
  missing test as a passing witness. Every run must report at least one test.
* two mutations whose evidence is byte-identical read as one law witnessed twice
  instead of two laws witnessed once, so the run fails on duplicate
  (witness, detail) evidence.
* every snippet is checked for presence and uniqueness BEFORE any worktree is
  built. An edit elsewhere in the tree can move the text an existing entry pins
  — F3B2 inserted a table into the governed array and a column check into the
  schema, which silently rotted five F3A/F3B1 entries — and finding that out one
  compile at a time costs a full run to learn something a string search answers
  immediately. The preflight reports every rotted entry at once.
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


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def failure_detail(output):
    """The line that says WHICH assertion fired, not merely where it lives.

    Rust prints the location first and the message second. Taking only the
    location collapses every case of a table-driven witness into one record.
    """
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
    """A cargo filter matching NO test exits 0 with `0 passed; 0 failed`.

    Scoring by exit code alone would read that as a green witness, so the count
    is parsed and a zero-test run is an infrastructure failure, not a pass.
    """
    total = 0
    for match in re.finditer(r"(\d+) passed; (\d+) failed", output):
        total += int(match.group(1)) + int(match.group(2))
    return total > 0


def preflight(mutations, repo):
    """Every snippet present exactly once, and actually a change.

    Read from the repository working tree, which `main` has already proved
    clean and at the candidate commit, so this sees exactly the bytes each
    worktree would be checked out with.
    """
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
        # `core.autocrlf=true` with no `.gitattributes` checks these sources out
        # with CRLF, while the manifest snippets are LF. Matching raw bytes would
        # report SNIPPET NOT FOUND for every entry and the run would look like an
        # infrastructure failure rather than the newline mismatch it is.
        source = source.replace("\r\n", "\n")
        if entry["old"] not in source:
            return False, "SNIPPET NOT FOUND", entry["old"][:60]
        if source.count(entry["old"]) != 1:
            return False, "SNIPPET AMBIGUOUS", entry["old"][:60]
        with open(target, "w", encoding="utf-8", newline="") as handle:
            handle.write(source.replace(entry["old"], entry["new"], 1))

        # R2A spans two crates by construction: the terminal VOCABULARY and
        # its maps live in `grapt-provider`, and the carriers that route on
        # them live in `grapt-runtime`. A mutation to a provider file run under
        # `-p grapt-runtime` would not even be rebuilt as the crate under test,
        # so its witness would stay green and be scored as broken rather than as
        # the cross-crate law it is. Every entry therefore names its package.
        command = ["cargo", "test", "-p", entry.get("package", "grapt-runtime")]
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
            return False, "GREEN -- BROKEN WITNESS", "the witness did not notice the mutation"
        return True, "RED (witness fired)", failure_detail(output)
    finally:
        git("worktree", "remove", "--force", workspace, cwd=repo, check=False)


PREFIX_TOKEN = re.compile(r"[A-Z]+")


def select_whole_prefix(mutations, prefix):
    """Rows whose id is exactly this whole alpha token plus decimal digits.

    Whole-token, never `str.startswith`. The campaign allocates a whole prefix
    per lane and several tokens already share a first letter: this very manifest
    holds both `FC01` and `F01`-`F05`, and the store manifest holds `P` beside
    R2C5A0's `PL`. Under a raw prefix match `--prefix F` runs `FC01` too, so one
    lane's battery reports another lane's row as its own evidence.

    The store runner (`re.fullmatch(rf"{prefix}[0-9]+")`) and the provider
    allocation guard (`re.fullmatch(r"([A-Za-z]+)[0-9]+")` compared on group 1)
    are the landed precedent; this is the same law, stated once here so a
    witness can call it without building a worktree.

    An id that is not `<uppercase token><digits>` matches nothing, so a
    malformed row cannot slip into a selection by resembling one.
    """
    if PREFIX_TOKEN.fullmatch(prefix) is None:
        raise ValueError("--prefix must contain uppercase ASCII letters only")
    whole_prefix = re.compile(rf"{re.escape(prefix)}[0-9]+")
    return [entry for entry in mutations if whole_prefix.fullmatch(entry["id"]) is not None]


def main():
    parser = argparse.ArgumentParser(
        description="Run every runtime mutation, or one exact whole-prefix allocation."
    )
    parser.add_argument(
        "--prefix",
        help="run only ids formed from this uppercase whole prefix plus decimal digits",
    )
    args = parser.parse_args()
    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: the candidate worktree is not clean")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)

    with open(MANIFEST, "r", encoding="utf-8") as handle:
        manifest = json.load(handle)
    mutations = manifest["mutations"]
    if args.prefix is not None:
        try:
            mutations = select_whole_prefix(mutations, args.prefix)
        except ValueError as problem:
            print(f"GATE FAILED: {problem}")
            return 1
        if not mutations:
            print(f"GATE FAILED: whole prefix {args.prefix} has no manifest rows")
            return 1
        print(f"selection: whole prefix {args.prefix}")

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

    run_root = tempfile.mkdtemp(prefix=f"grapt-runtime-mutations-{os.getpid()}-")
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
                key = (entry["test"], detail)
                if key in evidence:
                    unaccounted.append(
                        f"{entry['id']}: DUPLICATE EVIDENCE with {evidence[key]} "
                        f"— two laws cannot share one record"
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
