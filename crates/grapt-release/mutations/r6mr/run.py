#!/usr/bin/env python3
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


def run(command, cwd, check=False, environment=None):
    return subprocess.run(
        command,
        cwd=cwd,
        capture_output=True,
        text=True,
        check=check,
        env=environment,
    )


def git(*arguments, cwd, check=True):
    result = run(["git", *arguments], cwd, check=False)
    if check and result.returncode != 0:
        raise RuntimeError((result.stdout + result.stderr).strip())
    return result.stdout.strip()


def contained(root, relative):
    ceiling = os.path.realpath(root)
    target = os.path.realpath(os.path.join(ceiling, relative.replace("/", os.sep)))
    if target == ceiling or not target.startswith(ceiling + os.sep):
        return None
    return target


def normalized_text(path):
    with open(path, "r", encoding="utf-8", newline="") as handle:
        return handle.read().replace("\r\n", "\n")


def preflight(repo, mutations):
    problems = []
    law_pack = current_law_pack(repo)
    if not os.path.isfile(law_pack):
        problems.append(f"current law-pack evidence is absent: {law_pack}")
    ids = [entry["id"] for entry in mutations]
    expected = [f"R6MR{value:02d}" for value in range(1, len(ids) + 1)]
    if ids != expected:
        problems.append(f"ids are not the contiguous reviewed set: {ids!r}")
    for entry in mutations:
        if re.fullmatch(r"R6MR[0-9]{2}", entry["id"]) is None:
            problems.append(f"{entry['id']}: outside the exact R6MR allocation")
        path = contained(repo, entry["file"])
        if path is None:
            problems.append(f"{entry['id']}: path escapes candidate")
            continue
        source = normalized_text(path)
        count = source.count(entry["old"])
        if count != 1:
            problems.append(f"{entry['id']}: target occurs {count} times, required exactly once")
        if entry["old"] == entry["new"]:
            problems.append(f"{entry['id']}: mutation changes nothing")
    return problems


def current_law_pack(repo):
    explicit = os.environ.get("GRAPT_LAW_PACK")
    if explicit:
        return explicit
    fixtures = os.environ.get("SECODE_FIXTURES")
    if fixtures:
        return os.path.join(fixtures, "law-pack.json")
    artifact_root = os.environ.get("REMOTE_COMPUTE_ARTIFACT_ROOT")
    if artifact_root:
        return os.path.join(
            artifact_root, "fixtures", "rust-reconstruction", "law-pack.json"
        )
    return os.path.join(repo, "fixtures", "rust-reconstruction", "law-pack.json")


def ran_named_test(output, name):
    return f"test {name} ..." in output and re.search(r"[1-9][0-9]* failed", output) is not None


def mutate_one(repo, candidate, root, entry):
    worktree = os.path.join(root, entry["id"])
    git("worktree", "add", "--detach", "--quiet", worktree, candidate, cwd=repo)
    try:
        target = contained(worktree, entry["file"])
        if target is None:
            return False, "path escaped disposable worktree"
        source = normalized_text(target)
        if source.count(entry["old"]) != 1:
            return False, "target stopped occurring exactly once"
        mutated = source.replace(entry["old"], entry["new"], 1)
        with open(target, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(mutated)
        target = entry.get("target", "integration")
        target_arguments = ["--lib"] if target == "lib" else ["--test", target]
        environment = os.environ.copy()
        environment["GRAPT_LAW_PACK"] = current_law_pack(repo)
        result = run(
            [
                "cargo",
                "test",
                "-p",
                "grapt-release",
                *target_arguments,
                "--",
                "--exact",
                entry["test"],
            ],
            worktree,
            environment=environment,
        )
        output = result.stdout + result.stderr
        if result.returncode == 0:
            return False, "GREEN -- witness did not detect mutation"
        if not ran_named_test(output, entry["test"]):
            tail = " ".join(output.strip().splitlines()[-3:])
            return False, f"no named runtime witness ({tail})"
        detail = next((line.strip() for line in output.splitlines() if "assertion failed" in line or line.startswith("Error:")), "named test failed")
        return True, detail
    finally:
        git("worktree", "remove", "--force", worktree, cwd=repo, check=False)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--prefix", default="R6MR")
    arguments = parser.parse_args()
    if arguments.prefix != "R6MR":
        print("GATE FAILED: only the exact whole prefix R6MR is admitted")
        return 1
    repo = git("rev-parse", "--show-toplevel", cwd=HERE)
    if git("status", "--porcelain", cwd=repo):
        print("GATE FAILED: candidate worktree is not clean")
        return 1
    candidate = git("rev-parse", "HEAD", cwd=repo)
    with open(MANIFEST, "r", encoding="utf-8") as handle:
        mutations = json.load(handle)["mutations"]
    problems = preflight(repo, mutations)
    if problems:
        for problem in problems:
            print(f"GATE FAILED: {problem}")
        return 1
    print(f"candidate {candidate}")
    print(f"preflight: {len(mutations)} R6MR rows; every target occurs exactly once")
    root = tempfile.mkdtemp(prefix="grapt-r6mr-mutations-")
    failures = []
    try:
        for entry in mutations:
            accounted, detail = mutate_one(repo, candidate, root, entry)
            print(f"{entry['id']} {'RED' if accounted else 'UNACCOUNTED'}: {detail}")
            if not accounted:
                failures.append(entry["id"])
    finally:
        shutil.rmtree(root, ignore_errors=True)
    if git("status", "--porcelain", cwd=repo) or git("rev-parse", "HEAD", cwd=repo) != candidate:
        print("GATE FAILED: candidate changed during mutation run")
        return 1
    if failures:
        print(f"GATE FAILED: {len(failures)} unaccounted mutation(s): {failures}")
        return 1
    print(f"all {len(mutations)} R6MR mutations accounted for, every one RED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
