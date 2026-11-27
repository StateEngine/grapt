#!/usr/bin/env python3
"""Run PLR mutation witnesses with source, executable and receipt custody.

Each mutant lives in a disposable worktree at the exact candidate commit.
The package is cleaned and built in an isolated target, then the exact test
executable named by Cargo compiler-artifact JSON is run directly. OS source
events reject edits even when they are restored before final Git status.
Every intended red also runs green at the unmutated candidate. No passing
summary is printed until a versioned receipt is written and read back.
"""

import argparse
from contextlib import ExitStack
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from custody.receipt import (  # noqa: E402
    SCHEMA, CustodyError, candidate_identity, harness_identity, read_receipt,
    read_manifest, require_clean, resolve_artifact, sha256_file, write_receipt,
)
from custody.source_watch import SourceChanged, SourceWatch  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "mutation-manifest.json")

# `thread 'name' (12345) panicked at` - the parenthesised thread PID varies
# run to run, so it is removed before any evidence comparison.
PID_IN_PANIC = re.compile(r"\s\(\d+\)\s+panicked")

# Containment unit names embed the spawning test process's PID
# (secode-plugin-<pid>-<gen>-<seq>), so the same witness failure carries a
# different unit name on every run; token it so expected diagnostics and
# the duplicate-evidence guard compare the law, not the run.
PID_IN_UNIT_NAME = re.compile(r"secode-plugin-\d+-\d+-\d+")


def normalise_evidence(text):
    """Strip the per-run PIDs so two identical failures compare equal."""
    text = PID_IN_PANIC.sub(" panicked", text)
    return PID_IN_UNIT_NAME.sub("secode-plugin-UNIT", text)


def git(*args, cwd, check=True):
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def failure_detail(output):
    """The line that says WHICH assertion fired, not merely where it lives."""
    output = normalise_evidence(output)
    lines = output.splitlines()
    for index, line in enumerate(lines):
        if "panicked at" in line:
            location = line.strip()
            message = lines[index + 1].strip() if index + 1 < len(lines) else ""
            return f"{location} {message}".strip()
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("Error:"):
            # A Result-returning witness prints `Error: "..."` instead of
            # panicking; that line is the diagnostic, not its absence.
            return stripped
    return next(
        (line.strip() for line in lines if "assertion" in line),
        "failed without a captured diagnostic",
    )


def preflight(mutations, repo):
    """Every snippet present exactly once, and actually a change."""
    problems = []
    cache = {}
    for entry in mutations:
        path = os.path.join(repo, *entry["file"].split("/"))
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
        expected = entry.get("expected_diagnostic")
        if not isinstance(expected, str) or not expected.strip():
            problems.append(
                f"{entry['id']}: expected_diagnostic is missing or empty - "
                "the intended red cannot be recognised"
            )
    return problems


def cargo_in_workspace(workspace, args, target_dir):
    """cargo in a worktree pinned to the battery's dedicated target dir."""
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = target_dir
    return subprocess.run(
        ["cargo", *args],
        cwd=workspace,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def compiled_witness(entry, workspace, target_dir, clean=True):
    """Build and resolve the exact test executable from Cargo JSON.

    Cleaning the package first forbids an old package binary from becoming a
    fresh compiler-artifact claim through a preserved incremental target dir.
    Dependencies still use the isolated battery target cache.
    """
    if clean:
        cleaned = cargo_in_workspace(
            workspace, ["clean", "-p", entry["package"]], target_dir
        )
        if cleaned.returncode:
            raise CustodyError(f"{entry['id']}: package clean failed: {cleaned.stderr[-200:]}")
    target_args = (["--lib"] if entry["test_target"] == "lib"
                   else ["--test", entry["test_target"]])
    command = [
        "cargo", "test", "-p", entry["package"], *target_args,
        "--no-run", "--no-fail-fast", "--message-format=json",
    ]
    result = cargo_in_workspace(workspace, command[1:], target_dir)
    if result.returncode:
        raise CustodyError(f"{entry['id']}: build failed: {result.stderr[-300:]}")
    # A crate's folded integration binary is built from its tests/integration/main.rs.
    target_source = {"lib": "src/lib.rs", "integration": "tests/integration/main.rs"}
    source = (Path(workspace) / "crates" / entry["package"] /
              target_source.get(entry["test_target"], f"tests/{entry['test_target']}.rs"))
    artifact = resolve_artifact(result.stdout, source, target_dir)
    return artifact, {"command": command, "exit": result.returncode,
                      "output_sha256": hashlib.sha256(
                          (result.stdout + result.stderr).encode()).hexdigest()}


def execute_witness(entry, workspace, artifact, target_dir, execution_budget):
    command = [artifact["path"], "--exact", entry["test"], "--nocapture"]
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = target_dir
    if execution_budget[0] <= 0:
        raise CustodyError("20-minute witness execution budget exhausted")
    started = time.monotonic()
    try:
        result = subprocess.run(
            command, cwd=os.path.join(workspace, "crates", entry["package"]),
            env=env, capture_output=True, text=True,
            check=False, timeout=execution_budget[0],
        )
    except subprocess.TimeoutExpired as error:
        raise CustodyError("20-minute witness execution budget exhausted") from error
    finally:
        execution_budget[0] -= time.monotonic() - started
    output = result.stdout + result.stderr
    counts = [tuple(map(int, match)) for match in
              re.findall(r"(\d+) passed; (\d+) failed", output)]
    tests = sum(passed + failed for passed, failed in counts)
    record = {"command": command, "exit": result.returncode, "tests": tests,
              "duration_seconds": round(time.monotonic() - started, 3),
              "output_sha256": hashlib.sha256(output.encode()).hexdigest()}
    return record, output


def assert_no_links(root):
    """Refuse Windows reparse points before any recursive worktree removal."""
    root = Path(root).absolute()
    if os.name != "nt":
        return
    root_attributes = getattr(os.stat(root, follow_symlinks=False),
                              "st_file_attributes", 0)
    if root_attributes & 0x400:
        raise CustodyError(f"refusing to remove junction root: {root}")
    for directory, dirs, files in os.walk(root):
        for name in dirs + files:
            path = Path(directory) / name
            attributes = getattr(os.stat(path, follow_symlinks=False),
                                 "st_file_attributes", 0)
            if attributes & 0x400:  # FILE_ATTRIBUTE_REPARSE_POINT
                raise CustodyError(f"refusing to remove worktree with link: {path}")


def run_one(entry, repo, candidate, run_root, target_dir,
            green_workspace, green_target, green_cache, execution_budget):
    workspace = os.path.join(run_root, entry["id"])
    git("worktree", "add", "--detach", "--quiet", workspace, candidate, cwd=repo)
    try:
        target = os.path.join(workspace, *entry["file"].split("/"))
        with open(target, "r", encoding="utf-8", newline="") as handle:
            source = handle.read()
        source = source.replace("\r\n", "\n")
        # Asserted again HERE, not only in preflight: an unapplied mutant is
        # indistinguishable from a survivor, so the count is proved
        # immediately before the write rather than inferred from an earlier
        # pass.
        if entry["old"] not in source:
            return None, "SNIPPET NOT FOUND", entry["old"][:60]
        if source.count(entry["old"]) != 1:
            return None, "SNIPPET AMBIGUOUS", entry["old"][:60]
        with open(target, "w", encoding="utf-8", newline="") as handle:
            handle.write(source.replace(entry["old"], entry["new"], 1))
        with SourceWatch(workspace) as source_watch:
            artifact, build = compiled_witness(entry, workspace, target_dir)
            red, output = execute_witness(
                entry, workspace, artifact, target_dir, execution_budget
            )
        source_watch.assert_unchanged()
        artifact["sha256_after"] = sha256_file(artifact["path"])

        if red["tests"] != 1:
            return None, "NO EXACT TEST RAN", f"observed {red['tests']} tests"
        if red["exit"] == 0:
            return None, "GREEN -- BROKEN WITNESS", "mutation survived"
        detail = failure_detail(output)
        expected = entry["expected_diagnostic"]
        if expected not in detail:
            # The witness fired on SOMETHING, but not on the law this row
            # exists to guard: a different assertion or an infrastructure
            # fault, so the red attributes nothing.
            return (
                None,
                "DIAGNOSTIC MISMATCH (infrastructure red)",
                f"the red does not carry the intended diagnostic {expected!r}: "
                f"{detail[:200]}",
            )

        key = entry["test_target"]
        if key not in green_cache:
            green_cache[key] = compiled_witness(
                entry, green_workspace, green_target
            )
        green_artifact, green_build = green_cache[key]
        green, green_output = execute_witness(
            entry, green_workspace, green_artifact, green_target, execution_budget
        )
        green_artifact["sha256_after"] = sha256_file(green_artifact["path"])
        if green["exit"] != 0 or green["tests"] != 1:
            return None, "RESTORED GREEN FAILED", failure_detail(green_output)
        green["artifact"] = green_artifact
        green["build"] = green_build
        record = {
            "id": entry["id"], "expected_diagnostic": expected,
            "diagnostic": detail, "source_changed": False,
            "mutant_file": entry["file"],
            "mutant_sha256": sha256_file(target),
            "artifact": artifact, "build": build, "red": red, "green": green,
        }
        return record, "RED / RESTORED GREEN", detail
    finally:
        assert_no_links(workspace)
        git("worktree", "remove", "--force", workspace, cwd=repo)


PREFIX_TOKEN = re.compile(r"[A-Z]+")


def select_whole_prefix(mutations, prefix):
    """Rows whose id is exactly this whole alpha token plus decimal digits."""
    if PREFIX_TOKEN.fullmatch(prefix) is None:
        raise ValueError("--prefix must contain uppercase ASCII letters only")
    whole_prefix = re.compile(rf"{re.escape(prefix)}[0-9]+")
    return [
        entry for entry in mutations if whole_prefix.fullmatch(entry["id"]) is not None
    ]


def main():
    parser = argparse.ArgumentParser(
        description="Run every PLR mutation, or one exact whole-prefix allocation."
    )
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument(
        "--prefix",
        help="run only ids formed from this uppercase whole prefix plus decimal digits",
    )
    selection.add_argument("--id", help="run one exact manifest id for a smoke")
    parser.add_argument("--receipt", required=True,
                        help="new durable custody receipt path outside the source root")
    parser.add_argument("--manifest", default=MANIFEST,
                        help="mutation manifest whose rows all carry its own prefix "
                             "(default: the PLR manifest)")
    args = parser.parse_args()
    manifest_path = Path(args.manifest).resolve()
    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    receipt_path = Path(args.receipt).resolve()
    if receipt_path.is_relative_to(Path(repo).resolve()):
        print("GATE FAILED: receipt must be outside the source root")
        return 1
    if receipt_path.exists():
        print("GATE FAILED: receipt path already exists; preserve the prior evidence")
        return 1
    before = candidate_identity(repo)
    try:
        require_clean(before, "candidate before")
    except CustodyError as error:
        print(f"GATE FAILED: {error}")
        return 1
    candidate = before["commit"]
    harness_paths = [Path(__file__).resolve(), manifest_path,
                     Path(__file__).resolve().parents[1] / "custody/receipt.py",
                     Path(__file__).resolve().parents[1] / "custody/source_watch.py",
                     Path(__file__).resolve().parents[1] / "custody/verify.py"]
    harness_before = harness_identity(harness_paths)

    with open(manifest_path, "r", encoding="utf-8") as handle:
        manifest = json.load(handle)
    mutations = manifest["mutations"]
    foreign = [entry["id"] for entry in mutations
               if entry["id"] not in {row["id"] for row in
                                      select_whole_prefix([entry], manifest["prefix"])}]
    if foreign:
        print(f"GATE FAILED: {manifest_path.name} rows outside prefix "
              f"{manifest['prefix']}: {foreign}")
        return 1
    if args.id is not None:
        mutations = [entry for entry in mutations if entry["id"] == args.id]
        if len(mutations) != 1:
            print(f"GATE FAILED: exact id {args.id!r} has {len(mutations)} rows")
            return 1
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
    print(f"candidate tree {before['tree']}")

    rotted = preflight(mutations, repo)
    if rotted:
        print(
            f"\nPREFLIGHT FAILED: {len(rotted)} entry/entries do not match the candidate:"
        )
        for problem in rotted:
            print(f"  {problem}")
        print(
            "\nA rotted entry witnesses nothing. Re-pin it against the current "
            "source rather than deleting it."
        )
        return 1
    print(f"preflight: all {len(mutations)} snippets present exactly once")
    print(f"{len(mutations)} mutations, each in its own disposable worktree\n")

    run_root = tempfile.mkdtemp(prefix=f"grapt-plr-mutations-{os.getpid()}-")
    target_dir = os.path.join(run_root, "mutant-target")
    green_target = os.path.join(run_root, "green-target")
    green_workspace = os.path.join(run_root, "green")
    os.makedirs(target_dir)
    os.makedirs(green_target)
    print(f"dedicated CARGO_TARGET_DIR {target_dir}")
    unaccounted = []
    evidence = {}
    records = []
    green_cache = {}
    execution_budget = [1200.0]
    source_changed = False
    try:
        git("worktree", "add", "--detach", "--quiet", green_workspace,
            candidate, cwd=repo)
        with ExitStack() as stack:
            candidate_watch = stack.enter_context(SourceWatch(repo))
            green_watch = stack.enter_context(SourceWatch(green_workspace))
            for entry in mutations:
                record, label, detail = run_one(
                    entry, repo, candidate, run_root, target_dir,
                    green_workspace, green_target, green_cache, execution_budget,
                )
                print(f"  {entry['id']}  {label}")
                if detail:
                    print(f"      {detail}")
                if record is None:
                    unaccounted.append(f"{entry['id']}: {label}")
                else:
                    key = (entry["test"], detail)
                    if key in evidence:
                        unaccounted.append(
                            f"{entry['id']}: DUPLICATE EVIDENCE with {evidence[key]}"
                        )
                    evidence[key] = entry["id"]
                    records.append(record)
                candidate_watch.assert_unchanged()
                green_watch.assert_unchanged()
        source_changed = (candidate_watch.changed.is_set()
                          or green_watch.changed.is_set())
        candidate_watch.assert_unchanged()
        green_watch.assert_unchanged()
    except (CustodyError, SourceChanged) as error:
        print(f"GATE FAILED: {error}")
        return 1
    finally:
        if Path(green_workspace).exists():
            assert_no_links(green_workspace)
            git("worktree", "remove", "--force", green_workspace, cwd=repo)
        if not any((Path(run_root) / entry["id"]).exists() for entry in mutations):
            if not Path(run_root).resolve().is_relative_to(
                Path(tempfile.gettempdir()).resolve()
            ) or not Path(run_root).name.startswith("grapt-plr-mutations-"):
                raise CustodyError("refusing cleanup outside the owned temporary root")
            assert_no_links(run_root)
            shutil.rmtree(run_root)

    print()
    after = candidate_identity(repo)
    harness_after = harness_identity(harness_paths)
    if after != before or harness_after != harness_before or source_changed:
        print("GATE FAILED: candidate or harness changed during the run")
        return 1
    print("candidate verified unchanged after the run")

    if unaccounted:
        print(f"\n{len(unaccounted)} unaccounted mutation(s):")
        for failure in unaccounted:
            print(f"  {failure}")
        return 1
    receipt = {
        "schema": SCHEMA, "outcome": "accepted",
        "candidate": {"before": before, "after": after},
        "harness": {"before": harness_before, "after": harness_after},
        "source_changed": source_changed, "entries": records,
    }
    try:
        pinned = read_manifest(manifest_path)
        digest = write_receipt(receipt_path, receipt,
                               [entry["id"] for entry in mutations], pinned)
        read_receipt(receipt_path, [entry["id"] for entry in mutations], pinned)
    except CustodyError as error:
        print(f"GATE FAILED: {error}")
        return 1
    print(f"custody receipt {receipt_path} sha256 {digest}")
    print(f"all {len(mutations)} mutations accounted for, intended RED/restored-green")
    return 0


if __name__ == "__main__":
    sys.exit(main())
