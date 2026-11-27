#!/usr/bin/env python3
"""Fail closed unless every selected DOGFOOD-PERMISSION mutant is witnessed red."""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


HERE = Path(__file__).resolve().parent
MANIFEST = HERE / "manifest.json"
PREFIXES = {"DGP": 6, "DGPR": 15}


def run(command, *, cwd, check=True):
    result = subprocess.run(
        command, cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and result.returncode != 0:
        detail = (result.stdout + result.stderr).strip()
        raise RuntimeError(f"{' '.join(command)} failed: {detail}")
    return result


def git(*args, cwd, check=True):
    result = run(["git", *args], cwd=cwd, check=check)
    return result.stdout.strip()


def whole_prefix(rows, prefix):
    if re.fullmatch(r"[A-Z]+", prefix) is None:
        raise ValueError(f"malformed mutation prefix {prefix!r}")
    pattern = re.compile(rf"^{re.escape(prefix)}[0-9]+$")
    return [row for row in rows if pattern.fullmatch(row["id"])]


def load_manifest(path):
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def allocation_problems(repo, prefix):
    problems = []
    for path in repo.rglob("manifest.json"):
        if "mutations" not in path.parts or path == MANIFEST:
            continue
        try:
            rows = load_manifest(path).get("mutations", [])
        except (OSError, ValueError, TypeError):
            continue
        pattern = re.compile(rf"^{re.escape(prefix)}[0-9]+$")
        stolen = [row.get("id") for row in rows if pattern.fullmatch(str(row.get("id", "")))]
        if stolen:
            problems.append(f"{path.relative_to(repo)} owns {stolen}")
    return problems


def preflight(rows, repo):
    problems = []
    for row in rows:
        target = repo / row["file"]
        source = target.read_text(encoding="utf-8").replace("\r\n", "\n")
        count = source.count(row["old"])
        if count != 1:
            problems.append(f"{row['id']}: target occurs {count} times, expected exactly once")
    return problems


def test_ran(output, test):
    return test in output and "running 1 test" in output


def windows_link_scan(workspace):
    if os.name != "nt":
        return []
    result = run(["cmd", "/c", "dir", "/a:l", str(workspace)], cwd=workspace, check=False)
    output = result.stdout + result.stderr
    return [line for line in output.splitlines() if "<JUNCTION>" in line or "<SYMLINKD>" in line]


def remove_worktree(repo, workspace):
    links = windows_link_scan(workspace)
    if links:
        raise RuntimeError(f"refusing worktree removal; link scan found: {links}")
    result = run(
        ["git", "worktree", "remove", "--force", str(workspace)],
        cwd=repo,
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stdout + result.stderr).strip()
        raise RuntimeError(f"worktree removal failed: {detail}")


def run_one(row, repo, candidate, run_root):
    workspace = run_root / row["id"]
    git("worktree", "add", "--detach", "--quiet", str(workspace), candidate, cwd=repo)
    try:
        target = workspace / row["file"]
        source = target.read_text(encoding="utf-8").replace("\r\n", "\n")
        if source.count(row["old"]) != 1:
            return False, "target ceased to occur exactly once"
        target.write_text(source.replace(row["old"], row["new"], 1), encoding="utf-8", newline="\n")
        command = [
            "cargo",
            "+1.97.1",
            "test",
            "-p",
            row.get("package", "grapt-permission"),
        ]
        features = row.get("features", [])
        if not isinstance(features, list) or any(
            not isinstance(feature, str) for feature in features
        ):
            return False, "row features must be a string list"
        if features:
            command.extend(["--features", ",".join(features)])
        if row.get("lib"):
            command.append("--lib")
        elif row.get("test_target"):
            command.extend(["--test", row["test_target"]])
        else:
            return False, "row names neither lib nor test_target"
        command.extend(["--no-fail-fast", "--", "--exact", row["test"]])
        result = run(command, cwd=workspace, check=False)
        output = result.stdout + result.stderr
        if not test_ran(output, row["test"]):
            return False, "no exact witness test ran"
        if result.returncode == 0:
            return False, "GREEN -- BROKEN WITNESS"
        if row["red"] not in output:
            return False, f"red lacked named evidence {row['red']!r}"
        return True, row["red"]
    finally:
        remove_worktree(repo, workspace)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--prefix", default="DGP")
    args = parser.parse_args()
    repo = Path(git("rev-parse", "--show-toplevel", cwd=HERE))
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: candidate worktree is not clean")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)
    manifest = load_manifest(MANIFEST)
    try:
        rows = whole_prefix(manifest["mutations"], args.prefix)
    except (KeyError, TypeError, ValueError) as error:
        print(f"GATE FAILED: {error}")
        return 1
    if args.prefix not in PREFIXES or not rows:
        print(f"GATE FAILED: exact allocated prefix must be one of {sorted(PREFIXES)}")
        return 1
    expected = [f"{args.prefix}{index:02d}" for index in range(1, PREFIXES[args.prefix] + 1)]
    if [row["id"] for row in rows] != expected:
        print(f"GATE FAILED: expected inventory {expected}")
        return 1
    stolen = allocation_problems(repo, args.prefix)
    if stolen:
        print("GATE FAILED: prefix allocation is not exclusive")
        for problem in stolen:
            print(f"  {problem}")
        return 1
    rotted = preflight(rows, repo)
    if rotted:
        print("PREFLIGHT FAILED")
        for problem in rotted:
            print(f"  {problem}")
        return 1
    print(f"candidate {candidate}; {len(rows)} exact-once {args.prefix} mutations")
    run_root = Path(tempfile.mkdtemp(prefix=f"grapt-permission-{args.prefix.lower()}-"))
    failures = []
    try:
        for row in rows:
            try:
                accounted, detail = run_one(row, repo, candidate, run_root)
            except (OSError, RuntimeError) as error:
                accounted, detail = False, str(error)
            label = "RED" if accounted else "UNACCOUNTED"
            print(f"{row['id']} {label}: {detail}")
            if not accounted:
                failures.append(row["id"])
    finally:
        shutil.rmtree(run_root, ignore_errors=True)
    if git("status", "--porcelain", cwd=repo) or git("rev-parse", "HEAD", cwd=repo) != candidate:
        print("GATE FAILED: candidate changed during the run")
        return 1
    if failures:
        print(f"GATE FAILED: unaccounted mutations {failures}")
        return 1
    print(f"all {len(rows)} {args.prefix} mutations accounted for, every one RED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
