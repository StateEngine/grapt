"""Build a genuinely PREVIOUS package, then exercise the upgrade onto the candidate.

## Why a second real build, and not a synthesised "previous"

An upgrade journey needs two packages. The tempting shortcut is to take one
build and re-label it with an earlier version string, which would be a
fabricated artifact: the packager refuses a `source_commit` that is not a commit
in the repository, and defeating that check to feed a journey would corrupt the
one thing the whole campaign is about.

The bundle the governed runner ships carries COMPLETE HISTORY, so the previous
commit is genuinely present in this checkout. This adds a worktree at it, builds
the shipped set there with the same builder, and packages it with the same
packager. Both packages are real, both attest a commit that exists, and neither
is special-cased anywhere.

## The limit this journey reports rather than implies

If the two commits do not differ under `crates/`, the two packages carry
byte-identical binaries. The upgrade then exercises the install, backup,
identity transition and rollback MECHANICS truthfully, and proves nothing at all
about a code upgrade. That is the situation on this branch. `exercise_package`
records `binaries_differ` so the receipt states which of the two was
demonstrated; this script additionally records whether the two commits even
touch the product source, so a reader does not have to infer it from digests.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import exercise_package as journey  # noqa: E402

TARGET = "x86_64-unknown-linux-gnu"
#: The paths that decide whether an upgrade could possibly change the program.
PRODUCT_PATHS = ("crates", "Cargo.toml", "Cargo.lock")


def run(argv: list[str], cwd: Path) -> None:
    subprocess.run(argv, cwd=cwd, check=True, timeout=10_800)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path, help="the candidate checkout")
    parser.add_argument("--current-package", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path, help="outside the checkout")
    parser.add_argument("--receipt", required=True, type=Path)
    parser.add_argument("--previous", default="HEAD~1")
    arguments = parser.parse_args()

    root = arguments.root.resolve()
    scratch = arguments.scratch.resolve()
    scratch.mkdir(parents=True, exist_ok=True)

    previous_commit = subprocess.check_output(
        ["git", "rev-parse", arguments.previous], cwd=root, text=True
    ).strip()
    current_commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    if previous_commit == current_commit:
        raise journey.ExerciseError(
            f"{arguments.previous!r} resolves to the candidate itself; an upgrade "
            "journey between one package and itself proves nothing"
        )

    # Does the product source differ at all between the two commits? Recorded
    # BEFORE building, so the receipt carries the reason for whatever the
    # digests turn out to say rather than an explanation invented afterwards.
    changed = subprocess.check_output(
        ["git", "diff", "--name-only", previous_commit, current_commit, "--", *PRODUCT_PATHS],
        cwd=root, text=True,
    ).split()

    tree = scratch / "previous-tree"
    previous_package = scratch / "previous-package"
    try:
        run(["git", "worktree", "add", "--detach", str(tree), previous_commit], root)
        # The SAME builder and the SAME packager. A previous package assembled by
        # a different path would be comparing two things, only one of which is
        # the product of this campaign's tooling.
        run(["python3", str(root / "scripts/release/build_artifacts.py"), "--root", str(tree)], root)
        run([
            "python3", str(root / "scripts/release/package.py"),
            "--rust-root", str(tree),
            "--artifacts", str(tree / "artifacts"),
            "--build-receipt", str(tree / "artifacts/build-receipt.json"),
            "--output", str(previous_package),
            "--target", TARGET,
        ], root)

        environment = journey.scrubbed_environment()
        journey.verify_scrub(environment)
        work = scratch / "upgrade-work"
        work.mkdir()
        records = journey.upgrade(
            previous_package, arguments.current_package.resolve(), work, environment,
            journey.shipped.suffix_for(TARGET),
        )
    finally:
        # Leave the candidate checkout as it was found. Worktree metadata lives
        # under .git rather than in the working tree, so it would not show as
        # dirt -- which is exactly why removing it deliberately matters.
        subprocess.run(["git", "worktree", "remove", "--force", str(tree)],
                       cwd=root, check=False, timeout=300)
        subprocess.run(["git", "worktree", "prune"], cwd=root, check=False, timeout=300)

    receipt = {
        "schema": "grapt.release.upgrade-journey.v1",
        "previous_commit": previous_commit,
        "current_commit": current_commit,
        "product_paths_changed": changed,
        "product_source_differs": bool(changed),
        "cases": [record["case"] for record in records],
        "records": records,
        "attests": (
            "two packages built by the same builder and assembled by the same "
            "packager from two commits that both exist in this repository. The "
            "installed identity was checked from the INSTALLED manifest at every "
            "step, the replaced version was preserved before being overwritten, "
            "and rollback restored the previous identity. If "
            "product_source_differs is false the binaries are identical and this "
            "demonstrates the install/backup/rollback mechanics and the identity "
            "transition -- NOT a code upgrade."
        ),
    }
    arguments.receipt.parent.mkdir(parents=True, exist_ok=True)
    arguments.receipt.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({k: receipt[k] for k in
                      ("previous_commit", "current_commit", "product_source_differs", "cases")},
                     indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except journey.ExerciseError as error:
        print(f"upgrade journey: {error}", file=sys.stderr)
        raise SystemExit(2) from error
