"""Replay committed daily-use mutations in an exclusively owned checkout.

Use --check to validate exact-one anchors without changing source. Run a single
--id or all rows for --tree rust/retained. Heavy Rust runs use the governed carrier.
"""
import argparse
import hashlib
import json
import re
import subprocess
import shutil
import sys
from pathlib import Path


def prepare(root, row):
    path = (root / row["source"]).resolve()
    if not path.is_relative_to(root):
        raise ValueError("mutation path escapes checkout")
    original = path.read_bytes()
    source = original.decode("utf-8").replace("\r\n", "\n")
    for edit in row["edits"]:
        count = source.count(edit["anchor"])
        if count != 1:
            raise ValueError(f"{row['id']}: expected exactly one anchor, got {count}")
        source = source.replace(edit["anchor"], edit["replacement"], 1)
    return path, original, source.encode("utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--tree", required=True, choices=["rust", "retained"])
    parser.add_argument("--id", action="append")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--archive", type=Path, help="Zip evidence even when a row fails")
    parser.add_argument("--bun", default="bun", help="Bun executable path when it is not on PATH")
    args = parser.parse_args()
    root = args.root.resolve(strict=True)
    manifest = json.loads(Path(__file__).with_name("replay_mutations.json").read_text(encoding="utf-8"))
    rows = [row for row in manifest["rows"] if row["tree"] == args.tree and (not args.id or row["id"] in args.id)]
    if not rows or (args.id and set(args.id) != {row["id"] for row in rows}):
        raise ValueError("unknown mutation id for selected tree")
    for row in rows:
        prepare(root, row)
    if args.check:
        print(json.dumps({"checked": [row["id"] for row in rows]}))
        return
    if args.output is None:
        raise ValueError("--output required for execution")
    args.output.mkdir(parents=True, exist_ok=False)
    try:
        replay(rows, args, root)
    finally:
        if args.archive:
            args.archive.parent.mkdir(parents=True, exist_ok=True)
            shutil.make_archive(str(args.archive.with_suffix("")), "zip", args.output)


def replay(rows, args, root):
    receipts = []
    for row in rows:
        path, original, mutated = prepare(root, row)
        def run(phase):
            command = list(row["command"])
            if command[0] == "bun":
                command[0] = args.bun
            elif command[0] in ("python", "python3"):
                command[0] = sys.executable
            log = args.output / f"{row['id']}-{phase}.log"
            with log.open("wb") as output:
                result = subprocess.run(command, cwd=root / row.get("cwd", "."),
                                        stdout=output, stderr=subprocess.STDOUT, timeout=300)
            text = log.read_text(encoding="utf-8", errors="replace")
            if phase == "red":
                expected = row.get("red_exit", 101 if row["tree"] == "rust" else 1)
                if result.returncode != expected or not re.search(row["failure_pattern"], text):
                    raise RuntimeError(f"{row['id']}: no named assertion failure, exit {result.returncode}")
            elif result.returncode != 0 or not re.search(row.get("green_pattern", r"[1-9][0-9]* (?:pass|passed)"), text):
                raise RuntimeError(f"{row['id']}: green did not run passing tests")
            return {"exit": result.returncode, "log_sha256": hashlib.sha256(log.read_bytes()).hexdigest()}
        try:
            path.write_bytes(mutated)
            red = run("red")
        finally:
            path.write_bytes(original)
            if path.read_bytes() != original:
                raise RuntimeError("source restoration failed")
        green = run("green")
        receipts.append({"id": row["id"], "source_sha256": hashlib.sha256(original).hexdigest(),
                         "red": red, "green": green, "restored": True})
        (args.output / "receipt.json").write_text(json.dumps(receipts, indent=2) + "\n", encoding="utf-8")
        print(row["id"], "red then green; source restored", flush=True)


if __name__ == "__main__":
    main()
