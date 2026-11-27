"""Assemble a REVIEW-CANDIDATE dogfood package for DAILY-USE-BACKEND-01 r2.

This is NOT the accepted distribution door. `scripts/dogfood/package.py` is,
and it is left alone: its pins describe accepted bytes, and adopting new pins
is a landing decision rather than an authoring one.

What this does instead is assemble a package whose manifest describes THE
CANDIDATE HONESTLY, and refuses to write the manifest at all when it cannot.
Every identity in the output is either observed here or carried from a receipt
that was checked against the bytes -- not composed from an argument vector.

CORRECTED, and the corrections are the point of this file:

  * The first version's host check read a top-level `sha256` that the governed
    receipt does not have, accepted `None`, and therefore verified NOTHING
    against any real receipt. Host provenance now requires the receipt's nested
    `host.sha256`/`host.bytes` and a well-formed `source_commit`, compares them
    against BOTH the source artifact and the copied artifact, and requires the
    host identity to be one the caller explicitly approved.

  * The first version wrote a `build` string it made up. It now consumes a
    build receipt from `build_candidate.py` binding source commit, tree state,
    exact argument vector, profile, target and output hashes, and refuses to
    package bytes that receipt does not describe.

  * Bounds are checked before allocation, in `receipts.py`.

  * The unaccepted status now reaches the MANIFEST as well as the side receipt,
    because the launcher reads the manifest and a reader of the launched
    installation should not have to find a second file to learn what this is.

The existing `scripts/dogfood/windows.py` verifies manifest bytes at BOTH
preparation and launch. That is why this writes a real manifest rather than a
convenient one: the launcher is the check, and it is reused unchanged. Note
that `windows.py prepare` also stamps a `build` LABEL from operator
configuration into the running service; that label is not attestation and
nothing here treats it as any.
"""

import argparse
import json
import subprocess
from pathlib import Path

from receipts import (
    MAX_ARTIFACT_BYTES,
    MAX_RECEIPT_BYTES,
    ProvenanceError,
    _bounded_size,
    copy_bounded,
    digest,
    load_receipt,
    must_match_artifact,
    required_bytes,
    required_commit,
    required_sha256,
    required_text,
)

HOST = "grapt-desktop.exe"
PRODUCT = "grapt-server.exe"
CLI = "grapt.exe"
# The migration binary the operator plan's rehearse, status, handoff,
# disposition and restore commands run (R6-CUTOVER-SOAK-01 r13). A package
# without it cannot perform the supported switch or its rollback.
MIGRATE = "grapt-migrate.exe"

#: What each packaged governed binary must be, by name. The build receipt
#: records a package and bin for every output; they are CHECKED against this,
#: not merely copied into the manifest, so an entry describing some other
#: build's binary under this name is refused rather than packaged.
EXPECTED = {
    PRODUCT: ("grapt-product", "grapt-server"),
    CLI: ("grapt-cli", "grapt"),
    MIGRATE: ("grapt-release", "grapt-migrate"),
}
NOTICES = "THIRD-PARTY-NOTICES.txt"

STATUS = "UNACCEPTED REVIEW CANDIDATE"

#: Paths that cannot change a built binary, so a checkout may move across them
#: after the build without invalidating its receipt. Everything else is
#: refused. Deliberately short: `crates/`, `Cargo.toml`, `Cargo.lock`,
#: `rust-toolchain*`, `.cargo/` and `build.rs` are all absent on purpose.
FIXTURE_ONLY_PREFIXES = (
    "scripts/daily-use/",
    "docs/",
    "remote-compute/daily-use",
)


def git(rust: Path, *args: str) -> str:
    return subprocess.check_output(["git", *args], cwd=rust, text=True).strip()


def host_provenance(
    receipt_path: Path,
    source: Path,
    copied: Path,
    approved: set[str],
) -> dict:
    """Establish that the reused host is the approved, attested artifact.

    The host is REUSED rather than rebuilt, so its provenance is entirely
    inherited. Four independent things have to hold, and each was previously
    either unchecked or vacuously checked:

      1. the receipt actually carries an artifact identity, in the shape the
         governed generator writes it (`host.sha256`, `host.bytes`);
      2. it carries a non-missing source identity, so the manifest is not
         writing `null` where a commit belongs;
      3. it describes the SOURCE bytes we were pointed at -- otherwise we are
         copying one artifact under another's receipt;
      4. it describes the COPIED bytes in the output -- otherwise a copy that
         truncated or raced is packaged with a receipt for what it used to be.

    And the caller must name the host identity it approves. Without that the
    check is only internally consistent: any artifact shipped with its own
    matching receipt would pass, which is not what "the accepted host" means.
    """
    receipt = load_receipt(receipt_path)
    attested_sha = required_sha256(receipt, receipt_path, "host", "sha256")
    attested_bytes = required_bytes(receipt, receipt_path, "host", "bytes")
    source_commit = required_commit(receipt, receipt_path, "source_commit")

    if attested_sha not in approved:
        raise ProvenanceError(
            f"the host receipt attests {attested_sha}, which is not an approved host identity"
        )

    observed_source = digest(source)
    must_match_artifact("host source", receipt_path, attested_sha, attested_bytes, observed_source, source)

    observed_copy = digest(copied)
    must_match_artifact("host copy", receipt_path, attested_sha, attested_bytes, observed_copy, copied)

    return {
        "source_commit": source_commit,
        "sha256": observed_copy[0],
        "bytes": observed_copy[1],
        "profile": "release",
        "reused_from": str(source),
        "build_receipt": str(receipt_path),
        "build_receipt_sha256": digest(receipt_path)[0],
        "reuse_basis": (
            "the desktop host comes from the named governed build receipt; "
            "its attested host identity was compared "
            "against both the source artifact and the copied bytes"
        ),
    }


def assembly_provenance(rust: Path, head: str) -> dict:
    """What produced this package, as distinct from what produced the binaries.

    The binaries' source is immutable and comes from their build receipt. This
    describes the checkout that ASSEMBLED them, including any uncommitted
    fixture edits, so a reader is never told a dirty tree equals its HEAD.
    """
    dirt = subprocess.check_output(
        ["git", "status", "--porcelain"], cwd=rust, text=True
    ).rstrip()
    return {
        "commit": head,
        "clean": not dirt,
        "uncommitted_paths": sorted(
            line[3:] for line in dirt.splitlines() if len(line) > 3
        )[:32],
    }


def claims(produces: object, name: str) -> bool:
    """Does this recorded command claim to have produced `name`?

    A bare string is one binary; a list is one invocation that produced
    several. Anything else claims nothing -- and `None` in particular must not
    read as a match, which is how an unaccounted-for binary would slip through.
    """
    if isinstance(produces, str):
        return produces == name
    if isinstance(produces, list):
        return any(isinstance(entry, str) and entry == name for entry in produces)
    return False


def build_provenance(
    receipt_path: Path,
    rust: Path,
    head: str,
    profile: str,
    target: str,
    copies: dict[str, tuple[Path, tuple[str, int]]],
) -> dict[str, dict]:
    """Bind the packaged product and CLI bytes to the build that made them."""
    receipt = load_receipt(receipt_path)
    attested_commit = required_commit(receipt, receipt_path, "source_commit")
    # THE BINARY'S SOURCE AND THE PACKAGING FIXTURE'S SOURCE ARE TWO FACTS.
    #
    # A later checkout is accepted only when every path that differs is on the
    # allowlist below, and only when it DESCENDS from the attested commit.
    if attested_commit != head:
        descends = subprocess.run(
            ["git", "merge-base", "--is-ancestor", attested_commit, head],
            cwd=rust,
            check=False,
        )
        if descends.returncode != 0:
            raise ProvenanceError(
                f"{head} does not descend from the attested {attested_commit}: "
                "these binaries belong to a different line of history"
            )
        changed = subprocess.check_output(
            ["git", "diff", "--name-only", attested_commit, head],
            cwd=rust,
            text=True,
        ).split()
        # AN ALLOWLIST, NOT AN EXCLUSION LIST. Asking only whether `crates/`,
        # `Cargo.toml` and `Cargo.lock` moved does NOT establish that build
        # inputs held still: a toolchain file, `.cargo/config`, a build helper
        # or an included asset changes the bytes without touching any of them.
        # So anything not named here is refused, and adding a path is a
        # deliberate act rather than an omission.
        offending = [
            path
            for path in changed
            if not any(path.startswith(prefix) for prefix in FIXTURE_ONLY_PREFIXES)
        ]
        if offending:
            raise ProvenanceError(
                f"{head} differs from the attested {attested_commit} outside the "
                f"fixture allowlist: {', '.join(sorted(offending)[:8])}"
            )

    # A dirty tree is refused HERE rather than by re-inspecting the checkout,
    # because what matters is the tree AT BUILD TIME. Re-checking now would
    # pass for a tree cleaned up after a dirty build.
    if receipt.get("tree_dirty") is not False:
        raise ProvenanceError(
            f"{receipt_path} does not attest a clean tree at build time: "
            f"{receipt.get('tree_dirty_detail') or receipt.get('tree_dirty')!r}"
        )
    if required_text(receipt, receipt_path, "profile") != profile:
        raise ProvenanceError(f"{receipt_path} attests a different profile than {profile}")
    if required_text(receipt, receipt_path, "target") != target:
        raise ProvenanceError(f"{receipt_path} attests a different target than {target}")

    commands = receipt.get("commands")
    if not isinstance(commands, list) or not commands:
        raise ProvenanceError(f"{receipt_path} records no build commands")

    described = {}
    for name, (path, observed) in copies.items():
        attested_sha = required_sha256(receipt, receipt_path, "outputs", name, "sha256")
        attested_bytes = required_bytes(receipt, receipt_path, "outputs", name, "bytes")
        must_match_artifact(name, receipt_path, attested_sha, attested_bytes, observed, path)
        attested_identity = (
            required_text(receipt, receipt_path, "outputs", name, "package"),
            required_text(receipt, receipt_path, "outputs", name, "bin"),
        )
        if name in EXPECTED and attested_identity != EXPECTED[name]:
            raise ProvenanceError(
                f"{receipt_path} describes {name} as {attested_identity[0]}/{attested_identity[1]}, "
                f"not {EXPECTED[name][0]}/{EXPECTED[name][1]}"
            )
        # `produces` is either the one binary a command made or the list a
        # single build invocation made. One cargo command builds both binaries
        # here, so requiring a bare string would have made this check
        # unsatisfiable against the governed run's own evidence.
        produced = [
            command
            for command in commands
            if isinstance(command, dict) and claims(command.get("produces"), name)
        ]
        if not produced:
            raise ProvenanceError(f"{receipt_path} records no command producing {name}")
        described[name] = {
            "source_commit": attested_commit,
            "sha256": observed[0],
            "bytes": observed[1],
            "profile": profile,
            "target": target,
            "package": required_text(receipt, receipt_path, "outputs", name, "package"),
            "build_commands": produced,
            "build_receipt": str(receipt_path),
            "build_receipt_sha256": digest(receipt_path)[0],
        }
    # One file packaged under two names is a substitution, whatever its receipt
    # says: the migration binary copied from the CLI would otherwise pass as
    # both, each entry faithfully describing the same bytes.
    by_digest: dict[str, str] = {}
    for name, entry in sorted(described.items()):
        if entry["sha256"] in by_digest:
            raise ProvenanceError(
                f"{name} and {by_digest[entry['sha256']]} are the same bytes; "
                "one of them is not the binary its name claims"
            )
        by_digest[entry["sha256"]] = name
    return described


def assemble(args: argparse.Namespace) -> None:
    output = Path(args.output)
    if output.exists():
        raise ValueError("Candidate package output must be a new directory")
    rust = Path(args.rust_root).resolve()
    host = Path(args.host).resolve()
    host_receipt = Path(args.host_receipt).resolve()
    build_receipt = Path(args.build_receipt).resolve()
    approved = {identity.strip().lower() for identity in args.approved_host_sha256}

    head = git(rust, "rev-parse", "HEAD")
    if git(rust, "status", "--porcelain", "--", "crates", "Cargo.toml", "Cargo.lock"):
        raise ValueError(
            "the candidate checkout has uncommitted crate changes; the manifest "
            "would name a commit the bytes were not built from"
        )
    base = args.accepted_base
    subprocess.run(["git", "merge-base", "--is-ancestor", base, head], cwd=rust, check=True)

    # THE BINARIES COME FROM THE GOVERNED RUN, never from a local build tree.
    # This used to read `rust_root/target/<target>/<profile>`, which quietly
    # assumed someone had built here. Local Windows builds are not authorized,
    # so that path could only ever have been satisfied by doing the forbidden
    # thing -- and if a stale local target/ HAD existed, the assembler would
    # have packaged those bytes while the receipt described the governed run's.
    # The two would have disagreed and only the hash comparison would have
    # caught it. Reading the returned artifacts removes the disagreement at its
    # source.
    built_dir = Path(args.built).resolve()
    sources = {name: built_dir / name for name in (PRODUCT, CLI, MIGRATE)}
    for built in sources.values():
        if not built.is_file():
            raise ValueError(
                f"{built} is missing; it must come from a governed build run's "
                "returned artifacts"
            )

    # PREFLIGHT BEFORE ANY COPY. The first version copied product, CLI, host
    # and notices and only then asked whether they were within bounds, so an
    # oversized input cost a full disk copy before it was refused. Every input
    # is now measured and refused first, and the copy itself is bounded.
    preflight = dict(sources)
    preflight[HOST] = host
    preflight[NOTICES] = Path(args.notices).resolve()
    for name, candidate in preflight.items():
        bound = MAX_RECEIPT_BYTES if name == NOTICES else MAX_ARTIFACT_BYTES
        _bounded_size(candidate, bound)

    output.mkdir(parents=True)
    copies: dict[str, tuple[Path, tuple[str, int]]] = {}
    for name, built in sources.items():
        copy_bounded(built, output / name)
        copies[name] = (output / name, digest(output / name))

    binaries = build_provenance(build_receipt, rust, head, args.profile, args.target, copies)

    copy_bounded(host, output / HOST)
    binaries[HOST] = host_provenance(host_receipt, host, output / HOST, approved)
    binaries[HOST]["target"] = args.target

    notices = preflight[NOTICES]
    copy_bounded(notices, output / NOTICES, MAX_RECEIPT_BYTES)
    notice_sha, notice_bytes = digest(output / NOTICES)

    manifest = {
        "schema": "grapt.windows-dogfood.package.v1",
        "binaries": binaries,
        "notices": {"sha256": notice_sha, "bytes": notice_bytes},
        # CARRIED INTO THE MANIFEST, not only the side receipt. The launcher
        # reads this file; a reader of a prepared installation should learn
        # from the artifact it actually consults that nothing here is accepted.
        "status": STATUS,
        # The commit the PRODUCT BINARIES were built from.
        "candidate_commit": binaries[PRODUCT]["source_commit"],
        # The commit this package was ASSEMBLED at, which may carry later
        # fixture edits from the allowlist that no binary depends on. Carries
        # the working tree's dirt: a checkout with uncommitted fixture changes
        # is NOT fully described by its HEAD, and saying so is the difference
        # between a provenance record and a plausible one.
        "assembly_provenance": assembly_provenance(rust, head),
    }
    (output / "dogfood-package.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    (output / "CANDIDATE-STATUS.json").write_text(
        json.dumps(
            {
                "schema": "grapt.daily-use.candidate-package.v1",
                "status": STATUS,
                "campaign": "DAILY-USE-BACKEND-01 r2",
                "candidate_commit": binaries[PRODUCT]["source_commit"],
                "assembly_provenance": assembly_provenance(rust, head),
                "accepted_base": base,
                "authority": (
                    "campaign-local assembly authorised for this campaign only. "
                    "NOT an accepted distribution, NOT a pin adoption, and NOT a "
                    "claim of review. scripts/dogfood/package.py remains the "
                    "accepted door and its pins are untouched."
                ),
                "attribution_limits": (
                    "the build receipt binds these bytes to the commands that "
                    "produced them on one run. It is not a reproducibility "
                    "claim. The `build` label reported by the running service "
                    "is operator configuration from windows.py prepare and "
                    "attests nothing about compiled source."
                ),
                "host_provenance": binaries[HOST],
                "approved_host_identities": sorted(approved),
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    # NAMES THE BINARY'S SOURCE, not the checkout. Printing `head` here said
    # the package came from the commit that happened to be checked out, which
    # is exactly the conflation the manifest is careful to avoid.
    print(
        json.dumps(
            {
                "output": str(output),
                "candidate_commit": binaries[PRODUCT]["source_commit"],
                "assembled_at": head,
                "status": STATUS,
            },
            indent=2,
        )
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True)
    parser.add_argument("--rust-root", required=True)
    parser.add_argument("--host", required=True)
    parser.add_argument("--host-receipt", required=True)
    parser.add_argument("--build-receipt", required=True)
    parser.add_argument(
        "--built",
        required=True,
        help="directory of a GOVERNED run's returned binaries. There is no "
        "local-build fallback: this campaign does not build Windows artifacts "
        "on this host.",
    )
    parser.add_argument(
        "--approved-host-sha256",
        required=True,
        action="append",
        help="a host identity this assembly may reuse; repeat to allow more than one",
    )
    parser.add_argument("--notices", required=True)
    parser.add_argument("--accepted-base", required=True)
    parser.add_argument("--profile", default="debug")
    parser.add_argument("--target", default="x86_64-pc-windows-msvc")
    assemble(parser.parse_args())


if __name__ == "__main__":
    main()
