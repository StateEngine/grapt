#!/usr/bin/env python3
"""Run DOGFOOD-0 platform mutations in isolated detached worktrees."""

import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
MANIFEST = Path(__file__).with_name("manifest.json")
PREFIX = "IGPX"
EXPECTED = [f"{PREFIX}{number:02d}" for number in range(1, 14)]


def run(args, cwd=ROOT, env=None):
    return subprocess.run(
        args, cwd=cwd, text=True, capture_output=True, check=False, env=env
    )


def refuse(detail):
    print(f"{PREFIX} REFUSED: {detail}", file=sys.stderr)
    raise SystemExit(2)


def edits(row):
    return row.get("edits") or [
        {key: row[key] for key in ("file", "old", "new")}
    ]


def id_prefix(identifier):
    match = re.fullmatch(r"([A-Z]+)[0-9]+", identifier)
    return match.group(1) if match else ""


def load_rows():
    try:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        refuse(f"manifest unreadable: {error}")
    rows = document.get("mutations", [])
    identifiers = [row.get("id") for row in rows]
    if document.get("prefix") != PREFIX or identifiers != EXPECTED:
        refuse(f"exact inventory is {identifiers}, expected {EXPECTED}")
    if document.get("rows") != len(EXPECTED):
        refuse("declared row count does not match the exact inventory")
    for manifest in ROOT.glob("crates/**/mutations/manifest.json"):
        if manifest.resolve() == MANIFEST.resolve():
            continue
        try:
            other = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        collisions = [
            row.get("id", "")
            for row in other.get("mutations", [])
            if id_prefix(row.get("id", "")) == PREFIX
        ]
        if collisions:
            refuse(f"prefix collision in {manifest.relative_to(ROOT)}: {collisions}")
    return rows


def preflight(rows):
    problems = []
    for row in rows:
        if not edits(row):
            problems.append(f"{row['id']}: no edits")
        seen = set()
        for edit in edits(row):
            relative = Path(edit["file"])
            target = (ROOT / relative).resolve()
            try:
                target.relative_to(ROOT.resolve())
            except ValueError:
                problems.append(f"{row['id']}: path escapes root: {relative}")
                continue
            key = os.path.normcase(str(target))
            if key in seen:
                problems.append(f"{row['id']}: two edits name {relative}")
                continue
            seen.add(key)
            try:
                source = target.read_text(encoding="utf-8").replace("\r\n", "\n")
            except OSError as error:
                problems.append(f"{row['id']}: {relative} unreadable: {error}")
                continue
            count = source.count(edit["old"])
            if count != 1:
                problems.append(
                    f"{row['id']}: anchor count {count}, expected one in {relative}"
                )
            if edit["old"] == edit["new"]:
                problems.append(f"{row['id']}: no-op edit in {relative}")
    if problems:
        refuse("; ".join(problems))


def command(row):
    base = [
        "cargo",
        "test",
        "-p",
        row["package"],
    ]
    if row["test_target"] != "__lib__":
        base.extend(["--test", row["test_target"]])
    return [*base, row["test"], "--no-fail-fast"]


def remove_worktree(path):
    resolved = path.resolve()
    temporary = Path(tempfile.gettempdir()).resolve()
    if resolved.parent != temporary or not resolved.name.startswith("grapt-igpx-"):
        refuse(f"refusing unsafe worktree removal target {resolved}")
    if os.name == "nt" and path.exists():
        run(["cmd", "/c", "dir", "/a:l", str(path)])
    removed = run(["git", "worktree", "remove", "--force", str(path)])
    if removed.returncode != 0 or path.exists():
        refuse(f"worktree removal failed for {path}: {removed.stderr.strip()}")


def apply_row(row, worktree):
    for edit in edits(row):
        target = worktree / edit["file"]
        source = target.read_text(encoding="utf-8").replace("\r\n", "\n")
        count = source.count(edit["old"])
        if count != 1:
            refuse(f"{row['id']} anchor count changed to {count}")
        target.write_text(
            source.replace(edit["old"], edit["new"], 1),
            encoding="utf-8",
            newline="\n",
        )


def run_row(row, revision):
    path = Path(tempfile.gettempdir()) / f"grapt-igpx-{row['id'].lower()}-{os.getpid()}"
    if path.exists():
        refuse(f"pre-existing worktree path {path}")
    added = run(["git", "worktree", "add", "--detach", str(path), revision])
    if added.returncode != 0:
        refuse(f"worktree add failed for {row['id']}: {added.stderr.strip()}")
    try:
        apply_row(row, path)
        environment = dict(os.environ)
        environment["CARGO_TARGET_DIR"] = str(path / "target")
        result = run(command(row), cwd=path, env=environment)
        evidence = result.stdout + result.stderr
        if result.returncode == 0:
            refuse(f"{row['id']} survived")
        if "1 failed" not in evidence or row["test"] not in evidence:
            refuse(
                f"{row['id']} went red without executing its exact witness; "
                f"bounded evidence tail:\n{evidence[-4_000:]}"
            )
        if row["red"] not in evidence:
            refuse(
                f"{row['id']} went red without named evidence {row['red']!r}; "
                f"bounded evidence tail:\n{evidence[-4_000:]}"
            )
        print(f"MUTATED: {row['id']} RED — {row['red']}")
    finally:
        remove_worktree(path)


def main():
    if len(sys.argv) != 3 or sys.argv[1:] != ["--prefix", PREFIX]:
        refuse(f"usage: run.py --prefix {PREFIX}")
    status = run(["git", "status", "--porcelain"])
    if status.returncode != 0 or status.stdout.strip():
        refuse("candidate worktree is not clean")
    revision_result = run(["git", "rev-parse", "HEAD"])
    if revision_result.returncode != 0:
        refuse(revision_result.stderr.strip())
    revision = revision_result.stdout.strip()
    rows = load_rows()
    preflight(rows)
    print(f"preflight: {len(rows)} rows; every anchor occurs exactly once")
    for row in rows:
        run_row(row, revision)
    if run(["git", "status", "--porcelain"]).stdout.strip():
        refuse("candidate changed during mutation run")
    markers = run(["rg", "-n", "MUTATED:", "crates/grapt-platform/src", "crates/grapt-product/src"])
    if markers.returncode == 0:
        refuse("mutation marker survived in candidate source")
    print(f"{len(rows)}/{len(rows)} {PREFIX} rows RED at {revision}")


if __name__ == "__main__":
    main()
