#!/usr/bin/env python3
"""Runs the IA0-2 U1b1 `IGL` mutation witnesses for `grapt-config` source custody.

Each entry in `manifest.json` names one load-bearing behaviour of source
custody, the exact source edit that removes it, and the witness that must turn
red once it is gone. A witness that stays green under its own mutation
attributes nothing.

**No mutant is ever written into the calling worktree.** Every mutation is
applied inside its own disposable `git worktree` at the exact candidate commit,
so no restore step can leave a stale build behind.

**No mutant build touches an ambient target directory.** Each worktree gets its
own deterministic `CARGO_TARGET_DIR` INSIDE that worktree, so a row is never
scored against artifacts another row produced, and never against the caller's.
Inheriting the ambient target is the defect this lane's provider neighbour was
opened to close; this runner does not reproduce it. The variable is SET rather
than merely unset, because unsetting it would fall back to the worktree default
only by convention, and a convention is not an isolation.

Properties this runner enforces that a bare "did it go red" loop does not:

* an empty test filter EXITS ZERO, so a typo in a witness name would score a
  missing test as a passing witness. Every run must report at least one test.
* byte-identical evidence from two rows reads as one law witnessed twice, so the
  run fails on duplicate (witness, detail) records. The detail is PID-normalised
  first, because an un-normalised PID makes identical records look distinct and
  silently disarms the guard.
* every snippet is checked for presence and uniqueness BEFORE any worktree is
  built. An unapplied mutant is indistinguishable from a surviving one at the end
  of a run.
* the `IGL` allocation is verified against EVERY manifest in the workspace and
  the selector matches a whole alpha token, never a raw string prefix. See
  `--self-test`.

    python crates/grapt-config/mutations/igl/run.py
    python crates/grapt-config/mutations/igl/run.py --prefix IGL
    python crates/grapt-config/mutations/igl/run.py --self-test
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
PREFIX = "IGL"


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


PID_IN_PANIC = re.compile(r"^thread '([^']*)' \(\d+\) panicked at ")


def failure_detail(output):
    """The line that says WHICH assertion fired, not merely where it lives.

    Rust prints the location first and the message second. Taking only the
    location collapses every case of a table-driven witness into one record.

    The process id is normalised out. `thread 'main' (12345) panicked at ...`
    carries a value that differs on every run, and leaving it in would make the
    duplicate-evidence guard below unable to ever see a duplicate.

    The pattern is anchored to that exact prefix rather than matching any
    parenthesised number: an assertion message routinely prints real
    parenthesised values, and blanking those would erase the very content that
    tells two records apart.
    """
    lines = output.splitlines()
    for index, line in enumerate(lines):
        if "panicked at" in line:
            location = line.strip()
            message = lines[index + 1].strip() if index + 1 < len(lines) else ""
            detail = f"{location} {message}".strip()
            return PID_IN_PANIC.sub(r"thread '\1' (pid) panicked at ", detail, count=1)
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

    Whole-token, never `str.startswith`: a raw `--prefix I` would sweep this
    lane's `IGL` rows, U1a's `IGC` rows and `grapt-store`'s `IAB` rows into one
    battery and report each lane's evidence as the others'. `IGL` and `IGC` are
    the closest pair in the workspace, sharing two of three letters, which is
    exactly the case a prefix match gets wrong quietly.

    An id that is not `<uppercase token><digits>` matches nothing, and
    `re.escape` means a metacharacter prefix matches literally.
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
    this one, which sits one level deeper than U1a's beside it.
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
    """Fail closed unless `IGL` is this lane's alone, across every manifest.

    Two separate checks, because they fail in different directions: every row
    HERE is a whole `IGL` token, and no OTHER manifest allocates `IGL`.

    Deliberately NOT asserted: global id uniqueness. Ids repeat legitimately
    across crates because each runner scores its own manifest.

    The census WALKS the tree rather than globbing one shape. A
    `crates/*/mutations/manifest.json` glob would miss the nested manifests —
    this file's own included — and report every token they hold as free, which
    is the one answer a prefix census must never get wrong.
    """
    problems = []
    for entry in mutations:
        if re.fullmatch(rf"{PREFIX}[0-9]+", entry["id"]) is None:
            problems.append(
                f"{entry['id']}: this manifest may contain only whole {PREFIX} token rows"
            )

    for path in sorted(census_manifests(repo)):
        if os.path.abspath(path) == os.path.abspath(MANIFEST):
            continue
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
    return problems


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


def isolated_environment(workspace):
    """This worktree's own cargo target, set rather than inherited.

    An ambient `CARGO_TARGET_DIR` in the caller's environment would otherwise
    be inherited by every mutant build at once: rows would share one target,
    a row could be scored against a neighbour's artifacts, and a hostile or
    merely stale ambient target could serve an UNMUTATED binary to a row that
    then reports GREEN -- BROKEN WITNESS for a law that is in fact held.

    The path is deterministic and inside the disposable worktree, so it is
    removed with it and no cleanup step can forget it.
    """
    environment = dict(os.environ)
    environment["CARGO_TARGET_DIR"] = os.path.join(workspace, "mutation-target")
    return environment


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

        command = ["cargo", "test", "-p", entry.get("package", "grapt-config")]
        if "test_target" in entry:
            command += ["--test", entry["test_target"]]
        else:
            command += ["--lib"]
        command += ["--", "--exact", entry["test"]]

        result = subprocess.run(
            command,
            cwd=workspace,
            capture_output=True,
            text=True,
            check=False,
            env=isolated_environment(workspace),
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
    {"id": "IGL01"},
    {"id": "IGL10"},
    {"id": "IGC07"},
    {"id": "IGC31"},
    {"id": "IAB01"},
    {"id": "IGL.1"},
]


def self_test():
    """Proves the selector takes a WHOLE token, on real and hostile ids.

    A lane's battery is its evidence. `IGL` and `IGC` are the same crate's two
    allocations and share two letters; if either token could take the other's
    rows, U1b1 would hand U1a's landed evidence in as its own RED and the
    campaign's whole-prefix allocation would stop meaning anything mechanically.
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

    # 2. The neighbour in the SAME crate: U1a owns `IGC`, U1b owns `IGL`.
    #    Neither token may take the other's rows, and neither may take
    #    `grapt-store`'s `IAB`.
    check(
        "IGL does not select any IGC or IAB row",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "IGL")],
        ["IGL01", "IGL10"],
    )
    check(
        "IGC does not select any IGL row",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "IGC")],
        ["IGC07", "IGC31"],
    )
    check(
        "the shared letter selects nothing",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "I")],
        [],
    )
    check(
        "the shared two-letter stem selects nothing",
        [entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "IG")],
        [],
    )

    # 3. The counterexample: state what a raw prefix match would have done, so
    #    this fails loudly if anyone restores `startswith` selection. Under
    #    `startswith` the stem `IG` sweeps both of this crate's lanes at once.
    startswith_ig = [row["id"] for row in SELF_TEST_ROWS if row["id"].startswith("IG")]
    check(
        "startswith('IG') would have swept both of this crate's lanes into one battery",
        sorted(
            set(startswith_ig)
            - {entry["id"] for entry in select_whole_prefix(SELF_TEST_ROWS, "IG")}
        ),
        ["IGC07", "IGC31", "IGL.1", "IGL01", "IGL10"],
    )

    # 4. A prefix that is not an uppercase alpha token is refused, not coerced.
    for bad in ("igl", "IGL1", "", "IGL*"):
        try:
            select_whole_prefix(rows, bad)
        except ValueError:
            print(f"  ok   refuses {bad!r}")
        else:
            failures.append(f"accepted a malformed prefix {bad!r}")
            print(f"  FAIL accepted a malformed prefix {bad!r}")

    # 5. A metacharacter in an otherwise well-formed token matches literally.
    #    `IGL.1` is a malformed ROW; it must not be reachable from any selection.
    check(
        "a malformed row is unreachable from every allocation",
        [
            entry["id"]
            for token in ("I", "IG", "IGL", "IGC", "IAB")
            for entry in select_whole_prefix(SELF_TEST_ROWS, token)
            if entry["id"] == "IGL.1"
        ],
        [],
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
        description="Run every grapt-config source-custody mutation, or one exact whole-prefix allocation."
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
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: the candidate worktree is not clean")
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

    print(f"{len(mutations)} mutations, each in its own disposable worktree and its own target\n")

    run_root = tempfile.mkdtemp(prefix=f"grapt-config-igl-mutations-{os.getpid()}-")
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
