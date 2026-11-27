"""Assemble a release package whose manifest describes it HONESTLY, or refuse.

## The rule this file exists to enforce

A package manifest is read by an installer and by an operator recovering a
failed activation. Every field in it must be traceable to something observed,
and a field that cannot be established REFUSES THE WHOLE MANIFEST rather than
being written as a plausible value. `scripts/daily-use/assemble_candidate.py`
established that discipline for the Windows dogfood package; this is the same
discipline for the release artifact set, and it deliberately reuses that file's
vocabulary — `{"sha256", "bytes"}` per artifact, `source_commit`, `tree_dirty`,
`profile`, `target` — rather than inventing a second way to say the same things.

## What it will not do

It will NOT infer a signature. With no publisher, per-target signing state is
an explicit `"signed": false` with the reason. A publisher reaches the manifest
only when a live Windows Authenticode check reports every exact packaged file
Valid under that subject. A stub verifier is not signed-release evidence, and
an absent certificate is reported as absent.

It will NOT accept a binary that no recorded build command claims to have
produced. A directory listing is not provenance: bytes that appeared in an
output directory without a command accounting for them are exactly what a
provenance record is supposed to catch.

It will NOT re-hash the source path and call that the package digest. Every
digest is read FROM DISK after the copy, so the manifest describes the bytes
that are actually in the package.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import stat
import subprocess
import sys
from pathlib import Path

import shipped
import signing_check
import licenses
import notices
import stateengine_payload

#: The receipt schemas this packager knows how to consume.
BUILD_SCHEMAS = {
    # The target-neutral name this builder emits now, and the Linux-only name it
    # emitted before the Windows target existed. The old one is still accepted
    # because packages and preserved receipts carrying it are real evidence, and
    # refusing them would invalidate history to tidy a string.
    "secode.release.build.v1",
    "secode.release.linux-build.v1",
    "secode.daily-use.candidate-build.v1",
}

MANIFEST_SCHEMA = "secode.release.package.v1"

#: The two compatibility aliases, alias -> target, before the target suffix.
#:
#: StateEngine launches `secode` and `<install>/secode-server` by name. The  # grapt-rename: keep
#: package therefore declares these two names as aliases of the grapt binaries,
#: and installation materializes each as a copy of its verified target inside
#: the version's own `bin`. They are NOT entries in `binaries`: the signed
#: inventory stays the five grapt binaries. The installer accepts exactly this
#: map and nothing else, so adding a third alias here is a refused package.
COMPAT_ALIASES = {"secode": "grapt", "secode-server": "grapt-server"}  # grapt-rename: keep
# Installation materializes each alias as a hash-verified byte copy of its
# target (not a link), so every installed version carries two extra binaries
# until the aliases are removed.
COMPAT_ALIASES_REMOVAL = (
    "temporary: removed once StateEngine launches grapt and grapt-server by name"
)
PROJECT_LICENSES = ("LICENSE-MIT", "LICENSE-APACHE")

#: The paths `assembly_provenance` observes. The product source, its dependency
#: pins, and the release tooling that produced the manifest.
ASSEMBLY_SCOPE = (
    "crates", "packages/plugin-bridge", "packages/stateengine", "Cargo.toml", "Cargo.lock",
    "scripts/release", "remote-compute", "docs/release-packaging/QUICKSTART.md",
    "scripts/windows/recover-legacy-sign-in.ps1",
    "notices", *PROJECT_LICENSES, "licenses",
)

#: A single artifact may not exceed this. Measured before any copy.
MAX_ARTIFACT_BYTES = 100 * 1024 * 1024
MAX_PLUGIN_RUNTIME_BYTES = 160 * 1024 * 1024
MAX_RECEIPT_BYTES = 1024 * 1024


class PackageError(RuntimeError):
    """A refusal this packager can name."""


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


def bounded(path: Path, bound: int) -> int:
    """Refuse an oversized input BEFORE copying it.

    Measuring after the copy costs the full disk write for something that was
    always going to be refused.
    """
    if not path.is_file():
        raise PackageError(f"{path} is absent")
    size = path.stat().st_size
    if size > bound:
        raise PackageError(f"{path} is {size} bytes, over the {bound}-byte bound")
    return size


def load_receipt(path: Path) -> dict:
    bounded(path, MAX_RECEIPT_BYTES)
    try:
        receipt = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PackageError(f"{path} could not be read as JSON: {error}") from error
    if not isinstance(receipt, dict):
        raise PackageError(f"{path} is not a JSON object")
    schema = receipt.get("schema")
    if schema not in BUILD_SCHEMAS:
        raise PackageError(
            f"{path} carries schema {schema!r}, which this packager does not "
            f"consume. Known: {sorted(BUILD_SCHEMAS)}"
        )
    return receipt


def required(receipt: dict, path: Path, key: str) -> object:
    if key not in receipt:
        raise PackageError(f"{path} does not carry {key!r}")
    return receipt[key]


def claims(produces: object, name: str) -> bool:
    """Does this recorded command claim to have produced `name`?

    A bare string is one binary; a list is one invocation that produced several.
    Anything else claims NOTHING — and `None` in particular must not read as a
    match, which is how an unaccounted-for binary would slip through.
    """
    if isinstance(produces, str):
        return produces == name
    if isinstance(produces, list):
        return any(isinstance(entry, str) and entry == name for entry in produces)
    return False


def accounted_for(receipt: dict, path: Path, names: list[str]) -> None:
    """Every packaged binary must be claimed by a recorded build command."""
    commands = receipt.get("commands")
    if not isinstance(commands, list):
        raise PackageError(f"{path} does not carry a list of build commands")
    for name in names:
        if not any(claims(step.get("produces"), name) for step in commands if isinstance(step, dict)):
            raise PackageError(
                f"no recorded build command in {path} claims to have produced "
                f"{name!r}: a directory listing is not provenance"
            )


def complete(names: list[str], path: Path, suffix: str = "") -> None:
    """The packaged set must be EXACTLY the declared shipped set.

    `accounted_for` answers "is every binary here explained?" — and a package
    that is simply MISSING one passes it, because every statement about the
    binaries that ARE present is true. That is the defect a provenance record
    assembled entirely out of true statements cannot catch on its own, and it is
    the defect this campaign exists to prevent, so it gets its own refusal.

    A missing entry and an unexpected extra are refused with equal force. An
    extra is not a bonus: it is a binary nobody declared, arriving in a package
    an operator is asked to trust.
    """
    expected = set(shipped.names(suffix))
    packaged = set(names)
    missing = sorted(expected - packaged)
    unexpected = sorted(packaged - expected)
    if missing or unexpected:
        detail = []
        if missing:
            detail.append(f"missing {missing}")
        if unexpected:
            detail.append(f"undeclared {unexpected}")
        raise PackageError(
            f"{path} does not carry the declared shipped set: {'; '.join(detail)}. "
            f"Declared in scripts/release/shipped.py as {shipped.names(suffix)}; "
            f"deliberately excluded: {sorted(shipped.EXCLUDED)}"
        )


def compat_aliases(names: list[str], suffix: str) -> dict[str, str]:
    """The compatibility alias map for this target, each target a shipped binary."""
    aliases = {f"{alias}{suffix}": f"{target}{suffix}" for alias, target in COMPAT_ALIASES.items()}
    for alias, target in aliases.items():
        if target not in names or alias in names:
            raise PackageError(
                f"compatibility alias {alias!r} must name a shipped binary and must not be one"
            )
    return aliases


def custody(receipt: dict, path: Path, root: Path, profile: str, target: str) -> str:
    """Bind the receipt to a clean, matching build, or refuse."""
    # A dirty tree is refused on the RECEIPT's word, not by re-inspecting the
    # checkout now: what matters is the tree AT BUILD TIME, and re-checking here
    # would pass for a tree tidied up after a dirty build.
    if receipt.get("tree_dirty") is not False:
        raise PackageError(
            f"{path} does not attest a clean tree at build time: "
            f"{receipt.get('tree_dirty')!r}"
        )
    if required(receipt, path, "profile") != profile:
        raise PackageError(f"{path} attests profile {receipt['profile']!r}, not {profile!r}")
    if required(receipt, path, "target") != target:
        raise PackageError(f"{path} attests target {receipt['target']!r}, not {target!r}")

    source_commit = required(receipt, path, "source_commit")
    if not isinstance(source_commit, str) or len(source_commit) != 40:
        raise PackageError(f"{path} carries a malformed source_commit: {source_commit!r}")
    known = subprocess.run(
        ["git", "cat-file", "-e", f"{source_commit}^{{commit}}"],
        cwd=root,
        check=False,
        capture_output=True,
    )
    if known.returncode != 0:
        raise PackageError(
            f"{source_commit} is not a commit in {root}: these binaries belong to "
            "a history this checkout cannot describe"
        )
    return source_commit


def signing_state(target: str) -> dict:
    """Per-target signing, reported truthfully.

    THERE IS NO SIGNING IDENTITY IN THIS TREE. A search of CurrentUser/My and
    LocalMachine/My found no code-signing certificate, and no signing
    variables are present. `packages/desktop/src/main/release-signing.ts` and
    `.github/workflows/publish.yml` DO carry Azure/Apple/Tauri integration, but
    the workflow's Windows signer is restricted to another organisation — so
    inherited secret NAMES are not evidence of access, and this function will
    not read one as a capability.

    When signing becomes available this returns the real verifier result. Until
    then it returns the absence, which is a fact rather than a gap to be filled.
    """
    return {
        "signed": False,
        "verifier": None,
        "reason": (
            "no code-signing certificate or signing identity exists in this tree "
            "or in the machine certificate stores; unsigned is reported as "
            "unsigned rather than deferred to a stub verifier"
        ),
        "target": target,
    }


def plugin_runtime(
    root: Path,
    output: Path,
    target: str,
    executable: Path | None,
    archive: Path | None,
    license_path: Path | None,
    required_runtime: bool,
) -> dict:
    """Copy one exact package-owned runtime, its license and the bridge.

    The caller acquires/extracts the official archive in its governed build
    workspace. This function does no network access and accepts no ambient Node.
    """
    supplied = [value is not None for value in (executable, archive, license_path)]
    if not any(supplied):
        if required_runtime:
            raise PackageError("the package requires pinned plugin runtime assets")
        return {
            "status": "not-included",
            "reason": "legacy package invocation did not request plugin compatibility assets",
        }
    if not all(supplied):
        raise PackageError(
            "plugin runtime packaging requires executable, source archive and license together"
        )
    assert executable is not None and archive is not None and license_path is not None
    pins_path = root / "packages/plugin-bridge/runtime-pins.json"
    pins = json.loads(pins_path.read_text(encoding="utf-8"))
    if pins.get("schema") != "grapt.plugin-runtime-pins.v1":
        raise PackageError(f"{pins_path} has an unsupported schema")
    target_pin = pins.get("targets", {}).get(target)
    if not isinstance(target_pin, dict):
        raise PackageError(f"no plugin runtime is pinned for target {target!r}")
    bridge = root / "packages/plugin-bridge/index.mjs"
    bounded(executable, MAX_PLUGIN_RUNTIME_BYTES)
    bounded(archive, MAX_PLUGIN_RUNTIME_BYTES)
    bounded(license_path, MAX_RECEIPT_BYTES)
    bounded(bridge, MAX_RECEIPT_BYTES)
    observed = {
        "executable_sha256": digest(executable)[0],
        "source_sha256": digest(archive)[0],
        "license_sha256": digest(license_path)[0],
    }
    for name, value in observed.items():
        if value != target_pin.get(name):
            raise PackageError(
                f"plugin runtime {name} {value} does not match pin {target_pin.get(name)!r}"
            )
    if executable.stat().st_size != target_pin.get("executable_bytes"):
        raise PackageError("plugin runtime executable size does not match its pin")

    runtime_dir = output / "plugin-runtime"
    bridge_dir = output / "plugin-bridge"
    runtime_dir.mkdir(parents=True)
    bridge_dir.mkdir()
    runtime_name = "node.exe" if target == "x86_64-pc-windows-msvc" else "node"
    packaged_executable = runtime_dir / runtime_name
    shutil.copy2(executable, packaged_executable)
    if target == "x86_64-unknown-linux-gnu":
        packaged_executable.chmod(
            packaged_executable.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH
        )
    packaged_license = runtime_dir / "LICENSE"
    packaged_bridge = bridge_dir / "index.mjs"
    shutil.copy2(license_path, packaged_license)
    shutil.copy2(bridge, packaged_bridge)
    bridge_sha, bridge_bytes = digest(packaged_bridge)
    runtime_manifest = {
        "schema": "grapt.plugin-runtime.v1",
        "target": target,
        "version": pins["version"],
        "executable_sha256": observed["executable_sha256"],
        "bridge_sha256": bridge_sha,
        "source_url": target_pin["source_url"],
        "source_sha256": observed["source_sha256"],
        "license_sha256": observed["license_sha256"],
    }
    runtime_manifest_path = runtime_dir / "runtime.json"
    runtime_manifest_path.write_text(
        json.dumps(runtime_manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    assets = {}
    for relative in [
        f"plugin-runtime/{runtime_name}",
        "plugin-runtime/LICENSE",
        "plugin-runtime/runtime.json",
        "plugin-bridge/index.mjs",
    ]:
        sha, size = digest(output / relative)
        assets[relative] = {"sha256": sha, "bytes": size}
    return {
        "status": "included",
        "version": pins["version"],
        "target": target,
        "source_url": target_pin["source_url"],
        "source_sha256": observed["source_sha256"],
        "bridge_sha256": bridge_sha,
        "bridge_bytes": bridge_bytes,
        "license": pins["license"],
        "assets": assets,
        "policy": {
            "fetch_at_runtime": False,
            "global_install": False,
            "update": "replace only through a newly pinned, requalified release package",
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-root", required=True, type=Path)
    parser.add_argument("--artifacts", required=True, type=Path, help="the build's returned artifacts")
    parser.add_argument("--build-receipt", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path, help="fresh package directory")
    parser.add_argument("--profile", default="release")
    parser.add_argument("--target", required=True)
    parser.add_argument("--plugin-runtime", type=Path)
    parser.add_argument("--plugin-runtime-archive", type=Path)
    parser.add_argument("--plugin-runtime-license", type=Path)
    parser.add_argument("--require-plugin-runtime", action="store_true")
    parser.add_argument("--stateengine-payload", type=Path,
                        help="verified local StateEngine payload for this target; no download")
    parser.add_argument(
        "--publisher",
        default=None,
        help="Windows Authenticode subject; accepted only after live verification",
    )
    arguments = parser.parse_args()

    root = arguments.rust_root.resolve()
    output = arguments.output.resolve()
    if output.exists():
        raise PackageError(
            f"{output} already exists; a package is assembled once, and reusing a "
            "directory would mean this manifest describes bytes another run left"
        )

    receipt_path = arguments.build_receipt.resolve()
    receipt = load_receipt(receipt_path)
    outputs = required(receipt, receipt_path, "outputs")
    if not isinstance(outputs, dict) or not outputs:
        raise PackageError(f"{receipt_path} carries no outputs")

    names = sorted(outputs)
    complete(names, receipt_path, shipped.suffix_for(arguments.target))
    accounted_for(receipt, receipt_path, names)
    source_commit = custody(receipt, receipt_path, root, arguments.profile, arguments.target)

    # PREFLIGHT EVERY INPUT BEFORE ANY COPY.
    sources = {name: (arguments.artifacts / name).resolve() for name in names}
    for name, path in sources.items():
        bounded(path, MAX_ARTIFACT_BYTES)
    quickstart = root / "docs" / "release-packaging" / "QUICKSTART.md"
    bounded(quickstart, 64 * 1024)

    try:
        notice_text = notices.assemble(root)
    except (OSError, ValueError) as error:
        raise PackageError(f"third-party notice fragments are unusable: {error}") from error
    for name in PROJECT_LICENSES:
        bounded(root / name, 1024 * 1024)
    try:
        licenses.verify(root)
    except (OSError, ValueError) as error:
        raise PackageError(f"dependency license register is missing or stale: {error}") from error
    license_root = root / "licenses"
    if not license_root.is_dir() or license_root.is_symlink():
        raise PackageError("dependency license directory is missing or linked")
    license_sources = []
    for path in license_root.glob("*/*"):
        if len(license_sources) >= 256:
            raise PackageError("dependency licenses exceed the 256-file package bound")
        if not path.is_file() or path.is_symlink() or path.parent.is_symlink():
            raise PackageError(f"dependency license is not a regular local file: {path}")
        bounded(path, 1024 * 1024)
        license_sources.append(path)
    if not license_sources:
        raise PackageError("dependency license directory is empty")

    # The pinned runtime executes a version probe during assembly. Its newly
    # created root must exclude other accounts before any payload is copied.
    output.mkdir(mode=0o700, parents=True)
    runtime = plugin_runtime(
        root,
        output,
        arguments.target,
        arguments.plugin_runtime.resolve() if arguments.plugin_runtime else None,
        arguments.plugin_runtime_archive.resolve() if arguments.plugin_runtime_archive else None,
        arguments.plugin_runtime_license.resolve() if arguments.plugin_runtime_license else None,
        arguments.require_plugin_runtime,
    )
    try:
        stateengine = stateengine_payload.assemble(
            root, output, arguments.target, arguments.stateengine_payload)
    except (stateengine_payload.PayloadError, OSError, subprocess.SubprocessError) as error:
        raise PackageError(f"StateEngine payload: {error}") from error
    binaries: dict[str, dict] = {}
    for name in names:
        destination = output / name
        shutil.copyfile(sources[name], destination)
        # THE EXECUTABLE BIT IS PART OF THE ARTIFACT.
        #
        # `shutil.copyfile` copies bytes and nothing else, so a packaged binary
        # arrived non-executable and the provider canary died with
        # `PermissionError` trying to run the product it had just packaged. An
        # operator unpacking this would hit precisely that.
        #
        # The operator recipe used to instruct people to restore the bit by
        # hand. That was documentation standing in for a defect: a release
        # package whose binaries cannot be run is not a release package.
        destination.chmod(0o755)
        # FROM DISK, after the copy: the manifest describes the bytes that are
        # in the package, not the bytes that were in the build directory.
        sha, size = digest(destination)
        attested = outputs[name]
        if not isinstance(attested, dict):
            raise PackageError(f"{receipt_path} outputs[{name!r}] is not an object")
        if attested.get("sha256") != sha or attested.get("bytes") != size:
            raise PackageError(
                f"{name} does not match what {receipt_path} attests: packaged "
                f"{sha} / {size} bytes against attested "
                f"{attested.get('sha256')} / {attested.get('bytes')} bytes"
            )
        binaries[name] = {
            "sha256": sha,
            "bytes": size,
            "package": attested.get("package"),
            "bin": attested.get("bin"),
            "source_commit": source_commit,
            "profile": arguments.profile,
            "target": arguments.target,
            "build_receipt": receipt_path.name,
            "build_receipt_sha256": digest(receipt_path)[0],
        }

    shutil.copyfile(receipt_path, output / receipt_path.name)
    shutil.copyfile(quickstart, output / "QUICKSTART.md")
    (output / "THIRD_PARTY_NOTICES.md").write_bytes(notice_text)
    for name in PROJECT_LICENSES:
        shutil.copyfile(root / name, output / name)
    for source in license_sources:
        destination = output / source.relative_to(root)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)


    # SCOPED, and the scope is published in the manifest.
    #
    # An unscoped `git status` run on the carrier reports the build's own
    # `artifacts/` directory as uncommitted, so every governed package would
    # declare its assembly unclean because of the binaries it is packaging. That
    # is noise that would train a reader to ignore the field.
    #
    # Assembly provenance is about the SOURCE that produced this manifest, so
    # the scope is the source: the crates, the dependency pins, and the release
    # tooling itself. `build_artifacts.py` scopes its own custody check the same way
    # and for the same reason. The scope is written INTO the manifest rather
    # than left implicit -- a narrowed check that does not say what it narrowed
    # to is indistinguishable from a weakened one.
    dirt = subprocess.check_output(
        ["git", "status", "--porcelain", "--", *ASSEMBLY_SCOPE], cwd=root, text=True
    ).rstrip()
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()

    signing = {name: signing_state(arguments.target) for name in names}
    publisher = None
    if arguments.publisher is not None:
        if arguments.target != signing_check.WINDOWS_TARGET:
            raise PackageError("a publisher may be declared only for a Windows package")
        try:
            verified = signing_check.verify_windows_update(
                output, arguments.publisher, binaries
            )
        except signing_check.UpdateTrustError as error:
            raise PackageError(str(error)) from error
        publisher = verified["publisher"]
        signing = {
            name: {"signed": True, **row}
            for name, row in verified["binaries"].items()
        }

    manifest = {
        "schema": MANIFEST_SCHEMA,
        "target": arguments.target,
        "profile": arguments.profile,
        "toolchain": receipt.get("toolchain"),
        # THE IDENTITY. Settled during the migration campaign:
        # the engine identity is the attested source_commit of the binary, not a
        # crate version. `0.1.0` is carried identically by every crate here and
        # could not distinguish two builds, and the string handed to an operator
        # is written PERMANENTLY into a user's database by
        # `grapt-migrate --engine-version` (crates/grapt-store/src/epoch.rs
        # stamps one `engine_epoch` row, and there is no repair path).
        "engine_version": source_commit,
        "engine_version_is": "the attested source_commit of the built binaries",
        "crate_version_secondary": "0.1.0",
        "binaries": binaries,
        "plugin_runtime": runtime,
        "stateengine": stateengine,
        "entry_points": names,
        # Two names StateEngine still launches, as copies of signed binaries.
        # Not inventory entries, not signed separately, and temporary.
        "compat_aliases": compat_aliases(names, shipped.suffix_for(arguments.target)),
        "compat_aliases_removal": COMPAT_ALIASES_REMOVAL,
        # ROLES TRAVEL WITH THE ARTIFACT. A flat list of five names tells an
        # operator recovering a failed activation nothing about which one is the
        # server, and shipping `grapt-engined` without saying what it is would
        # let them reach for a binary that seals an EMPTY router and serves no
        # product route. The packaging disposition requires the manifest and
        # the operator recipe to carry that distinction, so it is a manifest
        # field rather than a sentence in a document the operator may not have.
        "entry_point_roles": shipped.roles(shipped.suffix_for(arguments.target)),
        # Named, not omitted: a binary that exists in the workspace and is
        # deliberately not shipped should be distinguishable from one that was
        # forgotten. In a list carrying only what is included, those two look
        # exactly alike.
        "excluded_binaries": shipped.EXCLUDED,
        # Null is an explicit absence, not a publisher declaration. A non-null
        # publisher reaches this manifest only after Authenticode verified every
        # exact packaged file as Valid under that subject.
        "publisher": publisher,
        "signing": signing,
        # The commit that ASSEMBLED this package, with its dirt. A checkout with
        # uncommitted changes is NOT described by its HEAD, and saying so is the
        # difference between a provenance record and a plausible one.
        "assembly_provenance": {
            "commit": head,
            "clean": not dirt,
            "uncommitted_paths": sorted(line[3:] for line in dirt.splitlines() if len(line) > 3)[:32],
            "observed_paths": list(ASSEMBLY_SCOPE),
            "observed_paths_is": (
                "the scope this cleanliness claim covers. The build's own "
                "artifacts/ output is NOT source drift and is outside it"
            ),
        },
        "attests": (
            "these are the bytes in this package, hashed from disk after the "
            "copy, each accounted for by a recorded build command and matching "
            "the digest its build receipt attests. NOT a signature, NOT a "
            "reproducibility claim, and NOT an accepted distribution."
        ),
    }
    (output / "release-manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(json.dumps({"package": str(output), "engine_version": source_commit,
                      "entry_points": names}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except PackageError as error:
        print(f"release package: {error}", file=sys.stderr)
        raise SystemExit(2) from error
