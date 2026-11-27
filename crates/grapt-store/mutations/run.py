#!/usr/bin/env python3
"""Re-runs the F3A mutation witnesses for `grapt-store` in disposable isolation.

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

A row may carry an `edits` LIST instead of one `file`/`old`/`new` triple, and the
two shapes mean the same thing when the list has one element — every row written
before the list existed keeps its exact behaviour.

The list exists because some laws cannot be broken by one file. A declared index
is the case that forced it: changing only the declaration makes the open-time
shape check refuse, so the mutant never reaches the law about whether the index
SERVES the query. Resurrecting the old index consistently — the CREATE and the
required-shape table together — compiles, passes verification, and leaves only
the EXPLAIN-plan witness to notice. Split across two single-file rows that
mutation is inexpressible; it was carried in prose in a receipt instead, which is
an experiment rather than a witness.

A multi-edit row is ATOMIC and its edits are validated as a set:

* every path must resolve INSIDE the disposable worktree. A manifest is data,
  and a row naming `../` or an absolute path would otherwise edit the calling
  repository — the isolation this runner's whole promise rests on.
* no two edits in one row may name the same file, measured by RESOLVED physical
  path rather than by spelling. `src/x.rs` and `src/../src/x.rs` are one file,
  and keying this check on the raw string let that pair through: each anchor was
  then counted against pristine bytes while the second write replaced the first,
  so a row could execute one half of a compound mutant and still be reported as
  the compound mutation.
* every anchor is checked before ANY file is written, and a write that fails
  part-way restores every target the row could have touched — including the one
  whose own write raised. Registering a target only after its write RETURNED
  left that target truncated by the open-and-fail, so the rollback that reported
  itself complete had skipped exactly the file that was damaged. No test runs
  either way: a half-applied mutant that compiled would produce evidence for a
  mutation that was never made.
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

# ---------------------------------------------------------------- H4 pins --
#
# The frozen live PS id set and retired tombstones. Updating either of these is
# a deliberate act that belongs in the same commit as the manifest change it
# describes. Live ids plus retired tombstones must stay contiguous from 1, and
# `preflight` verifies every source anchor before any worktree is built.
FROZEN_LIVE_PS = [
    "PS01", "PS02", "PS03", "PS04", "PS05", "PS06", "PS07", "PS08", "PS09",
    "PS10", "PS11", "PS12", "PS13", "PS14", "PS15", "PS16", "PS17", "PS18",
    "PS19", "PS20", "PS21", "PS22", "PS23", "PS24", "PS28", "PS29", "PS30",
    "PS31", "PS32", "PS33", "PS34", "PS35", "PS36", "PS37", "PS38", "PS40",
    "PS41", "PS42", "PS43", "PS45", "PS47",
]

# Retired ids, preserved so they can never be silently reused. Each names a law
# that no longer exists; see the manifest's `tombstones` entry for why.
RETIRED_PS = ["PS25", "PS26", "PS27", "PS39", "PS44", "PS46"]


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def edits_of(entry):
    """Every row reads as a LIST of edits.

    A `file`/`old`/`new` row is the one-element case rather than a separate code
    path, so there is one apply routine to get right instead of two.
    """
    if "edits" in entry:
        return entry["edits"]
    return [{"file": entry["file"], "old": entry["old"], "new": entry["new"]}]


def contained_path(root, relative):
    """The absolute path of `relative` inside `root`, or None if it escapes.

    `os.path.join` silently discards `root` when handed an absolute path, and
    `..` walks out of it, so both are resolved and then checked rather than
    pattern-matched. Returning None is a refusal the caller reports; there is no
    fallback that edits something else instead.
    """
    ceiling = os.path.realpath(root)
    target = os.path.realpath(os.path.join(ceiling, relative.replace("/", os.sep)))
    if target != ceiling and not target.startswith(ceiling + os.sep):
        return None
    return target


def physical_identity(path):
    """The key that answers "is this the same FILE", not "the same spelling".

    `contained_path` has already run `realpath`, so `..` segments and symlinks
    are gone. What remains is case: on Windows `SRC/x.rs` and `src/x.rs` are one
    file and `realpath` keeps both spellings, so `normcase` is the difference
    between refusing that pair and editing one file twice. On POSIX `normcase`
    is the identity function, which is correct there — those really are two
    files.

    Takes an already-contained absolute path. Passing a raw manifest string
    would key on the spelling this function exists to see past.
    """
    return os.path.normcase(path)


def evidence_key(test, detail):
    """The (test, detail) pair with the PID removed.

    Rust prints `thread '...' (12345) panicked at ...`. The PID differs on every
    process, so two rows whose evidence is otherwise byte-identical would never
    collide and the duplicate guard below would pass while witnessing one law
    twice. Normalising here is what makes that guard mean what it says.
    """
    return test, re.sub(r"\(\d+\) panicked", "(pid) panicked", detail)


INFRASTRUCTURE_DIAGNOSTICS = (
    (
        "frozen fixture absent",
        (
            "required frozen fixture is absent",
            "a fixture run without its frozen row proves nothing",
        ),
    ),
)


def infrastructure_diagnostic(output):
    """Name the evidence-absence diagnostic in this output, or None.

    A nonzero result after at least one test ran is not automatically a
    witness. A test that dies because its EVIDENCE IS MISSING has proved
    nothing about the mutation: it fails identically on unmutated source. To
    score that RED is to convert broken infrastructure into release evidence,
    and it also spends a duplicate-evidence key on a record that witnesses no
    law, so the next row that fails the same way is reported as a duplicate
    rather than as the infrastructure defect both rows really are.

    The set is bounded and keyed by DIAGNOSTIC, never by mutation ID. An
    exception listing CA01-CA06 would hide this same defect the moment any
    other row lost its fixture, and it would quietly stop being true as rows
    are added. Every phrase in a group must be present, so an ordinary
    mutation-sensitive assertion that merely quotes one phrase stays RED.

    Unknown failure is still RED. Only these named, loud, evidence-absence
    diagnostics are unaccounted.
    """
    for name, phrases in INFRASTRUCTURE_DIAGNOSTICS:
        if all(phrase in output for phrase in phrases):
            return f"{name}: the evidence is missing, so this run proves nothing about the mutation"
    return None


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
        edits = edits_of(entry)
        if not edits:
            problems.append(f"{entry['id']}: the row applies no edits")
            continue
        # Keyed by RESOLVED identity, and resolved BEFORE the duplicate check,
        # because two spellings of one file is the case a raw-string key cannot
        # see. Containment runs first so the key is always a real path.
        seen_files = {}
        for edit in edits:
            path = contained_path(repo, edit["file"])
            if path is None:
                problems.append(
                    f"{entry['id']}: PATH ESCAPES THE WORKTREE -- {edit['file']!r}"
                )
                continue
            identity = physical_identity(path)
            if identity in seen_files:
                problems.append(
                    f"{entry['id']}: TWO EDITS NAME ONE FILE -- "
                    f"{seen_files[identity]!r} and {edit['file']!r} resolve to the "
                    f"same path; one edit would move or consume the text the "
                    f"other pins"
                )
                continue
            seen_files[identity] = edit["file"]
            if path not in cache:
                try:
                    with open(path, "r", encoding="utf-8", newline="") as handle:
                        cache[path] = handle.read().replace("\r\n", "\n")
                except OSError as error:
                    cache[path] = None
                    problems.append(
                        f"{entry['id']}: cannot read {edit['file']}: {error}"
                    )
            source = cache[path]
            if source is None:
                continue
            found = source.count(edit["old"])
            if found == 0:
                problems.append(
                    f"{entry['id']}: SNIPPET NOT FOUND in {edit['file']} "
                    f"-- {edit['old'][:60]!r}"
                )
            elif found > 1:
                problems.append(
                    f"{entry['id']}: SNIPPET AMBIGUOUS ({found} matches) in "
                    f"{edit['file']} -- {edit['old'][:60]!r}"
                )
            if edit["old"] == edit["new"]:
                problems.append(
                    f"{entry['id']}: the edit to {edit['file']} changes nothing"
                )
    return problems


def run_one(entry, repo, candidate, run_root):
    workspace = os.path.join(run_root, entry["id"])
    git("worktree", "add", "--detach", "--quiet", workspace, candidate, cwd=repo)
    try:
        # Every edit is resolved and checked BEFORE any file is written, so a row
        # whose second anchor has rotted never leaves a first mutation applied.
        planned = []
        seen_files = {}
        for edit in edits_of(entry):
            target = contained_path(workspace, edit["file"])
            if target is None:
                return False, "PATH ESCAPES THE WORKTREE", edit["file"]
            # Same resolved-identity key as the preflight, and for the same
            # reason: an alias pair passes every per-edit check and then has one
            # write silently replace the other.
            identity = physical_identity(target)
            if identity in seen_files:
                return (
                    False,
                    "TWO EDITS NAME ONE FILE",
                    f"{seen_files[identity]} and {edit['file']} resolve to one file",
                )
            seen_files[identity] = edit["file"]
            with open(target, "r", encoding="utf-8", newline="") as handle:
                pristine = handle.read()
            # `core.autocrlf=true` with no `.gitattributes` checks these sources
            # out with CRLF, while the manifest snippets are LF. Matching raw
            # bytes would report SNIPPET NOT FOUND for every entry and the run
            # would look like an infrastructure failure rather than the newline
            # mismatch it is.
            #
            # `pristine` keeps the bytes as they are ON DISK and is what a
            # rollback puts back, so restoration is exact under either checkout
            # policy. Restoring the NORMALISED text instead would hand a CRLF
            # worktree back its file in LF — a silent rewrite that calls itself
            # a restore.
            source = pristine.replace("\r\n", "\n")
            if edit["old"] not in source:
                return False, "SNIPPET NOT FOUND", f"{edit['file']}: {edit['old'][:60]}"
            if source.count(edit["old"]) != 1:
                return False, "SNIPPET AMBIGUOUS", f"{edit['file']}: {edit['old'][:60]}"
            planned.append((target, pristine, source.replace(edit["old"], edit["new"], 1)))

        # Applied as a set. A write that fails part-way puts back every target
        # it could have damaged, because a half-applied mutant that still
        # compiled would produce evidence for a mutation nobody made.
        #
        # A target is registered BEFORE its write, not after. `open(..., "w")`
        # truncates first, so a write that raises part-way through leaves the
        # file short; registering on RETURN meant the one file the failure
        # actually damaged was the one file the rollback skipped, while the
        # caller still received "rolled back". Registering first can only
        # over-restore — writing pristine bytes over pristine bytes — which is
        # the harmless direction.
        written = []
        try:
            for target, _, mutated in planned:
                written.append(target)
                with open(target, "w", encoding="utf-8", newline="") as handle:
                    handle.write(mutated)
        except OSError as error:
            for target, original, _ in planned:
                if target in written:
                    with open(target, "w", encoding="utf-8", newline="") as handle:
                        handle.write(original)
            return False, "APPLY FAILED (rolled back)", str(error)

        # `package` defaults to `grapt-store`, so every inherited entry runs
        # exactly the command it always did. It exists because a law can live
        # in a crate this package's test command never compiles: a mutation to
        # such a file under `-p grapt-store` is not even built, so its witness
        # would stay GREEN and be scored as a broken witness rather than as the
        # cross-crate law it is. C19-C21 sidestep that by witnessing a
        # STRUCTURAL gate that reads sources from disk; a behavioural law has no
        # such route and needs its own package.
        command = ["cargo", "test", "-p", entry.get("package", "grapt-store")]
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
        # Before failure_detail, and before this row can spend a
        # duplicate-evidence key: a test that died for want of its own frozen
        # evidence is unaccounted infrastructure failure, never a witness.
        absent_evidence = infrastructure_diagnostic(output)
        if absent_evidence is not None:
            return False, "INFRASTRUCTURE FAILURE", absent_evidence
        return True, "RED (witness fired)", failure_detail(output)
    finally:
        git("worktree", "remove", "--force", workspace, cwd=repo, check=False)


def parse_args():
    parser = argparse.ArgumentParser(
        description="Run every store mutation, or one exact whole-prefix allocation."
    )
    parser.add_argument(
        "--prefix",
        help="run only ids formed from this uppercase prefix plus decimal digits",
    )
    return parser.parse_args()


def main():
    args = parse_args()
    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: the candidate worktree is not clean")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)

    # ---------------------------------------------------------------- H4 --
    # Pin checks, checked BEFORE anything runs, and fail closed.
    with open(MANIFEST, "rb") as handle:
        manifest_bytes = handle.read()

    manifest = json.loads(manifest_bytes.decode("utf-8"))
    mutations = manifest["mutations"]

    live_ps = sorted(
        entry["id"] for entry in mutations if re.fullmatch(r"PS[0-9]+", entry["id"])
    )
    if live_ps != sorted(FROZEN_LIVE_PS):
        missing = sorted(set(FROZEN_LIVE_PS) - set(live_ps))
        extra = sorted(set(live_ps) - set(FROZEN_LIVE_PS))
        print("GATE FAILED: live PS id set does not match the frozen set")
        print(f"  missing {missing}")
        print(f"  extra   {extra}")
        return 1
    retired = sorted(set(RETIRED_PS) & set(live_ps))
    if retired:
        print(f"GATE FAILED: retired PS ids reappeared as live rows: {retired}")
        return 1

    # The union of live and retired ids must be CONTIGUOUS from 1. A gap is what
    # a silently deleted row looks like from the outside, and the two pins above
    # cannot see one: dropping PS30 from both the manifest and the frozen set
    # satisfies them exactly. Recorded in prose across three receipts before this
    # made it executable.
    union = sorted(
        int(identifier[2:]) for identifier in set(live_ps) | set(RETIRED_PS)
    )
    expected = list(range(1, len(union) + 1))
    if union != expected:
        print("GATE FAILED: live and retired PS ids are not contiguous from 1")
        print(f"  missing {sorted(set(expected) - set(union))}")
        print(f"  beyond  {sorted(set(union) - set(expected))}")
        return 1
    print(
        f"pins: {len(live_ps)} live PS ids match the frozen "
        f"set; live plus {len(RETIRED_PS)} tombstones are contiguous 1-{union[-1]}"
    )
    if args.prefix is not None:
        if re.fullmatch(r"[A-Z]+", args.prefix) is None:
            print("GATE FAILED: --prefix must contain uppercase ASCII letters only")
            return 1
        whole_prefix = re.compile(rf"{re.escape(args.prefix)}[0-9]+")
        mutations = [
            entry for entry in mutations if whole_prefix.fullmatch(entry["id"]) is not None
        ]
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
    # Rows and anchors are no longer the same number: a multi-edit row pins one
    # anchor per edit. Printing only the row count understates what was checked,
    # and a count in a gate summary should be exactly what it says it is.
    anchors = sum(len(edits_of(entry)) for entry in mutations)
    print(f"preflight: {len(mutations)} rows, all {anchors} anchors present exactly once")

    print(f"{len(mutations)} mutations, each in its own disposable worktree\n")

    run_root = tempfile.mkdtemp(prefix=f"grapt-store-mutations-{os.getpid()}-")
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
