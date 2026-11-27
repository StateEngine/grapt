"""Focused red/original-green evidence for three changed legacy row anchors.

This grants no whole-family acceptance. Whole-prefix entry points and inventories
are unchanged; the rows come from their owning manifests, not copied mutations.
"""
import hashlib
import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "retained_product_runner", ROOT / "crates/grapt-product/mutations/run.py"
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("product mutation runner is unavailable")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)
ROWS = (
    ("grapt-permission", "DGPR12", "permission reply must distinguish a missing session before decision lookup"),
    ("grapt-runtime", "U06", "Idempotent emits no duplicate existing advisory"),
    ("grapt-store", "C21", "a transaction opened outside grapt-store carries no governed-write denial"),
)


def main():
    head = RUNNER.run(["git", "rev-parse", "HEAD"])
    if head.returncode != 0:
        raise RuntimeError(head.stderr)
    revision = head.stdout.strip()
    targets = set()
    for crate, identity, red in ROWS:
        path = ROOT / f"crates/{crate}/mutations/manifest.json"
        blob = path.read_bytes().replace(b"\r\n", b"\n")
        document = json.loads(blob)
        matches = [row for row in document["mutations"] if row["id"] == identity]
        if len(matches) != 1:
            raise RuntimeError(f"{identity} must occur exactly once in its owning manifest")
        row = dict(matches[0])
        row.setdefault("package", crate)
        row.setdefault("test_target", "lib")
        row["red"] = red
        for edit in row.get("edits") or [row]:
            targets.add(edit["file"])
        print(f"FOCUSED ROW {identity} manifest_sha256={hashlib.sha256(blob).hexdigest()}", flush=True)
        RUNNER.run_rows(identity, revision, [row])
    for relative in sorted(targets):
        original = RUNNER.git_blob(revision, relative)
        actual = (ROOT / relative).read_bytes().replace(b"\r\n", b"\n")
        if original.returncode != 0 or actual != original.stdout.replace(b"\r\n", b"\n"):
            raise RuntimeError(f"focused mutation residue: {relative}")
    print("Three focused rows red then original green; no whole-family acceptance claimed.")


if __name__ == "__main__":
    main()
