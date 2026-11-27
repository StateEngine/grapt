#!/usr/bin/env python3
"""Run this manifest's witnesses in isolated worktrees.

The runner refuses dirty or moving candidates, checks every anchor exactly
once before compiling, proves every exact filter selects and runs exactly one
test, rejects compile kills and green mutants, and rejects duplicate evidence. Its inventory
counts live rows separately from tombstones while using both for ID uniqueness.

Two kinds of row exist. A test row (the default, and every H/Z/PR01-PR10 row)
resurrects a deleted behaviour and is proved by an exact test filter going red.
A `kind: rustdoc` row resurrects a documentation defect that no test can see:
its command is FIXED by this runner, never taken from the manifest, and its red
is cargo exit 101 carrying that row's exact diagnostic under the governed
warning-to-error environment. An arbitrary nonzero exit, a compile kill, an
extra rustdoc error or a different diagnostic is a refusal, never a kill.

With no argument the runner executes every live row, exactly as before.
`--prefix P` executes exactly the rows whose WHOLE alphabetic prefix is P; a
malformed, unowned or empty selection refuses before any worktree is created.
`--pack NAME` executes the exact ordered ids declared by that manifest pack;
unknown, duplicate, missing or malformed membership refuses before mutation.

The blocking-send mutation is different: its truthful RED is a timeout
rather than an assertion. The runner
first lists exactly one selected test, then starts cargo in a dedicated POSIX
process group and kills that exact group at the row's declared bound. An
assertion failure cannot satisfy a timeout row and a timeout cannot satisfy an
ordinary row.

Every cargo child this runner starts is dispatched through `run_cargo`, which
is the only seam allowed to create one. A disposable worktree must never build
into an ambient or shared target directory. The Linux heavy gate exports
`CARGO_TARGET_DIR` for every route (`remote-compute/secode-linux.json`,
`cache.env`), so an inherited target would be shared by every mutant in the
battery: one mutant's artifacts would then decide the next mutant's verdict and
could forge both a green and this manifest's expected RED. Each row therefore
builds into its own deterministic directory under the run root, the ambient
variables are stripped from the child environment rather than merely overridden,
and a bounded recursive path/type/size fingerprint before and after the battery
proves the ambient directory received nothing.
"""

import json
import os
import re
import signal
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "manifest.json")

# The rustdoc route is fixed here on purpose. A manifest row names WHICH
# documentation defect it resurrects and WHAT diagnostic must name it; it can
# never name the command, the toolchain or the flags, because a row that could
# choose its own command could also choose one that passes.
RUSTDOC_PACKAGE = "grapt-provider"
RUSTDOC_COMMAND = (
    "cargo",
    "+1.97.1",
    "doc",
    "-p",
    RUSTDOC_PACKAGE,
    "--all-features",
    "--no-deps",
)
RUSTDOC_FLAGS = "-D warnings"
RUSTDOC_LINT_NOTE = "`-D rustdoc::private-intra-doc-links` implied by `-D warnings`"
RUSTDOC_ERROR_PREFIX = "error: public documentation for "
RUSTDOC_TAIL_ERROR = "error: could not document `" + RUSTDOC_PACKAGE + "`"
RUSTDOC_EXPECTED_EXIT = 101
OWNED_PREFIXES = ("H", "PR", "Z")

# Both names must be removed, not just overridden. `CARGO_TARGET_DIR` is the
# direct override; `CARGO_BUILD_TARGET_DIR` is the environment form of the
# `build.target-dir` config key. Leaving either in the child environment would
# leave a second, ambient answer to the question "where do artifacts go" that a
# nested cargo invocation could still read.
AMBIENT_TARGET_VARS = ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR")
FINGERPRINT_ENTRY_LIMIT = 1_000_000


class GateFailure(Exception):
    """A refusal that stops the battery rather than producing a verdict."""


class CargoTimedOut(Exception):
    """One deliberately timed row exceeded its declared execution bound."""

    def __init__(self, seconds, stdout, stderr):
        super().__init__(f"cargo exceeded {seconds} seconds")
        self.seconds = seconds
        self.stdout = stdout
        self.stderr = stderr


def contained(root, path):
    root_real = os.path.realpath(root)
    path_real = os.path.realpath(path)
    return os.path.normcase(path_real) == os.path.normcase(root_real) or (
        os.path.normcase(path_real).startswith(
            os.path.normcase(root_real + os.sep)
        )
    )


def target_dir_for(run_root, mutation_id):
    """This row's own target directory: deterministic in the row id.

    Deterministic and per-row are both load-bearing. Per-row is what stops one
    mutant's artifacts reaching the next one. Deterministic in the id is what
    lets a reader of the receipt name the exact directory a row built into
    instead of a directory that only existed inside one process.
    """
    if not isinstance(mutation_id, str) or not re.fullmatch(
        r"[A-Za-z]+[0-9]+", mutation_id
    ):
        raise GateFailure(f"row id {mutation_id!r} cannot name a target directory")
    directory = os.path.join(run_root, "targets", mutation_id)
    if not contained(run_root, directory):
        raise GateFailure(f"target directory escaped the run root: {directory}")
    return directory


def run_cargo(command, cwd, target_dir, environment_updates=None, timeout_seconds=None):
    """The only seam that starts a cargo child.

    Both the flag and the environment are set. The flag settles THIS
    invocation ahead of any `.cargo/config.toml` in the tree; the environment
    settles any cargo a build script or a test starts underneath it.
    """
    if not command or command[0] != "cargo":
        raise GateFailure(f"non-cargo command dispatched through the cargo seam: {command!r}")
    if "--target-dir" in command:
        raise GateFailure("a caller named its own --target-dir; the seam owns it")
    argv = list(command)
    separator = argv.index("--") if "--" in argv else len(argv)
    argv[separator:separator] = ["--target-dir", target_dir]
    environment = dict(os.environ)
    for name in AMBIENT_TARGET_VARS:
        environment.pop(name, None)
    environment["CARGO_TARGET_DIR"] = target_dir
    if environment_updates:
        forbidden = sorted(set(environment_updates).intersection(AMBIENT_TARGET_VARS))
        if forbidden:
            raise GateFailure(
                f"a caller tried to override cargo target custody: {forbidden}"
            )
        environment.update(environment_updates)
    if timeout_seconds is None:
        return subprocess.run(
            argv, cwd=cwd, capture_output=True, text=True, check=False, env=environment
        )
    if os.name != "posix":
        raise GateFailure("timed mutation rows run only on the governed Linux target")
    process = subprocess.Popen(
        argv,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        stdout, stderr = process.communicate()
        raise CargoTimedOut(timeout_seconds, stdout, stderr) from None
    return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)


def directory_fingerprint(path):
    """Bounded recursive path/type/size census of a target directory."""
    try:
        fingerprint = []
        pending = [("", path)]
        while pending:
            relative_root, absolute_root = pending.pop()
            with os.scandir(absolute_root) as scan:
                entries = sorted(scan, key=lambda entry: entry.name)
            for entry in entries:
                relative = os.path.join(relative_root, entry.name).replace(os.sep, "/")
                metadata = entry.stat(follow_symlinks=False)
                if entry.is_symlink():
                    kind = "symlink"
                elif entry.is_dir(follow_symlinks=False):
                    kind = "directory"
                else:
                    kind = "file"
                fingerprint.append((kind, relative, metadata.st_size))
                if len(fingerprint) > FINGERPRINT_ENTRY_LIMIT:
                    raise GateFailure(
                        f"target fingerprint exceeds {FINGERPRINT_ENTRY_LIMIT} entries: {path}"
                    )
                if kind == "directory":
                    pending.append((relative, entry.path))
    except FileNotFoundError:
        return ("absent",)
    except OSError as error:
        return ("unreadable", str(error))
    return ("present", tuple(sorted(fingerprint)))


def ambient_targets():
    return {name: os.environ[name] for name in AMBIENT_TARGET_VARS if name in os.environ}


def ambient_fingerprints(ambient):
    return {name: directory_fingerprint(value) for name, value in ambient.items()}


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def mutation_manifests(repo):
    paths = []
    for root, dirs, files in os.walk(repo):
        dirs[:] = [name for name in dirs if name not in {".git", "target"}]
        if "manifest.json" not in files:
            continue
        path = os.path.join(root, "manifest.json")
        try:
            with open(path, "r", encoding="utf-8") as handle:
                document = json.load(handle)
        except (OSError, json.JSONDecodeError):
            continue
        if isinstance(document.get("mutations"), list):
            paths.append((path, document))
    return sorted(paths, key=lambda item: item[0])


def inventory(repo):
    """Manifest-local IDs include tombstones; global live counts exclude them.

    Historical manifests intentionally reuse local IDs such as M01. Prefix
    allocation is the global boundary, so every prefix this manifest owns must
    be absent everywhere outside it, while each manifest checks its own
    live/tombstone namespace together.

    This manifest owns the whole prefixes `H`, `PR` and `Z`. Each must be
    unique across other manifests. Compare complete alphabetic prefixes:
    `PR` must never collide with `P` by first character alone.
    """
    owned_prefixes = OWNED_PREFIXES
    live = 0
    retired = 0
    qualified = 0
    problems = []
    seen_prefixes = set()
    for path, document in mutation_manifests(repo):
        relative = os.path.relpath(path, repo).replace(os.sep, "/")
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
                match = re.fullmatch(r"([A-Za-z]+)[0-9]+", mutation_id)
                if relative == "crates/grapt-provider/mutations/manifest.json":
                    if match is None or match.group(1) not in owned_prefixes:
                        problems.append(
                            f"provider row {mutation_id} does not use a whole prefix "
                            f"this manifest owns {owned_prefixes}"
                        )
                    else:
                        seen_prefixes.add(match.group(1))
                elif match is not None and match.group(1) in owned_prefixes:
                    problems.append(
                        f"whole prefix {match.group(1)} is already used outside provider: "
                        f"{relative}"
                    )
    for prefix in owned_prefixes:
        if prefix not in seen_prefixes:
            problems.append(f"provider manifest has no {prefix}-prefixed row")
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


def test_count(output):
    total = 0
    for match in re.finditer(r"(\d+) passed; (\d+) failed", output):
        total += int(match.group(1)) + int(match.group(2))
    return total


def listed_test_count(output):
    return sum(1 for line in output.splitlines() if line.strip().endswith(": test"))


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


def prefix_of(mutation_id):
    match = re.fullmatch(r"([A-Za-z]+)[0-9]+", mutation_id)
    return match.group(1) if match else None


def select_prefix(mutations, prefix):
    """Return the rows whose WHOLE prefix is `prefix`, or a refusal string.

    Whole-prefix comparison is the same rule the inventory uses: `P` must not
    reach the `PR` rows, and `PR` must not reach a future `P` row.
    """
    if prefix is None:
        return mutations, None
    if not re.fullmatch(r"[A-Za-z]+", prefix):
        return None, f"malformed prefix {prefix!r}: expected one or more letters"
    if prefix not in OWNED_PREFIXES:
        return None, (
            f"prefix {prefix!r} is not owned by this manifest {OWNED_PREFIXES}"
        )
    chosen = [row for row in mutations if prefix_of(row.get("id", "")) == prefix]
    if not chosen:
        return None, f"prefix {prefix!r} selects no live row in this manifest"
    return chosen, None


def select_pack(mutations, packs, name):
    """Return one manifest-declared ordered pack, or a refusal string."""
    ids = packs.get(name)
    if ids is None:
        return None, f"pack {name!r} is not declared by this manifest"
    if not isinstance(ids, list) or not ids:
        return None, f"pack {name!r} must declare at least one mutation id"
    if any(not isinstance(mutation_id, str) for mutation_id in ids):
        return None, f"pack {name!r} contains a non-string mutation id"
    if len(set(ids)) != len(ids):
        return None, f"pack {name!r} contains duplicate mutation ids"
    by_id = {row.get("id"): row for row in mutations}
    missing = [mutation_id for mutation_id in ids if mutation_id not in by_id]
    if missing:
        return None, f"pack {name!r} names missing mutation ids: {missing}"
    return [by_id[mutation_id] for mutation_id in ids], None


def parse_selection(argv):
    if not argv:
        return None, None
    if argv[0] not in ("--prefix", "--pack"):
        return None, f"unknown argument {argv[0]!r}: expected --prefix PREFIX or --pack NAME"
    if len(argv) != 2:
        return None, f"{argv[0]} takes exactly one value"
    return (argv[0][2:], argv[1]), None


def run_rustdoc(entry, workspace, target_dir):
    """A rustdoc row is red only for its own exact diagnostic."""
    if entry.get("package") != RUSTDOC_PACKAGE:
        return False, "ROW OUTSIDE THE FIXED RUSTDOC ROUTE", str(entry.get("package"))
    expected = entry.get("expect_error")
    if not isinstance(expected, str) or not expected.startswith(RUSTDOC_ERROR_PREFIX):
        return False, "ROW NAMES NO EXACT DIAGNOSTIC", str(expected)

    result = run_cargo(
        list(RUSTDOC_COMMAND),
        workspace,
        target_dir,
        {"RUSTDOCFLAGS": RUSTDOC_FLAGS},
    )
    output = result.stdout + result.stderr
    lines = [line.rstrip() for line in output.splitlines()]
    documentation_errors = [
        line for line in lines if line.startswith(RUSTDOC_ERROR_PREFIX)
    ]
    other_errors = [
        line
        for line in lines
        if line.startswith("error")
        and not line.startswith(RUSTDOC_ERROR_PREFIX)
        and line != RUSTDOC_TAIL_ERROR
    ]

    if other_errors:
        return False, "NOT A DOC WITNESS (other error)", other_errors[0][:240]
    if result.returncode != RUSTDOC_EXPECTED_EXIT:
        return (
            False,
            f"WRONG EXIT {result.returncode} (expected {RUSTDOC_EXPECTED_EXIT})",
            output.strip()[-240:],
        )
    if len(documentation_errors) != 1:
        return (
            False,
            f"EXPECTED EXACTLY ONE DOC-LINK ERROR, SAW {len(documentation_errors)}",
            "; ".join(documentation_errors)[:240],
        )
    if documentation_errors[0] != expected:
        return False, "WRONG DIAGNOSTIC", documentation_errors[0][:240]
    if RUSTDOC_LINT_NOTE not in output:
        return False, "THE GOVERNED LINT IS NOT NAMED", RUSTDOC_LINT_NOTE
    return True, "RED (rustdoc witness fired)", expected


def run_one(entry, repo, candidate, run_root):
    workspace = os.path.join(run_root, entry["id"])
    target_dir = target_dir_for(run_root, entry["id"])
    if os.path.exists(target_dir):
        raise GateFailure(
            f"{entry['id']}: its target directory already exists before the row ran: "
            f"{target_dir}"
        )
    git("worktree", "add", "--detach", "--quiet", workspace, candidate, cwd=repo)
    try:
        target = os.path.join(workspace, entry["file"].replace("/", os.sep))
        with open(target, "r", encoding="utf-8", newline="") as handle:
            source = handle.read().replace("\r\n", "\n")
        if source.count(entry["old"]) != 1:
            return False, "ANCHOR CHANGED AFTER PREFLIGHT", entry["old"][:60]
        with open(target, "w", encoding="utf-8", newline="") as handle:
            handle.write(source.replace(entry["old"], entry["new"], 1))

        if entry.get("kind") == "rustdoc":
            return run_rustdoc(entry, workspace, target_dir)

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
        expected = entry.get("expected", "assertion-red")
        if expected == "timeout":
            listing = run_cargo(command + ["--list"], workspace, target_dir)
            listing_output = listing.stdout + listing.stderr
            if listing.returncode != 0:
                return False, "COMPILE KILL (not a witness)", listing_output.strip()[-240:]
            selected = listed_test_count(listing_output)
            if selected != 1:
                return False, f"EXPECTED EXACTLY ONE TEST, LISTED {selected}", entry["test"]
        timed_out = None
        try:
            result = run_cargo(
                command,
                workspace,
                target_dir,
                timeout_seconds=entry.get("timeout_seconds"),
            )
        except CargoTimedOut as timeout:
            timed_out = timeout
            result = None
        output = "" if result is None else result.stdout + result.stderr

        # Where the artifacts landed is checked BEFORE the verdict is read. A
        # row that built anywhere but its own directory has no verdict to give:
        # its red could be the previous mutant's, and its green could be too.
        leaked = os.path.join(workspace, "target")
        if os.path.exists(leaked):
            return False, "TARGET LEAKED INTO THE WORKTREE", leaked
        if directory_fingerprint(target_dir) == ("absent",):
            return False, "ROW BUILT OUTSIDE ITS OWN TARGET DIRECTORY", target_dir

        if timed_out is not None:
            if expected != "timeout":
                return False, "UNEXPECTED TIMEOUT", f"exceeded {timed_out.seconds} seconds"
            return (
                True,
                "RED (TIMEOUT defect evidence)",
                f"exactly one test selected; exceeded {timed_out.seconds} seconds",
            )

        ran = test_count(output)
        if result.returncode != 0 and ran == 0:
            return False, "COMPILE KILL (not a witness)", output.strip()[-240:]
        if ran != 1:
            return False, f"EXPECTED EXACTLY ONE TEST, RAN {ran}", entry["test"]
        if expected == "timeout":
            return False, "ASSERTION RED, EXPECTED TIMEOUT", failure_detail(output)
        if result.returncode == 0:
            return False, "GREEN -- BROKEN WITNESS", "the witness missed its mutation"
        return True, "RED (witness fired)", failure_detail(output)
    finally:
        git("worktree", "remove", "--force", workspace, cwd=repo, check=False)
        if os.path.exists(target_dir):
            shutil.rmtree(target_dir)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    requested, argument_problem = parse_selection(argv)
    if argument_problem:
        print(f"SELECTION REFUSED: {argument_problem}")
        return 1
    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: the candidate worktree is not clean")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)
    with open(MANIFEST, "r", encoding="utf-8") as handle:
        manifest = json.load(handle)
    mutations = manifest["mutations"]
    packs = manifest.get("packs", {})

    print(f"candidate {candidate}")
    live, retired, qualified, inventory_problems = inventory(repo)
    print(
        f"inventory: {live} live mutations; {retired} tombstone(s); "
        f"{qualified} manifest-qualified ids"
    )
    if inventory_problems:
        for problem in inventory_problems:
            print(f"  {problem}")
        return 1

    # Selection refuses before any worktree exists, and before any anchor is
    # read: an unowned or empty selection must cost nothing.
    if requested is None:
        selection_kind = None
        selection_value = None
        selection_problem = None
    else:
        selection_kind, selection_value = requested
        if selection_kind == "prefix":
            mutations, selection_problem = select_prefix(mutations, selection_value)
        else:
            mutations, selection_problem = select_pack(mutations, packs, selection_value)
    if selection_problem:
        print(f"SELECTION REFUSED: {selection_problem}")
        return 1
    if requested is not None:
        print(
            f"selection: {selection_kind} {selection_value} -> "
            f"{len(mutations)} live row(s)"
        )

    rotted = preflight(mutations, repo)
    if rotted:
        print(f"PREFLIGHT FAILED: {len(rotted)} problem(s)")
        for problem in rotted:
            print(f"  {problem}")
        return 1
    print(f"preflight: all {len(mutations)} snippets present exactly once")

    run_root = tempfile.mkdtemp(prefix=f"grapt-provider-mutations-{os.getpid()}-")
    ambient = ambient_targets()
    before = ambient_fingerprints(ambient)
    print(f"run root {run_root}")
    if ambient:
        for name, value in sorted(ambient.items()):
            print(f"ambient {name}={value} (stripped from every cargo child)")
    else:
        print("ambient target: none set")
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
                continue
            # A rustdoc row is identified by the diagnostic it must produce,
            # exactly as a test row is identified by the filter it must fire.
            # Indexing `test` unconditionally crashed the run AFTER the first
            # rustdoc row had already gone red, which loses the receipt for
            # every row behind it.
            key = (entry.get("test") or entry.get("expect_error"), detail)
            if key in evidence:
                unaccounted.append(
                    f"{entry['id']}: DUPLICATE EVIDENCE with {evidence[key]}"
                )
            evidence[key] = entry["id"]
    finally:
        shutil.rmtree(run_root, ignore_errors=True)
        residue = os.path.exists(run_root)

    if residue:
        print(f"GATE FAILED: the run root survived cleanup: {run_root}")
        return 1

    # Custody, stated positively: the ambient target directory is exactly as
    # this run found it. An unreadable or absent ambient stays unreadable or
    # absent -- creating it is a write like any other.
    after = ambient_fingerprints(ambient)
    disturbed = sorted(name for name in ambient if before[name] != after[name])
    if disturbed:
        print("GATE FAILED: the ambient target directory was written during the run")
        for name in disturbed:
            print(f"  {name}={ambient[name]}: {before[name]} -> {after[name]}")
        return 1
    if ambient:
        print(
            "ambient target unchanged across the battery "
            f"({', '.join(sorted(ambient))}): zero writes"
        )

    if git("status", "--porcelain", cwd=repo) or git("rev-parse", "HEAD", cwd=repo) != candidate:
        print("GATE FAILED: the candidate worktree changed during the run")
        return 1
    print("candidate verified unchanged after the run")
    if unaccounted:
        print(f"{len(unaccounted)} unaccounted mutation(s):")
        for failure in unaccounted:
            print(f"  {failure}")
        return 1
    # Report separate counts for each whole prefix so the summary cannot
    # attribute every mutation to just one of the prefixes this manifest owns.
    counts = {}
    for entry in mutations:
        match = re.fullmatch(r"([A-Za-z]+)[0-9]+", entry["id"])
        prefix = match.group(1) if match else "?"
        counts[prefix] = counts.get(prefix, 0) + 1
    split = ", ".join(f"{prefix}x{count}" for prefix, count in sorted(counts.items()))
    scope = "mutations" if requested is None else f"selected {requested} mutations"
    print(f"all {len(mutations)} {scope} accounted for ({split}), every one RED")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except GateFailure as failure:
        print(f"GATE FAILED: {failure}")
        sys.exit(1)
