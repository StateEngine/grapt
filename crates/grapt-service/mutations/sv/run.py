#!/usr/bin/env python3
"""Runs the R4D-2C `SV` mutation witnesses for the elected service controller.

Each entry in `manifest.json` names one load-bearing behaviour of the
controller, the exact source edit that removes it, and the witness that must
turn red once it is gone. A witness that stays green under its own mutation
attributes nothing.

This is a SEPARATE battery from R4D-1's `QD` one directory up and R4D-2A's `QS`
alongside it, with its own allocation. It does not read, run or rewrite either;
the runners check each other's manifests only to prove that no two have taken
the same whole-token prefix.

**No mutant is ever written into the calling worktree.** Every mutation is
applied inside its own disposable `git worktree` checked out at the exact
candidate commit, so there is no restore step that could leave a stale build
behind.

Properties this runner enforces that a bare "did it go red" loop does not:

* an empty test filter EXITS ZERO, so a typo in a witness name would score a
  missing test as a passing witness. Every run must report at least one test.
* two mutations whose evidence is byte-identical read as one law witnessed twice
  instead of two laws witnessed once, so the run fails on duplicate
  (witness, detail) evidence. The detail is PID-normalised first: Rust prints
  `thread 'name' (12345) panicked at ...`, and an un-normalised PID makes two
  genuinely identical records look distinct, which silently disarms the guard.
* every snippet is checked for presence and uniqueness BEFORE any worktree is
  built. An unapplied mutant is indistinguishable from a surviving one at the
  end of a run, so the preflight reports every rotted entry at once instead of
  discovering them one compile at a time.
* the `SV` allocation is verified against EVERY manifest in the workspace and
  the selector matches a whole alpha token, never a raw string prefix.

# Why whole-token selection matters MORE for this lane than for `QS`

The `Q` family was the recorded example, but the `S` family this lane joins is
worse, and the figures are this lane's own rather than inherited. Re-walked at
this candidate's base `3cb00a3c` (tree `6a7f3e24`): **17 manifests, 703 ids**
counting tombstones — 21 more than the 682 measured at `4a65a2b9`, because
R4D-2A's `QS` landed in between. That drift is exactly why a prefix is re-walked
at admission and never inherited from a preflight.

Within that census the letter `S` is already carrying two live tokens:
`S` itself with 8 rows and `SC` with 32. Adding `SV` makes three. A
`str.startswith('S')` selector would therefore sweep 40 rows belonging to two
other lanes into this battery's evidence — a larger contamination than the `Q`
collision that first established this law. `--self-test` proves the separation
on exactly those tokens.

Six real ids in this repository carry ordinals mid-string — `M06B` and
`MH1A`/`MH1B`/`MH2A`/`MH2B`/`MH2C` — so a census that derived a family by
stripping trailing digits would invent `MB`, `MHA`, `MHB` and `MHC` and could
report a taken token free. Ids are read verbatim here.

    python crates/grapt-service/mutations/sv/run.py
    python crates/grapt-service/mutations/sv/run.py --prefix SV
    python crates/grapt-service/mutations/sv/run.py --self-test
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
PREFIX = "SV"

# The one untracked entry this battery tolerates in the candidate worktree.
#
# It is the StateEngine platform directory. A strictly-empty porcelain gate is
# wrong here in BOTH directions, and this runner is deliberately asymmetric
# about it:
#
#   * requiring strict emptiness makes the battery unrunnable on any seat whose
#     platform state is live, and the recorded way that gets "fixed" is by
#     MOVING the directory — a custody change made to satisfy a gate, which is
#     worse than the condition it was hiding;
#   * requiring the directory to EXIST would be its own defect, since a clean
#     checkout that never ran the platform has nothing to show.
#
# So absent is green, present-as-untracked is green, and ANY other porcelain
# line is red — including a tracked modification to that same path.
TOLERATED_PORCELAIN = {"?? .stateengine/"}


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def porcelain_problems(repo):
    """Porcelain lines this battery refuses, with the platform entry tolerated.

    Returns the offending lines, so the caller reports WHICH line was refused
    rather than only that something was dirty. A mutation battery that ran with
    an unrelated edit in the tree would be scoring that edit as well as its own.
    """
    status = git("status", "--porcelain", cwd=repo)
    if not status:
        return []
    return [
        line for line in status.splitlines() if line.strip() not in TOLERATED_PORCELAIN
    ]


PID_IN_PANIC = re.compile(r"^thread '([^']*)' \(\d+\) panicked at ")


def failure_detail(output):
    """The line that says WHICH assertion fired, not merely where it lives.

    Rust prints the location first and the message second. Taking only the
    location collapses every case of a table-driven witness into one record.

    The process id is normalised out. `thread 'main' (12345) panicked at ...`
    carries a value that differs on every run, and leaving it in would make the
    duplicate-evidence guard below unable to ever see a duplicate.

    The pattern is anchored to that exact prefix rather than matching any
    parenthesised number: assertion messages routinely print real parenthesised
    values, and blanking those would erase the very content that tells two
    records apart.
    """
    lines = output.splitlines()
    for index, line in enumerate(lines):
        if "panicked at" in line:
            location = line.strip()
            message = lines[index + 1].strip() if index + 1 < len(lines) else ""
            detail = f"{location} {message}".strip()
            return PID_IN_PANIC.sub(r"thread '\1' (pid) panicked at ", detail, count=1)
    # A test declared `-> Result<..>` fails by RETURNING an error, and libtest
    # prints that as a bare `Error: <Debug>` line under the test's stdout
    # header. There is no `panicked at` and no `assertion` anywhere in the
    # output, so the two scans above find nothing.
    #
    # This runner inherited those two scans from a battery whose witnesses all
    # panicked. This lane's do not: most of its witnesses match on a refusal
    # variant and return a described error, which is the shape that carries the
    # most information. Scored without this branch, 19 of 32 rows reported
    # "failed without a captured diagnostic" — every one genuinely RED, and
    # every one indistinguishable from the others as EVIDENCE. A record that
    # cannot say which law fired is exactly what the duplicate-evidence guard
    # exists to refuse, and it was being handed empty strings to compare.
    #
    # The test name is carried alongside the message because the `Error:` line
    # alone does not name its test, and the guard keys on (witness, detail).
    current_test = None
    for line in lines:
        stripped = line.strip()
        header = re.fullmatch(r"---- (.+) stdout ----", stripped)
        if header is not None:
            current_test = header.group(1)
            continue
        if stripped.startswith("Error:"):
            named = f"{current_test}: {stripped}" if current_test else stripped
            return named
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


PREFIX_TOKEN = re.compile(r"[A-Z]+")


def select_whole_prefix(mutations, prefix):
    """Rows whose id is exactly this whole alpha token plus decimal digits.

    Whole-token, never `str.startswith`. The collision is real and present in
    this workspace, and for this lane it is in the `S` family: a bare `S` token
    is live with 8 rows and `SC` with 32, measured by walking every manifest at
    this candidate's base. Under a raw prefix match `--prefix S` would sweep all
    40 of those rows in and report two other lanes' evidence as this one's.

    An id that is not `<uppercase token><digits>` matches nothing, so a
    malformed row cannot slip into a selection by resembling one, and
    `re.escape` means a metacharacter prefix matches literally rather than
    widening the selection.
    """
    if PREFIX_TOKEN.fullmatch(prefix) is None:
        raise ValueError("--prefix must contain uppercase ASCII letters only")
    whole_prefix = re.compile(rf"{re.escape(prefix)}[0-9]+")
    return [entry for entry in mutations if whole_prefix.fullmatch(entry["id"]) is not None]


PRUNED_DIRS = {".git", "target", "node_modules", "__pycache__"}


def census_manifests(repo):
    """Every `manifest.json` under any `mutations` directory, at any depth.

    Heavy directories are pruned so this stays a search of the source tree and
    not of build output. Anything named `manifest.json` with a `mutations` path
    component counts, which is the shape every known layout shares — including
    this lane's own extra level, `crates/<crate>/mutations/<lane>/`.
    """
    found = []
    for current, directories, files in os.walk(repo):
        directories[:] = [name for name in directories if name not in PRUNED_DIRS]
        if "manifest.json" not in files:
            continue
        relative = os.path.relpath(current, repo).replace(os.sep, "/").split("/")
        if "mutations" in relative:
            found.append(os.path.join(current, "manifest.json"))
    return found


def allocation_problems(mutations, repo):
    """Fail closed unless `QS` is this lane's alone, across every manifest.

    Two separate checks, because they fail in different directions:

    * every row HERE is a whole `SV` token, so a stray id cannot ride along in
      this lane's battery without being selected by its own allocation; and
    * no OTHER manifest in the workspace allocates the `SV` token — including
      R4D-2A's `QS` manifest in the sibling directory and R4D-1's `QD` manifest
      one directory up, which are the two nearest and therefore likeliest
      collisions.

    Note what is deliberately NOT asserted: global id uniqueness. Ids already
    repeat legitimately across crates — `M01`-`M15` exist in `grapt-event`,
    `grapt-schema` and `grapt-store` at once — because each runner scores its
    own manifest. A uniqueness check would fail on day one for a reason that has
    nothing to do with this allocation.

    The census WALKS the tree rather than globbing one shape. This repository
    holds manifests at three different depths now — `crates/<crate>/mutations/`,
    `crates/<crate>/mutations/<lane>/` and `mutations/<lane>/` at the root — and
    a census that only globbed `crates/*/mutations/` would silently miss both
    the root lanes and this one. A prefix census that cannot see a manifest
    reports its token free, which is the one answer it must never get wrong.

    Re-walked in this candidate from accepted base `eefea222`: 21 manifests and
    822 ids counting tombstones, including this manifest's 44 rows. These are
    observations, not inherited allocation authority. Tombstones are counted
    because a retired id is still a consumed one: reusing it is exactly the
    collision a census exists to prevent.

    Ids are read VERBATIM and each is asserted against `^[A-Z]+[0-9]+$` rather
    than having a prefix derived by stripping digits. Six real ids in this
    repository carry their ordinals mid-string — `M06B` and
    `MH1A`/`MH1B`/`MH2A`/`MH2B`/`MH2C` — and a trailing-anchored regex either
    drops them or invents families that were never allocated.
    """
    problems = []
    for entry in mutations:
        if re.fullmatch(rf"{PREFIX}[0-9]+", entry["id"]) is None:
            problems.append(
                f"{entry['id']}: this manifest may contain only whole {PREFIX} token rows"
            )

    seen_other = 0
    for path in sorted(census_manifests(repo)):
        if os.path.abspath(path) == os.path.abspath(MANIFEST):
            continue
        seen_other += 1
        try:
            with open(path, "r", encoding="utf-8") as handle:
                other = json.load(handle)
        except (OSError, ValueError) as error:
            problems.append(f"cannot read {path}: {error}")
            continue
        stolen = [
            row["id"]
            for row in other.get("mutations", [])
            if re.fullmatch(rf"{PREFIX}[0-9]+", row.get("id", "")) is not None
        ]
        if stolen:
            relative = os.path.relpath(path, repo)
            problems.append(f"{relative} also allocates {PREFIX}: {stolen}")

    # A census that found nothing to compare against would report this token
    # free for the wrong reason. There are at least two: R4D-2A's `QS` manifest
    # in the sibling directory and R4D-1's `QD` manifest one level up.
    if seen_other == 0:
        problems.append(
            "the census found no other manifest to compare against; "
            "a census that cannot see a manifest reports its token free"
        )
    return problems


def preflight(mutations, repo):
    """Every snippet present exactly once, and actually a change.

    Read from the repository working tree, which `main` has already proved
    clean (modulo the tolerated platform entry) and at the candidate commit, so
    this sees exactly the bytes each worktree would be checked out with.
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
        # with CRLF while the manifest snippets are LF. Matching raw bytes would
        # report SNIPPET NOT FOUND for every entry and the run would look like
        # an infrastructure failure rather than the newline mismatch it is.
        source = source.replace("\r\n", "\n")
        if entry["old"] not in source:
            return False, "SNIPPET NOT FOUND", entry["old"][:60]
        if source.count(entry["old"]) != 1:
            return False, "SNIPPET AMBIGUOUS", entry["old"][:60]
        with open(target, "w", encoding="utf-8", newline="") as handle:
            handle.write(source.replace(entry["old"], entry["new"], 1))

        command = ["cargo", "test", "-p", entry.get("package", "grapt-service")]
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


SELF_TEST_ROWS = [
    {"id": "SV01"},
    {"id": "SV14"},
    {"id": "SC01"},
    {"id": "SC32"},
    {"id": "S01"},
    {"id": "S08"},
    {"id": "QD01"},
    {"id": "QS01"},
    {"id": "SV.1"},
]


def self_test():
    """Proves the selector takes a WHOLE token, on real and hostile ids.

    A lane's battery is its evidence. If `--prefix S` also ran `SV01`, this lane
    would hand another lane's rows in as its own RED, and the campaign's
    whole-prefix allocation would stop meaning anything mechanically.

    The `S` family matters most here, and it is a worse collision than the `Q`
    one that established this law: a bare `S` token and `SC` are both live in
    this workspace, 8 rows and 32 rows respectively, so a stem match would pull
    in 40 foreign rows. `QD` and `QS` are included as the two nearest
    same-crate neighbours.
    """
    failures = []

    def check(name, actual, expected):
        if actual == expected:
            print(f"  ok   {name}")
        else:
            failures.append(f"{name}: expected {expected!r}, got {actual!r}")
            print(f"  FAIL {name}: expected {expected!r}, got {actual!r}")

    with open(MANIFEST, "r", encoding="utf-8") as handle:
        rows = json.load(handle)["mutations"]

    # 1. This lane's real allocation selects exactly its own rows.
    check(
        f"{PREFIX} selects this manifest's rows",
        [entry["id"] for entry in select_whole_prefix(rows, PREFIX)],
        [entry["id"] for entry in rows],
    )

    # 2. Three live tokens now share the letter S.
    check(
        "S selects only bare S rows",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "S")],
        ["S01", "S08"],
    )
    check(
        "SC selects only SC rows",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "SC")],
        ["SC01", "SC32"],
    )
    check(
        "SV selects only SV rows",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "SV")],
        ["SV01", "SV14"],
    )
    # And the two nearest same-crate neighbours stay separate.
    check(
        "QD selects only QD rows",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "QD")],
        ["QD01"],
    )
    check(
        "QS selects only QS rows",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "QS")],
        ["QS01"],
    )

    # 3. The counterexample: state what a raw prefix match would have done, so
    #    this fails loudly if anyone restores `startswith` selection.
    startswith_s = [row["id"] for row in SELF_TEST_ROWS if row["id"].startswith("S")]
    check(
        "startswith('S') would have swept in every SC and SV row",
        sorted(set(startswith_s) - {entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "S")}),
        ["SC01", "SC32", "SV.1", "SV01", "SV14"],
    )

    # 4. A prefix that is not an uppercase alpha token is refused, not coerced.
    for bad in ("sv", "SV1", "", "SV*"):
        try:
            select_whole_prefix(rows, bad)
        except ValueError:
            print(f"  ok   refuses {bad!r}")
        else:
            failures.append(f"accepted a malformed prefix {bad!r}")
            print(f"  FAIL accepted a malformed prefix {bad!r}")

    # 5. A metacharacter in an otherwise well-formed token matches literally.
    #    `SV.1` is a malformed ROW; it must not be reachable from any selection.
    check(
        "a malformed row is unreachable from every allocation",
        [
            entry["id"]
            for token in ("S", "SC", "SV", "QD", "QS")
            for entry in select_whole_prefix(SELF_TEST_ROWS, token)
            if entry["id"] == "SV.1"
        ],
        [],
    )

    # 6. The porcelain tolerance is exactly one entry, in one direction.
    check(
        "the tolerated porcelain set is exactly the platform directory",
        sorted(TOLERATED_PORCELAIN),
        ["?? .stateengine/"],
    )

    if failures:
        print(f"\nSELF TEST FAILED: {len(failures)} problem(s)")
        for failure in failures:
            print(f"  {failure}")
        return 1
    print("\nself test: whole-token selection holds")
    return 0


def main():
    parser = argparse.ArgumentParser(
        description="Run every R4D-2C service controller mutation, or one exact whole-prefix allocation."
    )
    parser.add_argument(
        "--prefix",
        help="run only ids formed from this uppercase whole prefix plus decimal digits",
    )
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="prove the whole-token selector and exit without building anything",
    )
    args = parser.parse_args()

    if args.self_test:
        return self_test()

    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    dirty = porcelain_problems(repo)
    if dirty:
        print("GATE FAILED: the candidate worktree carries changes this battery does not tolerate:")
        for line in dirty:
            print(f"  {line}")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)

    with open(MANIFEST, "r", encoding="utf-8") as handle:
        manifest = json.load(handle)
    mutations = manifest["mutations"]

    stolen = allocation_problems(mutations, repo)
    if stolen:
        print(f"GATE FAILED: the {PREFIX} allocation is not exclusive:")
        for problem in stolen:
            print(f"  {problem}")
        return 1
    print(f"allocation: {PREFIX} is exclusive across every workspace manifest")

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

    # The run root is the OS temp directory, never inside the repository. A
    # battery that wrote its own working files into the candidate would fail its
    # own post-run custody check for a reason that has nothing to do with a
    # mutation.
    run_root = tempfile.mkdtemp(prefix=f"grapt-service-sv-{os.getpid()}-")
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
    dirty = porcelain_problems(repo)
    if dirty or git("rev-parse", "HEAD", cwd=repo) != candidate:
        print("GATE FAILED: the candidate worktree changed during the run")
        for line in dirty:
            print(f"  {line}")
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
