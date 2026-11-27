"""Check the declared shipped set against what the workspace actually builds.

## The gap this closes

`scripts/release/shipped.py` declares which binaries a release contains, and
`build_artifacts.py` and `package.py` both read it, so those two cannot drift apart.
But all three could still drift from the WORKSPACE. A new binary target added by
any lane would be shipped by nobody and refused by nothing: the declaration
would stay internally consistent and simply stop describing the product.

That is not hypothetical. `grapt-engined` was absent from the first version of
the shipped list precisely because the list was assembled by reading manifests
by eye, and a grep for `[[bin]]` finds only explicit declarations and silently
misses every auto-bin. The packaging disposition requires the binary targets
to be enumerated from `cargo metadata` with "no grep-based silent omission".
This makes that requirement MECHANICAL instead of a promise someone kept once.

## The rule

Every `bin` target in every workspace member must be accounted for: either
SHIPPED or explicitly EXCLUDED. A target in neither set is a refusal naming it.
The reverse is checked too -- a declaration naming a binary the workspace no
longer builds is equally a defect, and one that would otherwise surface as a
confusing build failure hours into a governed job rather than as the one fact it
is.

Cargo coordinates are verified as well, not just names: a shipped entry whose
`package` does not actually contain that `bin` would fail the build with a cargo
error, and this says so first, in the vocabulary of the declaration.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import shipped

ROOT = Path(__file__).resolve().parents[2]


def workspace_binaries() -> dict[str, str]:
    """`{bin name: package name}` for every bin target in every workspace member.

    From `cargo metadata --no-deps`, which reports the RESOLVED target list --
    auto-discovered binaries included -- rather than the subset a manifest
    happens to spell out.
    """
    raw = subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        text=True,
    )
    metadata = json.loads(raw)
    found: dict[str, str] = {}
    for package in metadata["packages"]:
        for target in package["targets"]:
            if "bin" in target["kind"]:
                found[target["name"]] = package["name"]
    return found


def main() -> int:
    found = workspace_binaries()
    declared = set(shipped.SHIPPED) | set(shipped.EXCLUDED)
    problems: list[str] = []

    unaccounted = sorted(set(found) - declared)
    if unaccounted:
        problems.append(
            f"the workspace builds binaries no release declaration accounts for: "
            f"{unaccounted}. Each must be added to SHIPPED or, with its reason, "
            f"to EXCLUDED in scripts/release/shipped.py -- an artifact that is "
            f"neither shipped nor deliberately excluded is one nobody decided about"
        )

    phantom = sorted(declared - set(found))
    if phantom:
        problems.append(
            f"the release declaration names binaries this workspace does not "
            f"build: {phantom}. A declaration describing a product that no "
            f"longer exists fails hours into a governed build instead of here"
        )

    for name, entry in sorted(shipped.SHIPPED.items()):
        if name not in found:
            continue
        if found[name] != entry["package"]:
            problems.append(
                f"{name} is declared in package {entry['package']!r} but the "
                f"workspace builds it in {found[name]!r}"
            )
        if entry["bin"] != name:
            problems.append(
                f"{name} declares bin target {entry['bin']!r}; the produced file "
                f"name and the bin target must agree, because the builder copies "
                f"the artifact by its file name"
            )

    if problems:
        print("SHIPPED SET REFUSED:", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        return 2

    print(
        json.dumps(
            {
                "workspace_binaries": dict(sorted(found.items())),
                "shipped": shipped.names(),
                "excluded": shipped.EXCLUDED,
                "attests": (
                    "every bin target in every workspace member is either "
                    "shipped or explicitly excluded, and every shipped entry's "
                    "cargo coordinates match what the workspace builds"
                ),
            },
            indent=2,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
