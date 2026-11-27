#!/usr/bin/env python3
"""Fail-closed R4CA mutation runner using disposable exact-SHA worktrees."""

import json
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "manifest.json")
PANIC_PID = re.compile(r"\s\(\d+\)\s+panicked")
EXPECTED_IDS = tuple(f"R4CA{index:02d}" for index in range(1, 35))


def run(command, cwd, env=None):
    return subprocess.run(
        command, cwd=cwd, capture_output=True, text=True, check=False, env=env
    )


def git(arguments, cwd, required=True):
    result = run(["git", *arguments], cwd)
    if required and result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def tests_ran(output):
    total = 0
    for match in re.finditer(r"(\d+) passed; (\d+) failed", output):
        total += int(match.group(1)) + int(match.group(2))
    return total > 0


def detail(output):
    normal = PANIC_PID.sub(" panicked", output)
    lines = normal.splitlines()
    for index, line in enumerate(lines):
        if "panicked at" in line:
            following = lines[index + 1].strip() if index + 1 < len(lines) else ""
            return f"{line.strip()} {following}".strip()
    return next((line.strip() for line in lines if "assertion" in line), "no diagnostic")


def preflight(rows, repo):
    problems = []
    cache = {}
    for row in rows:
        path = os.path.join(repo, row["file"].replace("/", os.sep))
        if path not in cache:
            with open(path, "r", encoding="utf-8", newline="") as handle:
                cache[path] = handle.read().replace("\r\n", "\n")
        count = cache[path].count(row["old"])
        if count != 1:
            problems.append(f"{row['id']}: target occurs {count} times")
        if row["old"] == row["new"]:
            problems.append(f"{row['id']}: mutation changes nothing")
        if not row.get("test") or not row.get("test_target", "contracts"):
            problems.append(f"{row['id']}: exact witness target is absent")
    return problems


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def scan_junctions(workspace):
    if os.name != "nt":
        return
    result = run(["cmd", "/c", "dir", "/a:l", "/s", workspace], workspace)
    if result.returncode not in (0, 1):
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    if "<JUNCTION>" in result.stdout or "<SYMLINKD>" in result.stdout:
        raise RuntimeError(f"junction found inside disposable worktree {workspace}")


def mutate(row, repo, candidate, root):
    workspace = os.path.join(root, row["id"])
    git(["worktree", "add", "--detach", "--quiet", workspace, candidate], repo)
    try:
        path = os.path.join(workspace, row["file"].replace("/", os.sep))
        with open(path, "r", encoding="utf-8", newline="") as handle:
            source = handle.read().replace("\r\n", "\n")
        if source.count(row["old"]) != 1:
            return False, "target no longer occurs exactly once"
        with open(path, "w", encoding="utf-8", newline="") as handle:
            handle.write(source.replace(row["old"], row["new"], 1))
        environment = os.environ.copy()
        environment["CARGO_TARGET_DIR"] = os.path.join(workspace, "target-r4ca")
        result = run(
            [
                "cargo", "+1.97.1", "test", "-p", "grapt-cli",
                "--features", "loopback-testkit",
                "--test", row.get("test_target", "contracts"),
                "--no-fail-fast", "--", "--exact", row["test"],
            ],
            workspace,
            environment,
        )
        output = result.stdout + result.stderr
        if not tests_ran(output):
            return False, "no exact witness ran or the mutant did not compile"
        if result.returncode == 0:
            return False, "witness stayed green"
        red = f"test {row['test']} ... FAILED"
        if red not in output:
            return False, f"named red absent: {red}"
        return True, detail(output)
    finally:
        scan_junctions(workspace)
        git(["worktree", "remove", "--force", workspace], repo, required=False)


def main():
    repo = git(["rev-parse", "--show-toplevel"], HERE)
    if git(["status", "--porcelain"], repo):
        print("R4CA GATE FAILED: candidate worktree is not clean")
        return 1
    candidate = git(["rev-parse", "HEAD"], repo)
    with open(MANIFEST, "r", encoding="utf-8") as handle:
        rows = json.load(handle)["mutations"]
    ids = tuple(row["id"] for row in rows)
    if ids != EXPECTED_IDS:
        print(f"R4CA GATE FAILED: inventory {ids!r} != {EXPECTED_IDS!r}")
        return 1
    problems = preflight(rows, repo)
    if problems:
        print("R4CA PREFLIGHT FAILED")
        for problem in problems:
            print(f"  {problem}")
        return 1
    print(f"candidate {candidate}")
    print(f"preflight: all {len(rows)} targets occur exactly once")
    targets = sorted({row["file"] for row in rows})
    fingerprints = {
        target: sha256(os.path.join(repo, target.replace("/", os.sep)))
        for target in targets
    }
    root = tempfile.mkdtemp(prefix=f"grapt-r4ca-{os.getpid()}-")
    failures = []
    try:
        for row in rows:
            killed, why = mutate(row, repo, candidate, root)
            label = "RED" if killed else "FAILED"
            print(f"{row['id']} {label}: {why}")
            if not killed:
                failures.append(row["id"])
    finally:
        shutil.rmtree(root, ignore_errors=True)
    for target, before in fingerprints.items():
        after = sha256(os.path.join(repo, target.replace("/", os.sep)))
        if after != before:
            failures.append(f"{target}:target-residue")
    if git(["rev-parse", "HEAD"], repo) != candidate:
        failures.append("candidate-head-moved")
    if git(["status", "--porcelain"], repo):
        failures.append("candidate-worktree-dirty-after-run")
    residue = run(["rg", "-n", "MUTATED:", *targets], repo)
    if residue.returncode == 0:
        failures.append("mutation-marker-residue")
    elif residue.returncode not in (1,):
        failures.append("mutation-residue-scan-failed")
    if failures:
        print(f"R4CA GATE FAILED: {', '.join(failures)}")
        return 1
    print(f"R4CA GATE PASSED: {len(rows)}/{len(rows)} mutants RED; target residue zero")
    return 0


if __name__ == "__main__":
    sys.exit(main())
