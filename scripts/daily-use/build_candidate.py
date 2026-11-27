"""Capture product, CLI and migration build provenance from a governed carrier run.

Require matching commits and clean trees on both sides of the build interval,
then hash its outputs for assemble_candidate.py. This command never builds
and refuses incomplete evidence or an existing receipt.
"""

import argparse
import json
from pathlib import Path

from receipts import ProvenanceError, digest, load_receipt, required_commit, required_text

PRODUCT = "grapt-server.exe"
CLI = "grapt.exe"
MIGRATE = "grapt-migrate.exe"

BINARIES = {
    PRODUCT: ("grapt-product", "grapt-server"),
    CLI: ("grapt-cli", "grapt"),
    MIGRATE: ("grapt-release", "grapt-migrate"),
}


def capture(args: argparse.Namespace) -> int:
    run = Path(args.governed_run).resolve()
    out = Path(args.receipt)
    # NEVER OVERWRITE EARLIER BUILD EVIDENCE. A receipt silently replaced is
    # evidence destroyed, and the replacement would look exactly as trustworthy
    # as what it displaced.
    if out.exists():
        raise ProvenanceError(f"{out} already exists; build evidence is never overwritten")

    evidence = load_receipt(run / "build-evidence.json")
    before = required_commit(evidence, run, "commit_before")
    after = required_commit(evidence, run, "commit_after")
    # THE INTERVAL MUST COVER THE BUILD. Equal endpoints are the claim; a
    # single reading taken beforehand is not.
    if before != after:
        raise ProvenanceError(
            f"HEAD changed across the build interval: {before} then {after}"
        )
    for edge in ("tree_dirty_before", "tree_dirty_after"):
        if evidence.get(edge) is not False:
            raise ProvenanceError(f"{run} does not attest a clean tree at {edge}")

    profile = required_text(evidence, run, "profile")
    target = required_text(evidence, run, "target")
    commands = evidence.get("commands")
    if not isinstance(commands, list) or not commands:
        raise ProvenanceError(f"{run} records no build commands")

    outputs = {}
    for name, (package, binary) in sorted(BINARIES.items()):
        produced = run / name
        sha, size = digest(produced)
        outputs[name] = {
            "path": str(produced),
            "sha256": sha,
            "bytes": size,
            "package": package,
            "bin": binary,
        }

    receipt = {
        "schema": "secode.daily-use.candidate-build.v1",
        "source_commit": after,
        "tree_dirty": False,
        "profile": profile,
        "target": target,
        "commands": commands,
        "outputs": outputs,
        "governed_run": str(run),
        "attests": (
            "these outputs are the bytes a governed run produced by these "
            "commands, with HEAD and tree state observed on BOTH sides of the "
            "build interval. NOT a reproducibility claim and NOT a toolchain "
            "provenance claim."
        ),
    }
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(receipt, indent=2, sort_keys=True) + chr(10), encoding="utf-8")
    print(json.dumps({"receipt": str(out), "source_commit": after}, indent=2))
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--governed-run",
        required=True,
        help="directory of a GOVERNED carrier0 build run holding build-evidence.json "
        "and the produced binaries. This script never builds.",
    )
    parser.add_argument("--receipt", required=True)
    raise SystemExit(capture(parser.parse_args()))


if __name__ == "__main__":
    main()
