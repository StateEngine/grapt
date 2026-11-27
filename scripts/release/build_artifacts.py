"""Build the RELEASE artifact set for one target, on the governed Linux domain.

## Why this exists

At the accepted base there was NO artifact-producing build for Linux at all.
Every `remote-compute/*.json` profile is a GATE profile carrying
`"evidence": {"artifacts": []}`: they run fmt, clippy, rustdoc and test
batteries and return no binary. Linux is a REQUIRED execution target for
R6-RELEASE-PACKAGING-01, so the absence is work rather than a dependency.

## What it produces, and why that list

The SHIPPED SET declared in `scripts/release/shipped.py` -- not the two binaries
the Windows build happens to carry. That file is the single place the membership
lives, and `scripts/release/package.py` reads the same declaration, so the
builder and the packager cannot drift apart about what a release contains. Every
entry's cargo coordinates, the role it is disclosed under, and the one
deliberate exclusion are recorded there with their reasons.

This module deliberately does NOT restate the list. A second copy in a docstring
is a copy that can go stale, and it already did once: the header here said "the
four SHIPPABLE binaries" for as long as the table below it held five.

## What the receipt attests, and what it does not

It attests THE BYTES THESE COMMANDS PRODUCED ON THIS RUN, with source identity
and tree cleanliness observed on BOTH sides of the build-and-hash interval — so
a tree that changed while the binaries were being hashed is refused rather than
described. It is NOT a reproducibility claim and NOT a toolchain provenance
claim, and it says so in its own text rather than leaving a reader to infer the
limit.

The emitted keys are the ones the PACKAGER CONSUMES -- `source_commit`,
`tree_dirty`, `profile`, `target`, `outputs`, `commands` -- matching
`secode.daily-use.candidate-build.v1` rather than inventing a third vocabulary.
The Windows path emits a different intermediate shape
(`...candidate-build-evidence.v1`, with `commit_before`/`tree_dirty_before`)
that a later step converts; this one is written in the consumed shape directly,
so there is no converter to drift.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import shipped

#: The targets this builder knows how to produce, and how each is built.
#:
#: ONE builder rather than one per target. `remote-compute/daily-use-windows-build.py`
#: is a near-copy of the Linux path carrying its OWN two-entry binary list, and a
#: second copy here would reintroduce exactly the drift `shipped.py` was written
#: to end: two lists, disagreeing silently, each internally consistent.
#:
#: The Windows target is produced by cargo-xwin ON THE LINUX CARRIER. That is
#: Linux execution producing a Windows TARGET -- it is not the Windows-host
#: build the shared scheduler forbids, and it is the route the accepted desktop
#: host was built by.
TARGETS = {
    "x86_64-unknown-linux-gnu": {"build": ["build"], "suffix": ""},
    "x86_64-pc-windows-msvc": {"build": ["xwin", "build"], "suffix": ".exe"},
}

TARGET = "x86_64-unknown-linux-gnu"
PROFILE = "release"
TOOLCHAIN = "1.97.1"

#: TARGET-NEUTRAL, because this builder is. The name was
#: `linux-build-receipt.json` when there was only a Linux path, and the Windows
#: cross-build then produced five correct `.exe` binaries and a receipt the
#: governed profile could not find -- the build had succeeded and the run was
#: refused ARTIFACT_ABSENT over a file name.
RECEIPT_NAME = "build-receipt.json"
BUILD_SCHEMA = "secode.release.build.v1"

#: Produced binary -> (cargo package, cargo bin target).
#:
#: READ FROM `scripts/release/shipped.py`, which is the single declaration of
#: the shipped set that this builder and `scripts/release/package.py` BOTH
#: read. Two copies of this list could disagree silently: a builder that stops
#: producing a binary simply produces one fewer, and a packager holding its own
#: list would have no way to notice. Membership, cargo coordinates, the role
#: each binary is disclosed under, and the deliberate exclusion of
#: `cache-usage-benchmark` are all recorded there with their reasons.
BINARIES = shipped.cargo_coordinates()

#: The tree being built and where its artifacts land. Defaults to the working
#: directory, which is the candidate checkout under a governed run. They are
#: OVERRIDABLE so the same builder can produce a second, real build from another
#: worktree of the same repository -- which is how the upgrade journey obtains a
#: genuinely previous package instead of a synthesised one. Nothing about the
#: custody or refusal logic changes with them.
ROOT = Path.cwd()
OUTPUT = ROOT / "artifacts"

STEPS: list[dict] = []


def run(argv: list[str], timeout: int) -> None:
    subprocess.run(argv, cwd=ROOT, check=True, timeout=timeout)
    STEPS.append({"argv": argv, "cwd": "."})


def git(*args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def custody() -> tuple[str, str]:
    """Source identity and tree dirt, read together as one observation."""
    return (
        git("rev-parse", "HEAD"),
        git("status", "--porcelain", "--", "crates", "Cargo.toml", "Cargo.lock",
            "scripts/windows/recover-legacy-sign-in.ps1"),
    )


def digest(path: Path) -> tuple[str, int]:
    hasher = hashlib.sha256()
    counted = 0
    with path.open("rb") as handle:
        while True:
            block = handle.read(1024 * 1024)
            if not block:
                break
            counted += len(block)
            hasher.update(block)
    return hasher.hexdigest(), counted


def require_toolchain() -> None:
    """Refuse with the exact constraint rather than failing mid-build.

    A missing pinned toolchain should be reported as the one fact it is, not
    surfaced thousands of lines into a compile log.
    """
    try:
        version = subprocess.run(
            ["cargo", f"+{TOOLCHAIN}", "--version"],
            check=True,
            capture_output=True,
            text=True,
            timeout=300,
        )
    except (subprocess.CalledProcessError, FileNotFoundError, subprocess.TimeoutExpired) as error:
        print(
            f"GOVERNED TOOLCHAIN REFUSAL: cargo {TOOLCHAIN} is not available in "
            f"this carrier's effective environment. "
            f"CARGO_HOME={os.environ.get('CARGO_HOME')} "
            f"RUSTUP_HOME={os.environ.get('RUSTUP_HOME')}: {error}",
            file=sys.stderr,
        )
        raise SystemExit(3) from error
    STEPS.append({"argv": ["cargo", f"+{TOOLCHAIN}", "--version"], "cwd": "."})
    print(f"cargo: {version.stdout.strip()}", flush=True)


def main() -> int:
    global ROOT, OUTPUT, TARGET
    parser = argparse.ArgumentParser(description="build the shipped set for one target")
    parser.add_argument("--target", default=TARGET, choices=sorted(TARGETS),
                        help="the cargo target triple to produce")
    parser.add_argument("--root", type=Path, default=None,
                        help="the tree to build; defaults to the working directory")
    parser.add_argument("--output", type=Path, default=None,
                        help="where the artifacts land; defaults to <root>/artifacts")
    parser.add_argument("--jobs", type=int, default=8,
                        help="Cargo build jobs, bounded to 1–8 (default 8)")
    arguments = parser.parse_args()
    if not 1 <= arguments.jobs <= 8:
        parser.error("--jobs must be between 1 and 8")
    if arguments.root is not None:
        ROOT = arguments.root.resolve()
        OUTPUT = ROOT / "artifacts"
    if arguments.output is not None:
        OUTPUT = arguments.output.resolve()
    TARGET = arguments.target
    recipe = TARGETS[TARGET]
    suffix = recipe["suffix"]

    target_dir = os.environ.get("CARGO_TARGET_DIR")
    if not target_dir:
        print(
            "GOVERNED ENVIRONMENT REFUSAL: CARGO_TARGET_DIR is unset. The "
            "produced binaries live under it, and guessing a checkout-local "
            "target/ would either find nothing or find another run's bytes.",
            file=sys.stderr,
        )
        return 3

    require_toolchain()

    commit_before, dirt_before = custody()
    if dirt_before:
        print(
            f"BUILD REFUSED: the checkout is dirty before the build:\n{dirt_before}",
            file=sys.stderr,
        )
        return 4

    OUTPUT.mkdir(parents=True, exist_ok=True)

    run(["rustup", "target", "add", "--toolchain", TOOLCHAIN, TARGET], 900)

    argv = [
        "cargo",
        f"+{TOOLCHAIN}",
        *recipe["build"],
        "--release",
        # LOCKED. A release artifact must not silently resolve a different
        # dependency graph than the one the gates read; an out-of-date lockfile
        # fails the build instead of being updated underneath it.
        "--locked",
        "--target",
        TARGET,
        "-j",
        str(arguments.jobs),
    ]
    for package, binary in sorted(BINARIES.values()):
        argv.extend(["-p", package, "--bin", binary])
    run(argv, 7200)
    # The build step carries what it produced, so a packager can require every
    # packaged binary to be accounted for by a recorded command rather than
    # taking the directory listing on trust.
    STEPS[-1]["produces"] = sorted(f"{name}{suffix}" for name in BINARIES)

    built = Path(target_dir) / TARGET / PROFILE
    outputs = {}
    for name, (package, binary) in sorted(BINARIES.items()):
        # THE SUFFIX IS THE TARGET'S, not the host's. A Windows cross-build on
        # Linux produces `grapt-server.exe`, and a builder that looked for the
        # bare name would refuse a build that had in fact succeeded.
        produced = built / f"{name}{suffix}"
        if not produced.is_file():
            print(f"BUILD REFUSED: {produced} was not produced", file=sys.stderr)
            return 5
        shutil.copyfile(produced, OUTPUT / f"{name}{suffix}")
        # Hash the RETURNED artifact, not the source: these are the bytes the
        # collection door carries back and the packager will re-check.
        sha, size = digest(OUTPUT / f"{name}{suffix}")
        outputs[f"{name}{suffix}"] = {
            "sha256": sha,
            "bytes": size,
            "package": package,
            "bin": binary,
            "built_from": str(produced),
        }

    commit_after, dirt_after = custody()
    if commit_after != commit_before or dirt_after:
        print(
            "BUILD REFUSED: source custody changed across the build and hash "
            f"interval: {commit_before} -> {commit_after}, dirt after: {dirt_after!r}",
            file=sys.stderr,
        )
        return 6

    receipt = {
        "schema": BUILD_SCHEMA,
        # THE CONSUMED KEYS. A packager checks `source_commit` and requires
        # `tree_dirty` to be exactly False; both are written here rather than
        # left for a converter to derive from a before/after pair.
        "source_commit": commit_after,
        "tree_dirty": bool(dirt_before or dirt_after),
        "profile": PROFILE,
        "target": TARGET,
        "toolchain": TOOLCHAIN,
        # The interval facts are kept as well, because "clean at the start" and
        # "clean at the end" are two observations and collapsing them into one
        # boolean would hide which side moved.
        "commit_before": commit_before,
        "commit_after": commit_after,
        "tree_dirty_before": bool(dirt_before),
        "tree_dirty_after": bool(dirt_after),
        "cargo_target_dir": target_dir,
        "commands": STEPS,
        "outputs": outputs,
        "attests": (
            "these are the bytes these commands produced on this governed run, "
            "with source identity and tree cleanliness observed on BOTH sides "
            "of the build and hash interval. NOT a reproducibility claim and "
            "NOT a toolchain provenance claim."
        ),
    }
    (OUTPUT / RECEIPT_NAME).write_text(
        json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps({"commit": commit_after, "outputs": outputs}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
