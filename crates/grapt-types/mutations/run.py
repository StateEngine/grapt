#!/usr/bin/env python3
"""Run this crate's mutation witnesses in isolated worktrees.

    The rows are enumerated from the manifest, never from this docstring: an
    earlier version named a fixed range and went stale the moment a row was
    added (R2 finding Z1).

The runner refuses dirty or moving candidates, checks every anchor exactly
once before compiling, proves every exact filter ran at least one test, rejects
compile kills and green mutants, and rejects duplicate evidence. Its inventory
counts live rows separately from tombstones while using both for ID uniqueness.

One engine, three profiles. `--prefix PL` (the default) scores the R2C5A0
attempt receipt rows; `--prefix AUTH` scores the authority-boundary rows; and
`--prefix MC` scores the L1 model-target capability identity rows. Dispatch is a
STATIC EXACT MAP from a whole prefix token to one repo-contained manifest path.
There is deliberately no `--manifest` option: an arbitrary path would let a
caller point this engine at a document the global census never validated, and
the census is the only thing that proves a prefix is owned in exactly one
place. Whichever profile is selected, the global inventory validates BOTH.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))

# Static exact dispatch: whole prefix token -> the ONE manifest that owns it.
# Fixed, repo-contained paths derived from this file's own location.
PREFIX_MANIFESTS = {
    "PL": os.path.join(HERE, "manifest.json"),
    "AUTH": os.path.join(HERE, "auth", "manifest.json"),
    "MC": os.path.join(HERE, "mc", "manifest.json"),
    "MR": os.path.join(HERE, "mr", "manifest.json"),
}

# The same map keyed by repo-relative path, which is how the global census
# recognizes an owning manifest while walking the tree.
OWNED_RELATIVE = {
    "PL": "crates/grapt-types/mutations/manifest.json",
    "AUTH": "crates/grapt-types/mutations/auth/manifest.json",
    "MC": "crates/grapt-types/mutations/mc/manifest.json",
    "MR": "crates/grapt-types/mutations/mr/manifest.json",
}
OWNER_OF_RELATIVE = {relative: prefix for prefix, relative in OWNED_RELATIVE.items()}

DEFAULT_PREFIX = "PL"

# Strict whole-token grammar: the ENTIRE id is one run of uppercase ASCII
# letters followed by one run of digits, and the token is that whole letter
# run. No case folding, no surrounding whitespace, no separators.
SELECTOR = re.compile(r"([A-Z]+)[0-9]+")


def selector_token(mutation_id):
    """The whole prefix token of a mutation id, or None if it is not one.

    `AUTH01` is token `AUTH`. It is NOT token `A`, NOT `AU`, and `AUTHA01` is
    the DIFFERENT token `AUTHA` rather than `AUTH` with a suffix. This is the
    comparison the whole design rests on: `A` is a live prefix elsewhere in
    this workspace, so any containment-based test would make every AUTH row
    collide with it.
    """
    if not isinstance(mutation_id, str):
        return None
    match = SELECTOR.fullmatch(mutation_id)
    return match.group(1) if match else None


# Hostile cases the selector must classify exactly. This table is MANDATORY:
# `main` refuses to apply a single mutation until every row of it holds.
SELECTOR_SELF_TEST = (
    ("AUTH01", "AUTH"),
    ("AUTH1", "AUTH"),
    ("AUTH12", "AUTH"),
    ("A01", "A"),
    ("A1", "A"),
    ("AU01", "AU"),
    ("AUTHA01", "AUTHA"),
    ("AUTHX9", "AUTHX"),
    ("auth01", None),
    ("Auth01", None),
    ("aUTH01", None),
    ("AUTH", None),
    ("AUTH0A", None),
    ("01AUTH", None),
    ("AUTH_01", None),
    ("AUTH-01", None),
    ("AUTH 01", None),
    (" AUTH01", None),
    ("AUTH01 ", None),
    ("AUTH01\n", None),
    ("PL01", "PL"),
    ("P01", "P"),
    ("PLX01", "PLX"),
    ("pl01", None),
    # MC (L1 model-target capability identity). `M` is a live prefix with 116
    # rows in this workspace, so the whole-token rule is what keeps MC01 out of
    # `--prefix M` and keeps every `M` row out of `--prefix MC`.
    ("MC01", "MC"),
    ("MC1", "MC"),
    ("MC", None),
    ("MCA", None),
    ("MC0A", None),
    ("MC01B", None),
    ("MCC01", "MCC"),
    ("mc01", None),
    ("MC-01", None),
    ("MC_01", None),
    ("M C01", None),
    ("MC 01", None),
    ("", None),
    ("M01", "M"),
)


def selector_self_test():
    """Prove the selector before it is trusted to account for anything."""
    problems = []
    for mutation_id, expected in SELECTOR_SELF_TEST:
        actual = selector_token(mutation_id)
        if actual != expected:
            problems.append(
                f"selector {mutation_id!r}: expected token {expected!r}, got {actual!r}"
            )

    # AUTH must never claim a row belonging to A, AU, a longer token, a
    # lowercase spelling, or PL.
    for hostile in ("A01", "AU01", "AUTHA01", "AUTHX01", "auth01", "PL01"):
        if selector_token(hostile) == "AUTH":
            problems.append(f"selector admitted {hostile} as AUTH")
    # ...and the reverse: an AUTH row must never be read as one of those.
    for foreign in ("A", "AU", "AUTHA", "AUTHX", "PL"):
        if selector_token("AUTH01") == foreign:
            problems.append(f"selector read AUTH01 as {foreign}")

    # The map itself must be whole-token distinct, or dispatch is ambiguous
    # no matter how careful the comparison is.
    for owned in PREFIX_MANIFESTS:
        if selector_token(f"{owned}01") != owned:
            problems.append(f"owned prefix {owned} is not a whole selector token")
        for other in PREFIX_MANIFESTS:
            if owned != other and (owned.startswith(other) or other.startswith(owned)):
                problems.append(
                    f"owned prefixes {owned} and {other} are not whole-token distinct"
                )
    return problems


def parse_prefix(argv):
    """Exactly `--prefix X` or `--prefix=X`, and nothing else.

    Unknown flags are refused BY NAME rather than silently dropped: a runner
    that ignores an argument reports a coverage the caller did not ask for.
    """
    prefix = DEFAULT_PREFIX
    index = 0
    while index < len(argv):
        argument = argv[index]
        if argument == "--prefix":
            if index + 1 >= len(argv):
                raise ValueError("--prefix requires a value")
            prefix = argv[index + 1]
            index += 2
            continue
        if argument.startswith("--prefix="):
            prefix = argument[len("--prefix=") :]
            index += 1
            continue
        raise ValueError(f"unknown argument {argument!r}; this runner accepts --prefix only")
    if prefix not in PREFIX_MANIFESTS:
        known = ", ".join(sorted(PREFIX_MANIFESTS))
        raise ValueError(f"unknown prefix {prefix!r}; this runner dispatches {known} only")
    return prefix


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


class InventoryFailure(RuntimeError):
    """A manifest could not be read or is malformed.

    H1 (review, adopted): the first version swallowed every OSError and
    JSONDecodeError with `continue`, and silently ignored any document whose
    `mutations` key was not a list. A manifest that cannot be read is not a
    manifest with no rows — it is evidence the run did not see, and a gate that
    skips evidence reports a coverage it never had. Every failure below is
    terminal and names the exact path and reason.
    """


def mutation_manifests(repo):
    paths = []
    for root, dirs, files in os.walk(repo):
        dirs[:] = [name for name in dirs if name not in {".git", "target"}]
        if "manifest.json" not in files:
            continue
        path = os.path.join(root, "manifest.json")
        relative = os.path.relpath(path, repo).replace(os.sep, "/")
        try:
            with open(path, "r", encoding="utf-8") as handle:
                raw = handle.read()
        except OSError as error:
            raise InventoryFailure(f"{relative}: cannot be read: {error}") from error
        try:
            document = json.loads(raw)
        except json.JSONDecodeError as error:
            raise InventoryFailure(f"{relative}: is not valid JSON: {error}") from error
        if not isinstance(document, dict):
            raise InventoryFailure(
                f"{relative}: top level is {type(document).__name__}, not an object"
            )
        for key in ("mutations", "tombstones"):
            rows = document.get(key, [])
            if not isinstance(rows, list):
                raise InventoryFailure(
                    f"{relative}: {key} is {type(rows).__name__}, not a list"
                )
            for index, row in enumerate(rows):
                if not isinstance(row, dict):
                    raise InventoryFailure(
                        f"{relative}: {key}[{index}] is {type(row).__name__}, not an object"
                    )
                if not isinstance(row.get("id"), str) or not row["id"]:
                    raise InventoryFailure(
                        f"{relative}: {key}[{index}] has no non-empty string id"
                    )
        paths.append((path, document))
    return sorted(paths, key=lambda item: item[0])


# Cleanup problems observed across the run. A non-empty list makes the run
# RED at the terminal, per H2: a cleanup failure must never print success.
CLEANUP_PROBLEMS = []


def inventory(repo):
    """Manifest-local IDs include tombstones; global live counts exclude them.

    Historical manifests intentionally reuse local IDs such as M01. Prefix
    allocation is the global boundary, so each owned prefix must be absent
    everywhere outside the ONE manifest mapped to it, while each manifest
    checks its own live/tombstone namespace together.

    The prefix is compared WHOLE. `PL` is not `P` and `AUTH` is not `A`: a
    containment comparison would collide with the landed store rows and with
    the live `A` rows, which is exactly the trap the two-letter prefixes
    MH/ML/FC already prove is real.

    Both owned manifests are validated on every run, whichever one was
    selected. A census that only looked at the profile being scored could not
    see the collision it exists to refuse.
    """
    live = 0
    retired = 0
    qualified = 0
    problems = []
    owned_rows = {prefix: 0 for prefix in OWNED_RELATIVE}
    for path, document in mutation_manifests(repo):
        relative = os.path.relpath(path, repo).replace(os.sep, "/")
        owner = OWNER_OF_RELATIVE.get(relative)
        seen = {}
        for kind, rows in (
            ("live", document.get("mutations", [])),
            ("tombstone", document.get("tombstones", [])),
        ):
            for row in rows:
                mutation_id = row.get("id")
                if not isinstance(mutation_id, str):
                    problems.append(f"{relative}: {kind} row has no string id")
                    continue
                if mutation_id in seen:
                    problems.append(
                        f"{relative}: duplicate local id {mutation_id} in "
                        f"{seen[mutation_id]} and {kind} rows"
                    )
                else:
                    seen[mutation_id] = kind
                qualified += 1
                if kind == "live":
                    live += 1
                else:
                    retired += 1
                token = selector_token(mutation_id)
                if owner is not None:
                    if token != owner:
                        problems.append(
                            f"{relative}: row {mutation_id} does not use whole prefix {owner}"
                        )
                    else:
                        owned_rows[owner] += 1
                elif token in OWNED_RELATIVE:
                    problems.append(
                        f"whole prefix {token} is already used outside "
                        f"{OWNED_RELATIVE[token]}: {relative}"
                    )
    for prefix, count in sorted(owned_rows.items()):
        if count == 0:
            problems.append(f"{OWNED_RELATIVE[prefix]} has no {prefix}-prefixed row")
    return live, retired, qualified, problems


def failure_detail(output):
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
    total = 0
    for match in re.finditer(r"(\d+) passed; (\d+) failed", output):
        total += int(match.group(1)) + int(match.group(2))
    return total > 0


def preflight(mutations, repo):
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
            problems.append(f"{entry['id']}: snippet not found in {entry['file']}")
        elif found > 1:
            problems.append(
                f"{entry['id']}: snippet ambiguous ({found} matches) in {entry['file']}"
            )
        if entry["old"] == entry["new"]:
            problems.append(f"{entry['id']}: mutation changes nothing")
    return problems


def registered_worktrees(repo):
    """The exact set of worktree paths git currently knows about."""
    listed = git("worktree", "list", "--porcelain", cwd=repo)
    return {
        os.path.normcase(os.path.abspath(line[len("worktree ") :]))
        for line in listed.splitlines()
        if line.startswith("worktree ")
    }


def verify_removed(repo, workspace, baseline):
    """H2: removal is a CHECKED operation, not a best-effort call.

    Returns a list of problems. A worktree this run registered must be absent
    from git's registry AND from disk afterwards; a pre-existing or foreign
    worktree is never inspected for removal, because this run does not own it.
    """
    problems = []
    target = os.path.normcase(os.path.abspath(workspace))
    if target in baseline:
        return [f"{workspace}: pre-existing worktree — this run must not own or remove it"]
    if target in registered_worktrees(repo):
        problems.append(f"{workspace}: still registered as a git worktree after removal")
    if os.path.exists(workspace):
        problems.append(f"{workspace}: directory still present after removal")
    return problems


def run_one(entry, repo, candidate, run_root, baseline):
    workspace = os.path.join(run_root, entry["id"])
    if os.path.exists(workspace):
        return False, "WORKTREE PATH ALREADY EXISTS", workspace
    git("worktree", "add", "--detach", "--quiet", workspace, candidate, cwd=repo)
    try:
        target = os.path.join(workspace, entry["file"].replace("/", os.sep))
        with open(target, "r", encoding="utf-8", newline="") as handle:
            source = handle.read().replace("\r\n", "\n")
        if source.count(entry["old"]) != 1:
            return False, "ANCHOR CHANGED AFTER PREFLIGHT", entry["old"][:60]
        with open(target, "w", encoding="utf-8", newline="") as handle:
            handle.write(source.replace(entry["old"], entry["new"], 1))

        command = [
            "cargo",
            "test",
            "-p",
            entry["package"],
        ]
        if "test_target" in entry:
            command += ["--test", entry["test_target"]]
        else:
            command += ["--lib"]
        command += ["--no-fail-fast", "--", "--exact", entry["test"]]
        result = subprocess.run(
            command, cwd=workspace, capture_output=True, text=True, check=False
        )
        output = result.stdout + result.stderr

        if result.returncode != 0 and not ran_at_least_one_test(output):
            return False, "COMPILE KILL (not a witness)", output.strip()[-240:]
        if not ran_at_least_one_test(output):
            return False, "NO TEST RAN", "the exact filter matched nothing"
        if result.returncode == 0:
            return False, "GREEN -- BROKEN WITNESS", "the witness missed its mutation"
        return True, "RED (witness fired)", failure_detail(output)
    finally:
        # Checked removal. `check=False` is retained so a removal failure is
        # REPORTED by verify_removed rather than raised out of the `finally`
        # and masking the row's own result.
        git("worktree", "remove", "--force", workspace, cwd=repo, check=False)
        CLEANUP_PROBLEMS.extend(verify_removed(repo, workspace, baseline))


def main(argv):
    try:
        prefix = parse_prefix(argv)
    except ValueError as error:
        print(f"REFUSED: {error}")
        return 1

    selector_problems = selector_self_test()
    if selector_problems:
        print(f"SELECTOR SELF-TEST FAILED: {len(selector_problems)} problem(s)")
        for problem in selector_problems:
            print(f"  {problem}")
        return 1
    print(
        f"selector self-test: {len(SELECTOR_SELF_TEST)} hostile cases hold; "
        f"dispatching whole token {prefix}"
    )

    manifest_path = PREFIX_MANIFESTS[prefix]
    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: the candidate worktree is not clean")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)
    with open(manifest_path, "r", encoding="utf-8") as handle:
        mutations = json.load(handle)["mutations"]
    foreign = sorted(
        {row["id"] for row in mutations if selector_token(row.get("id")) != prefix}
    )
    if foreign:
        print(f"GATE FAILED: {os.path.relpath(manifest_path, repo)} carries non-{prefix} rows")
        for mutation_id in foreign:
            print(f"  {mutation_id}")
        return 1

    print(f"candidate {candidate}")
    try:
        live, retired, qualified, inventory_problems = inventory(repo)
    except InventoryFailure as failure:
        print(f"INVENTORY FAILED: {failure}")
        return 1
    print(
        f"inventory: {live} live mutations; {retired} tombstone(s); "
        f"{qualified} manifest-qualified ids"
    )
    if inventory_problems:
        for problem in inventory_problems:
            print(f"  {problem}")
        return 1

    rotted = preflight(mutations, repo)
    if rotted:
        print(f"PREFLIGHT FAILED: {len(rotted)} problem(s)")
        for problem in rotted:
            print(f"  {problem}")
        return 1
    print(f"preflight: all {len(mutations)} snippets present exactly once")

    # H2: snapshot what git already knows about, so this run can prove it
    # removed exactly what IT registered and never touches a foreign worktree.
    baseline = registered_worktrees(repo)
    run_root = tempfile.mkdtemp(prefix=f"grapt-types-mutations-{os.getpid()}-")
    unaccounted = []
    evidence = {}
    try:
        for entry in mutations:
            accounted, label, detail = run_one(entry, repo, candidate, run_root, baseline)
            print(f"  {entry['id']}  {label}")
            if detail:
                print(f"      {detail}")
            if not accounted:
                unaccounted.append(f"{entry['id']}: {label}")
                continue
            key = (entry["test"], detail)
            if key in evidence:
                unaccounted.append(
                    f"{entry['id']}: DUPLICATE EVIDENCE with {evidence[key]}"
                )
            evidence[key] = entry["id"]
    finally:
        # H2: run-root deletion is checked too. `ignore_errors` would hide
        # residue, and residue is exactly what the check is for.
        try:
            shutil.rmtree(run_root)
        except OSError as error:
            CLEANUP_PROBLEMS.append(f"{run_root}: run root could not be removed: {error}")
        if os.path.exists(run_root):
            CLEANUP_PROBLEMS.append(f"{run_root}: run root still present after removal")
        leaked = registered_worktrees(repo) - baseline
        for path in sorted(leaked):
            CLEANUP_PROBLEMS.append(f"{path}: worktree registered by this run was never removed")

    if git("status", "--porcelain", cwd=repo) or git("rev-parse", "HEAD", cwd=repo) != candidate:
        print("GATE FAILED: the candidate worktree changed during the run")
        return 1
    print("candidate verified unchanged after the run")
    if CLEANUP_PROBLEMS:
        print(f"CLEANUP FAILED: {len(CLEANUP_PROBLEMS)} problem(s)")
        for problem in CLEANUP_PROBLEMS:
            print(f"  {problem}")
        return 1
    print(f"cleanup verified: {len(mutations)} mutation worktrees removed, run root absent")
    if unaccounted:
        print(f"{len(unaccounted)} unaccounted mutation(s):")
        for failure in unaccounted:
            print(f"  {failure}")
        return 1
    print(f"all {len(mutations)} {prefix} mutations accounted for, every one RED")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
