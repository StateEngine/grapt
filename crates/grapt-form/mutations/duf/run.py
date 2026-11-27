#!/usr/bin/env python3
"""Run DAILY-USE-BACKEND-01 (DUF) form-law mutants in isolated detached worktrees.

Derived verbatim from the DUB runner so the isolation, allocation census, link
census, duplicate-evidence guard and cleanup discipline are the hardened ones
already proven in this tree. Two differences: the prefix is DUF, and the
surviving-marker scan covers the three crates this lane's rows edit.
"""

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
ROOT = HERE.parents[3]
MANIFEST = HERE / "manifest.json"
PREFIX = "DUF"
UNWALKED = {".git", "target", "node_modules"}


def run(args, cwd=ROOT, env=None):
    return subprocess.run(
        args,
        cwd=cwd,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def refuse(detail):
    print(f"{PREFIX} REFUSED: {detail}", file=sys.stderr)
    raise SystemExit(2)


def edits(row):
    if "edits" in row:
        return row["edits"]
    return [{key: row[key] for key in ("file", "old", "new")}]


def every_manifest():
    found = []
    for root, directories, files in os.walk(ROOT):
        directories[:] = [name for name in directories if name not in UNWALKED]
        if "manifest.json" in files:
            found.append(Path(root) / "manifest.json")
    return sorted(found)


def allocation_census():
    token = re.compile(rf"{PREFIX}[0-9]+")
    conflicts = []
    examined_rows = 0
    manifests = every_manifest()
    for path in manifests:
        try:
            document = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            conflicts.append(f"{path.relative_to(ROOT)} unreadable: {error}")
            continue
        rows = document.get("mutations")
        if not isinstance(rows, list):
            continue
        for row in rows:
            identity = row.get("id") if isinstance(row, dict) else None
            if not isinstance(identity, str):
                continue
            examined_rows += 1
            if token.fullmatch(identity) and path.resolve() != MANIFEST.resolve():
                conflicts.append(f"{path.relative_to(ROOT)} holds {identity}")
    if conflicts:
        refuse("whole-token collision: " + "; ".join(conflicts))
    print(
        f"allocation census: {len(manifests)} manifest(s), "
        f"{examined_rows} mutation row(s), {PREFIX} exclusively held here"
    )


def load_rows():
    try:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        refuse(f"manifest unreadable: {error}")
    rows = document.get("mutations")
    if document.get("prefix") != PREFIX or not isinstance(rows, list):
        refuse("manifest prefix or rows are malformed")
    expected = [f"{PREFIX}{number:02d}" for number in range(1, 16)]
    identities = [row.get("id") for row in rows]
    if document.get("rows") != len(expected) or identities != expected:
        refuse(f"exact inventory is {identities}, expected {expected}")
    return rows


def preflight(rows):
    problems = []
    root = ROOT.resolve()
    for row in rows:
        row_edits = edits(row)
        if not row_edits:
            problems.append(f"{row['id']}: no edits")
            continue
        for edit in row_edits:
            target = (ROOT / edit["file"]).resolve()
            try:
                target.relative_to(root)
            except ValueError:
                problems.append(f"{row['id']}: path escapes root: {edit['file']}")
                continue
            try:
                source = target.read_text(encoding="utf-8").replace("\r\n", "\n")
            except OSError as error:
                problems.append(f"{row['id']}: {edit['file']} unreadable: {error}")
                continue
            count = source.count(edit["old"])
            if count != 1:
                problems.append(
                    f"{row['id']}: anchor count {count}, expected one in {edit['file']}"
                )
            if edit["old"] == edit["new"]:
                problems.append(f"{row['id']}: no-op edit")
    if problems:
        refuse("; ".join(problems))
    print(f"preflight: {len(rows)} rows; every mutation anchor occurs exactly once")


def apply_row(row, worktree):
    by_file = {}
    for edit in edits(row):
        by_file.setdefault(edit["file"], []).append(edit)
    for relative, file_edits in by_file.items():
        target = worktree / relative
        source = target.read_text(encoding="utf-8").replace("\r\n", "\n")
        for edit in file_edits:
            count = source.count(edit["old"])
            if count != 1:
                refuse(f"{row['id']} anchor count changed to {count} in {relative}")
            source = source.replace(edit["old"], edit["new"], 1)
        target.write_text(source, encoding="utf-8", newline="\n")


def command(row):
    # This lane owns rows in two packages, so the package is a row coordinate
    # rather than a constant: a row that ran the wrong package's suite would
    # report a red that its own edit did not cause.
    args = ["cargo", "test", "--no-fail-fast", "-p", row["package"]]
    if row["test_target"] == "__lib__":
        args.append("--lib")
    elif row["test_target"] == "__bins__":
        args.append("--bins")
    else:
        args.extend(["--test", row["test_target"]])
    args.extend([row["test"], "--", "--exact"])
    return args


def ran_test(output):
    total = 0
    for match in re.finditer(r"(\d+) passed; (\d+) failed", output):
        total += int(match.group(1)) + int(match.group(2))
    return total > 0


def failure_detail(output):
    lines = output.splitlines()
    for index, line in enumerate(lines):
        if "panicked at" in line:
            next_line = lines[index + 1].strip() if index + 1 < len(lines) else ""
            return f"{line.strip()} {next_line}".strip()
    for line in lines:
        if line.startswith("Error:") or "assertion failed" in line:
            return line.strip()
    return "failed without a captured diagnostic"


def link_census(path):
    links = []
    is_junction = getattr(os.path, "isjunction", None)
    for root, directories, files in os.walk(path, followlinks=False):
        for name in [*directories, *files]:
            candidate = Path(root) / name
            if candidate.is_symlink() or (
                callable(is_junction) and is_junction(candidate)
            ):
                links.append(candidate)
    return links


def remove_worktree(path, run_root):
    resolved = path.resolve()
    if resolved.parent != run_root.resolve() or not resolved.name.startswith("worktree-"):
        refuse(f"unsafe worktree removal target {resolved}")
    links = link_census(path)
    if os.name == "nt":
        run(["cmd", "/c", "dir", "/a:l", "/s", str(path)])
    if links:
        refuse(f"{path} contains link/junction entries: {links[:8]}")
    removed = run(["git", "worktree", "remove", "--force", str(path)])
    if removed.returncode != 0 or path.exists():
        refuse(f"worktree removal failed for {path}: {removed.stderr.strip()}")


def run_row(row, revision, run_root, target_dir):
    worktree = run_root / f"worktree-{row['id'].lower()}"
    added = run(["git", "worktree", "add", "--detach", str(worktree), revision])
    if added.returncode != 0:
        refuse(f"worktree add failed for {row['id']}: {added.stderr.strip()}")
    try:
        apply_row(row, worktree)
        environment = dict(os.environ)
        environment["CARGO_TARGET_DIR"] = str(target_dir)
        result = run(command(row), cwd=worktree, env=environment)
        evidence = result.stdout + result.stderr
        if result.returncode == 0:
            refuse(f"{row['id']} survived")
        if not ran_test(evidence) or row["test"] not in evidence:
            refuse(
                f"{row['id']} failed without executing its exact witness; "
                f"bounded tail:\n{evidence[-4000:]}"
            )
        if row["red"] not in evidence:
            refuse(
                f"{row['id']} went red without named evidence {row['red']!r}; "
                f"bounded tail:\n{evidence[-4000:]}"
            )
        detail = failure_detail(evidence)
        print(f"MUTATED: {row['id']} RED — {detail}")
        return (row["test"], detail)
    finally:
        remove_worktree(worktree, run_root)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--prefix")
    parser.add_argument("--preflight-only", action="store_true")
    parser.add_argument("--row")
    args = parser.parse_args()
    if args.prefix != PREFIX:
        refuse(f"usage: run.py --prefix {PREFIX} [--preflight-only] [--row {PREFIX}NN]")
    status = run(["git", "status", "--porcelain"])
    if status.returncode != 0 or status.stdout.strip():
        refuse("candidate worktree is not clean")
    revision_result = run(["git", "rev-parse", "HEAD"])
    if revision_result.returncode != 0:
        refuse(revision_result.stderr.strip())
    revision = revision_result.stdout.strip()
    rows = load_rows()
    allocation_census()
    preflight(rows)
    print(f"candidate {revision}")
    if args.preflight_only:
        print("PREFLIGHT ONLY: no mutant ran; this is not mutation evidence")
        return 0
    all_rows = rows
    if args.row is not None:
        rows = [row for row in all_rows if row["id"] == args.row]
        if len(rows) != 1:
            refuse(f"selected row {args.row!r} is not in the exact {PREFIX} inventory")

    run_root = Path(tempfile.mkdtemp(prefix=f"grapt-duf-{os.getpid()}-"))
    target_dir = run_root / "target"
    evidence = {}
    try:
        for row in rows:
            key = run_row(row, revision, run_root, target_dir)
            if key in evidence:
                refuse(
                    f"{row['id']} duplicates evidence from {evidence[key]}: {key[1]}"
                )
            evidence[key] = row["id"]
    finally:
        remaining = run(["git", "worktree", "list", "--porcelain"])
        if str(run_root) in remaining.stdout:
            refuse(f"registered mutation worktree remains under {run_root}")
        if run_root.resolve().parent != Path(tempfile.gettempdir()).resolve():
            refuse(f"unsafe mutation run-root cleanup target {run_root}")
        shutil.rmtree(run_root, ignore_errors=False)

    if run(["git", "status", "--porcelain"]).stdout.strip():
        refuse("candidate changed during mutation run")
    marker = run(
        [
            "rg",
            "-n",
            "MUTATED:",
            "crates/grapt-form/src",
            "crates/grapt-api/src",
            "crates/grapt-product/src",
        ]
    )
    if marker.returncode == 0:
        refuse("mutation marker survived in candidate source")
    if args.row is None:
        print(f"{len(all_rows)}/{len(all_rows)} {PREFIX} rows RED at {revision}; survivors: 0")
    else:
        print(f"1/1 selected {args.row} row RED at {revision}; survivors: 0")
    return 0


if __name__ == "__main__":
    sys.exit(main())
