"""Cross-build the candidate product and CLI for Windows on the governed carrier.

DAILY-USE-BACKEND-01 r2. This closes the campaign's real packaging dependency:
`scripts/daily-use/assemble_candidate.py` refuses to package bytes no build
receipt describes, and until now nothing produced that receipt.

WHY THIS EXISTS RATHER THAN A LOCAL BUILD. I previously wrote a wrapper that
ran `cargo build` on the Windows host. It was refused, correctly: heavy
Windows builds are not authorized there, and receipt capture belongs wired into
the governed run rather than sitting beside it. A Windows TARGET produced by
cargo-xwin on the Linux workstation is Linux execution -- it is not a Windows
host fallback, and it is the same route the accepted desktop host was built by.

MECHANISM REFERENCE, read-only: `remote-compute/desktop-windows-build.py` in
the accepted retained root. The detail that matters and is easy to get wrong is
where the binaries land: `CARGO_TARGET_DIR/x86_64-pc-windows-msvc/release`, NOT
a checkout-local `target/`. The governed environment redirects CARGO_TARGET_DIR
into the carrier cache, so a checkout-relative guess finds nothing -- or worse,
finds a stale artifact from some other run.

WHAT THIS ATTESTS, and the interval it covers. The previous wrapper read HEAD
and the tree state ONCE, before building, and never looked again: a commit or
edit between the two builds would have produced mixed bytes under a clean
source claim. Here the source identity and tree cleanliness are captured on
BOTH SIDES of the build-and-hash interval and must agree, so the receipt's
claim covers the build rather than preceding it.

WHAT IT DOES NOT ATTEST, stated because overclaiming here is exactly what this
campaign has been correcting: these are the bytes THESE commands produced on
THIS run at THIS commit. It is not a reproducibility claim and not a toolchain
provenance claim.
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

TARGET = "x86_64-pc-windows-msvc"
PROFILE = "release"
TOOLCHAIN = "1.97.1"

#: Produced binary -> (cargo package, cargo bin target). `grapt-migrate.exe` is
#: the migration binary the cutover operator plan's rehearse, status, handoff,
#: disposition and restore commands run (R6-CUTOVER-SOAK-01 r13).
BINARIES = {
    "grapt-server.exe": ("grapt-product", "grapt-server"),
    "grapt.exe": ("grapt-cli", "grapt"),
    "grapt-migrate.exe": ("grapt-release", "grapt-migrate"),
}
#: Parallel jobs for the crossbuild: the allocation on the shared carrier, where
#: every governed build runs at two. It was hardcoded to eight.
JOBS = 2

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
        git("status", "--porcelain", "--", "crates", "Cargo.toml", "Cargo.lock"),
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
    """Refuse with an exact constraint rather than a mid-build failure.

    The accepted desktop receipt proves a PREVIOUS run had cargo-xwin. It does
    not prove this carrier's current CARGO_HOME/RUSTUP_HOME still does, and a
    missing subcommand should be reported as the exact constraint it is rather
    than surfacing as an opaque build failure thousands of lines into a log.
    """
    try:
        version = subprocess.run(
            ["cargo", f"+{TOOLCHAIN}", "xwin", "--version"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
            timeout=300,
        )
    except (subprocess.CalledProcessError, FileNotFoundError, subprocess.TimeoutExpired) as error:
        print(
            "GOVERNED TOOLCHAIN REFUSAL: cargo-xwin is not available in this "
            f"carrier's effective environment for toolchain {TOOLCHAIN}. "
            f"CARGO_HOME={os.environ.get('CARGO_HOME')} "
            f"RUSTUP_HOME={os.environ.get('RUSTUP_HOME')}: {error}",
            file=sys.stderr,
        )
        raise SystemExit(3) from error
    STEPS.append({"argv": ["cargo", f"+{TOOLCHAIN}", "xwin", "--version"], "cwd": "."})
    print(f"cargo-xwin: {version.stdout.strip()}", flush=True)


def main() -> int:
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

    OUTPUT.mkdir(exist_ok=True)

    run(["rustup", "target", "add", "--toolchain", TOOLCHAIN, TARGET], 900)

    argv = [
        "cargo",
        f"+{TOOLCHAIN}",
        "xwin",
        "build",
        "--release",
        "--locked",
        "--target",
        TARGET,
        "-j",
        str(JOBS),
    ]
    for package, binary in sorted(BINARIES.values()):
        argv.extend(["-p", package, "--bin", binary])
    run(argv, 7200)
    # The build step carries what it produced. `assemble_candidate.py` requires
    # a recorded command to account for every packaged binary, so this is the
    # link that makes that check answerable rather than decorative.
    STEPS[-1]["produces"] = sorted(BINARIES)

    built = Path(target_dir) / TARGET / PROFILE
    outputs = {}
    for name, (package, binary) in sorted(BINARIES.items()):
        produced = built / name
        if not produced.is_file():
            print(f"BUILD REFUSED: {produced} was not produced", file=sys.stderr)
            return 5
        shutil.copyfile(produced, OUTPUT / name)
        # Hash the RETURNED artifact, not the source: these are the bytes the
        # collection door carries back and the assembler will check.
        sha, size = digest(OUTPUT / name)
        outputs[name] = {
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

    evidence = {
        "schema": "secode.daily-use.candidate-build-evidence.v1",
        "commit_before": commit_before,
        "commit_after": commit_after,
        "tree_dirty_before": bool(dirt_before),
        "tree_dirty_after": bool(dirt_after),
        "profile": PROFILE,
        "target": TARGET,
        "toolchain": TOOLCHAIN,
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
    (OUTPUT / "build-evidence.json").write_text(
        json.dumps(evidence, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps({"commit": commit_after, "outputs": outputs}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
