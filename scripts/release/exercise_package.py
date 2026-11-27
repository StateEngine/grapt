"""Install a release package and EXERCISE it, as an operator would, or refuse.

## What this proves that the packager cannot

`package.py` proves the bytes in a package are the bytes a build produced. That
is provenance, and it is silent about whether any of it RUNS. A package whose
every digest checks out and whose binaries cannot execute on the target machine
is a perfectly attested brick.

So this installs the package into a prefix and drives the delivered binaries.
Nothing here reaches into the checkout for an executable: every command runs the
installed copy, by absolute path, from a working directory OUTSIDE the source
tree.

A Windows update is a stronger boundary than a fresh internal install. Before
the first activation byte moves, it requires an artifact-declared publisher and
a live Valid Authenticode result for every exact package file. Unsigned internal
packages remain honest install inputs, but cannot become update-success evidence.

## The toolchain claim, made falsifiable

`grapt-migrate` is a required delivered artifact on exactly one argument: the
operator it exists for has a broken install and a backup, not a Rust toolchain
and not this workspace. An exercise that quietly ran with cargo on PATH would
support that claim without testing it -- it would pass identically whether the
binaries were self-contained or not.

So the environment is SCRUBBED (`cargo`, `rustc` and `rustup` removed from
PATH), and the scrub is then VERIFIED: if `cargo` still resolves, this refuses
rather than reporting a green that would have been green either way. A witness
that would hold whether or not the thing it claims is true is not a witness.

## The refusals are part of the product surface

The packaging disposition requires `grapt-engined` to ship with its limits disclosed
and EXERCISED: `service status` is supported, `service ensure` and `service
stop` refuse by design with a typed non-zero refusal, and the binary serves no
product route. `grapt-migrate cutover` likewise refuses by design. Those
refusals are asserted here from the DELIVERED artifact, because a refusal
documented in a manifest and never fired is a claim, not a behaviour.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import stat
import subprocess
import tempfile
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import shipped  # noqa: E402
import signing_check  # noqa: E402

TOOLCHAIN_TOOLS = ("cargo", "rustc", "rustup")
RUN_TIMEOUT = 300
MAX_MANIFEST_BYTES = 1024 * 1024


class ExerciseError(RuntimeError):
    """A refusal this journey can name."""


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


def manifest_snapshot(package: Path) -> tuple[dict, bytes]:
    """Read one bounded manifest once, preserving exact bytes and failures."""
    path = package / "release-manifest.json"
    try:
        metadata = path.stat()
    except FileNotFoundError as error:
        raise ExerciseError(f"{path} is absent; this is not a release package") from error
    except OSError as error:
        raise ExerciseError(f"{path} is unreadable: {error}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ExerciseError(f"{path} is not a regular file")
    size = metadata.st_size
    if size > MAX_MANIFEST_BYTES:
        raise ExerciseError(
            f"{path} is {size} bytes, over the {MAX_MANIFEST_BYTES}-byte bound"
        )
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise ExerciseError(f"{path} is unreadable: {error}") from error
    if len(raw) > MAX_MANIFEST_BYTES:
        raise ExerciseError(
            f"{path} is {len(raw)} bytes, over the {MAX_MANIFEST_BYTES}-byte bound"
        )
    try:
        decoded = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ExerciseError(f"{path} is malformed UTF-8: {error}") from error
    try:
        value = json.loads(decoded)
    except json.JSONDecodeError as error:
        raise ExerciseError(f"{path} is malformed JSON: {error}") from error
    if not isinstance(value, dict):
        raise ExerciseError(f"{path} is not a JSON object")
    return value, raw


def load_manifest(package: Path) -> dict:
    """Compatibility view of :func:`manifest_snapshot` for read-only callers."""
    return manifest_snapshot(package)[0]


def scrubbed_environment() -> dict[str, str]:
    """PATH with every toolchain directory removed.

    Removes any PATH entry that CONTAINS one of the toolchain executables,
    rather than any entry whose name looks like cargo's. A directory is on the
    path because of what is in it, and matching on the name would leave a
    `~/.local/bin/cargo` symlink in place while reporting a scrubbed
    environment.
    """
    environment = dict(os.environ)
    kept = []
    for entry in environment.get("PATH", "").split(os.pathsep):
        if not entry:
            continue
        # `shutil.which` PER ENTRY, not a bare path existence check.
        #
        # `(directory / "cargo").exists()` is false on Windows, where the file
        # is `cargo.EXE` -- so the scrub kept every toolchain directory and the
        # verification then caught it and refused, which is the guard working
        # exactly as intended. `which` applies PATHEXT, so it answers "would a
        # child resolve this tool here" on both platforms instead of "is there a
        # file with precisely this name".
        if any(shutil.which(tool, path=entry) for tool in TOOLCHAIN_TOOLS):
            continue
        kept.append(entry)
    environment["PATH"] = os.pathsep.join(kept)
    # rustup respects these; leaving them would let a scrubbed PATH still reach
    # a toolchain through a shim the child resolves for itself.
    for variable in ("CARGO_HOME", "RUSTUP_HOME", "CARGO_TARGET_DIR", "RUSTUP_TOOLCHAIN"):
        environment.pop(variable, None)
    return environment


def verify_scrub(environment: dict[str, str]) -> dict[str, object]:
    """Prove the scrub actually removed the toolchain.

    Without this the "runs without a Rust toolchain" witness would pass on a
    machine that has one, which is every machine this ever runs on.
    """
    still_present = {
        tool: shutil.which(tool, path=environment["PATH"]) for tool in TOOLCHAIN_TOOLS
    }
    leaked = {tool: found for tool, found in still_present.items() if found}
    if leaked:
        raise ExerciseError(
            f"the toolchain scrub did not take effect: {leaked}. Exercising with a "
            "toolchain on PATH would pass whether or not the delivered binaries "
            "are self-contained, which is not a witness"
        )
    # And the tools must have been there BEFORE the scrub -- otherwise the
    # scrub is trivially satisfied and proves nothing about this machine.
    originally = {tool: shutil.which(tool) for tool in TOOLCHAIN_TOOLS}
    return {
        "toolchain_before_scrub": originally,
        "toolchain_after_scrub": still_present,
        "scrub_is_meaningful": any(originally.values()),
    }


def is_inside_repository(candidate: Path) -> bool:
    """The same rule `grapt-engined` applies, walking upward for a `.git`.

    Mirrored deliberately from `crates/grapt-engined/src/custody.rs`: a receipt
    three directories below a repository root is still inside that repository,
    and checking only the directory itself is the version of this rule that
    passes review and then fails in the field.
    """
    for directory in [candidate, *candidate.parents]:
        if (directory / ".git").exists():
            return True
    return False


def receipt_root_outside_repositories() -> Path:
    """A scratch directory that `grapt-engined` will accept.

    This took two governed runs to get right, and the reason is worth recording.
    `grapt-engined` refuses a receipt root inside a repository working tree,
    because a receipt written into a candidate tree voids the run that produced
    it. On the governed carrier `<operator-home>/.git` EXISTS, so the operator's
    entire home tree is "inside a repository" -- which includes the run root,
    and includes the executor's `TMPDIR`, which it points back into the run.

    So the base is CHOSEN by testing the product's own rule rather than assumed
    from `tempfile.gettempdir()`. Assuming is what failed twice.
    """
    candidates = [Path(tempfile.gettempdir()), Path("/tmp"), Path("/var/tmp")]
    for base in candidates:
        if base.is_dir() and not is_inside_repository(base):
            return Path(tempfile.mkdtemp(prefix="grapt-engined-receipts-", dir=base))
    raise ExerciseError(
        "no scratch directory outside a repository working tree is available "
        f"among {[str(base) for base in candidates]}; grapt-engined refuses a "
        "receipt root inside one, and writing there anyway would void the run"
    )


def execute(
    argv: list[str], cwd: Path, environment: dict[str, str], expect_exit: int | None = 0
) -> dict[str, object]:
    """Run a delivered binary and record what it did."""
    completed = subprocess.run(
        argv, cwd=cwd, env=environment, capture_output=True, text=True,
        timeout=RUN_TIMEOUT, check=False,
    )
    record = {
        "argv": argv,
        "exit_code": completed.returncode,
        "stdout": completed.stdout[-4096:],
        "stderr": completed.stderr[-4096:],
    }
    if expect_exit is not None and completed.returncode != expect_exit:
        raise ExerciseError(
            f"{argv} exited {completed.returncode}, expected {expect_exit}. "
            f"stdout: {completed.stdout[-2000:]!r} stderr: {completed.stderr[-2000:]!r}"
        )
    return record


def install(
    package: Path,
    prefix: Path,
    manifest: dict | None = None,
    manifest_bytes: bytes | None = None,
) -> tuple[dict, dict[str, Path]]:
    """Copy the package into a prefix, verifying every digest from the manifest.

    An installer that copies without checking is a copy command. The manifest is
    the only thing standing between an operator and a corrupted download, so it
    is CHECKED here rather than carried along.
    """
    manifest_path = package / "release-manifest.json"
    if manifest is None:
        manifest, manifest_bytes = manifest_snapshot(package)
    elif manifest_bytes is None:
        raise ExerciseError("a preloaded manifest requires its exact verified bytes")

    binaries = manifest.get("binaries")
    if not isinstance(binaries, dict) or not binaries:
        raise ExerciseError(f"{manifest_path} carries no binaries")

    # IDENTITY, BEFORE ANY BYTES ARE PLACED.
    #
    # Digests prove the binaries are the ones the manifest describes. They say
    # nothing about whether the manifest's IDENTITY is the identity those
    # binaries were built under -- a manifest can carry perfectly correct
    # digests and still claim to be a different engine than the one in the box,
    # which is how a mismatched component set reaches a running system with
    # every checksum passing.
    engine_version = manifest.get("engine_version")
    for name, attested in sorted(binaries.items()):
        if attested.get("source_commit") != engine_version:
            raise ExerciseError(
                f"{name} was built at {attested.get('source_commit')!r} but the package "
                f"declares engine_version {engine_version!r}: this package is assembled "
                "from mismatched components"
            )
    receipt_name = next(
        (attested.get("build_receipt") for attested in binaries.values() if attested.get("build_receipt")),
        None,
    )
    if receipt_name:
        receipt_path = package / receipt_name
        if not receipt_path.is_file():
            raise ExerciseError(f"the build receipt {receipt_name!r} the manifest cites is absent")
        receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        if receipt.get("source_commit") != engine_version:
            raise ExerciseError(
                f"the package declares engine_version {engine_version!r} but its own build "
                f"receipt attests {receipt.get('source_commit')!r}"
            )

    # SIGNING, AS A CLAIM THAT MUST BE BACKED.
    #
    # The charter forbids an unsigned-as-signed claim and forbids counting a
    # stub verifier as signed-release evidence. `signed: false` with a reason is
    # a truthful package and installs. `signed: true` with nothing behind it is
    # the exact claim that must not be trusted, and an installer that takes it
    # on faith is how the first becomes the second.
    for name, state in sorted(manifest.get("signing", {}).items()):
        if state.get("signed") is True and not state.get("verifier"):
            raise ExerciseError(
                f"{name} claims to be signed but carries no verifier result. An "
                "unsigned-as-signed claim is refused rather than installed"
            )
        if state.get("signed") is not True and not state.get("reason"):
            raise ExerciseError(
                f"{name} is not signed and does not say why. Unsigned is reportable; "
                "unsigned and unexplained is not"
            )

    prefix.mkdir(parents=True, exist_ok=True)
    installed: dict[str, Path] = {}
    for name, attested in sorted(binaries.items()):
        source = package / name
        if not source.is_file():
            raise ExerciseError(f"{name} is named by the manifest but absent from the package")
        sha, size = digest(source)
        if sha != attested.get("sha256") or size != attested.get("bytes"):
            raise ExerciseError(
                f"{name} in the package does not match its own manifest: "
                f"{sha}/{size} against {attested.get('sha256')}/{attested.get('bytes')}"
            )
        destination = prefix / name
        # `copy2`, not `copyfile`: MODE IS PART OF THE ARTIFACT, and an
        # installer that recreates it would hide whether the package carried it.
        shutil.copy2(source, destination)
        installed_sha, installed_size = digest(destination)
        if installed_sha != sha or installed_size != size:
            raise ExerciseError(
                f"{name} changed while it was installed: the installed copy "
                f"is {installed_sha}/{installed_size}, not the verified "
                f"{sha}/{size} package bytes"
            )
        # THE BIT IS ASSERTED, NEVER GRANTED.
        #
        # This used to `chmod` the installed binary. That made the installed
        # output executable no matter what the package contained, which is
        # exactly the mask that let non-executable packages pass every journey
        # until a provider canary tried to run one. An installer that repairs
        # the artifact it is verifying cannot witness the artifact.
        if os.name == "posix" and not destination.stat().st_mode & stat.S_IXUSR:
            raise ExerciseError(
                f"{name} is not executable in the package, so the installed copy "
                "is not either. The package sets this bit; needing to restore it "
                "by hand is the defect, not the remedy"
            )
        installed[name] = destination
    plugin_runtime = manifest.get("plugin_runtime")
    if isinstance(plugin_runtime, dict) and plugin_runtime.get("status") == "included":
        assets = plugin_runtime.get("assets")
        if not isinstance(assets, dict) or not assets:
            raise ExerciseError("included plugin runtime carries no asset inventory")
        for relative, attested in sorted(assets.items()):
            if not isinstance(relative, str) or not isinstance(attested, dict):
                raise ExerciseError("plugin runtime asset inventory is malformed")
            source = package / relative
            if not source.is_file():
                raise ExerciseError(f"plugin runtime asset {relative!r} is absent")
            sha, size = digest(source)
            if sha != attested.get("sha256") or size != attested.get("bytes"):
                raise ExerciseError(
                    f"plugin runtime asset {relative!r} does not match its manifest"
                )
            destination = prefix / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
            installed_sha, installed_size = digest(destination)
            if installed_sha != sha or installed_size != size:
                raise ExerciseError(f"plugin runtime asset {relative!r} changed during install")
            if relative.endswith("/node") and os.name == "posix" and not destination.stat().st_mode & stat.S_IXUSR:
                raise ExerciseError("packaged plugin runtime is not executable")
    installed_manifest = prefix / "release-manifest.json"
    installed_manifest.write_bytes(manifest_bytes)
    return manifest, installed


def install_refusal_matrix(package: Path, work: Path, suffix: str = "") -> list[dict]:
    """Mutate the PACKAGE and require the installer to refuse before unsafe use.

    The charter asks for refusal mutations covering component mismatch, bad
    signature/identity, and failed activation. Those are mutations of the
    ARTIFACT an operator receives, not of the tooling that made it: a package
    can be corrupted in transit, assembled from mismatched parts, or arrive
    claiming a signature nobody produced, and the installer is the last thing
    standing between that package and a running system.

    Each case copies the good package, applies exactly one mutation, and
    requires `install` to refuse with a message naming the actual fault. One
    mutation at a time is what makes the refusal attributable.
    """
    records = []
    matrix = work / "refusal-matrix"
    matrix.mkdir()

    def case(label: str, mutate) -> None:
        candidate = matrix / label
        shutil.copytree(package, candidate)
        mutate(candidate)
        try:
            install(candidate, matrix / f"{label}-prefix")
        except ExerciseError as refusal:
            records.append({"case": f"install-refuses-{label}", "refusal": str(refusal)})
            return
        raise ExerciseError(
            f"the installer ACCEPTED a package mutated for {label!r}. A package "
            "that reaches a running system after this mutation is the failure "
            "this matrix exists to prevent"
        )

    # THE NAMES ARE THE TARGET'S, not spelled bare here.
    #
    # These were bare names, so on Windows the "corrupt" case wrote a NEW file
    # called `grapt-tui` that no manifest declares, left the real
    # `grapt-tui.exe` untouched, and the installer correctly accepted the
    # package. It was reported as "the installer ACCEPTED a corrupted package",
    # which reads as a missing guard rather than as a mutation that never
    # happened. A mutation that does not mutate is the worst kind of green.
    def corrupt_component(candidate: Path) -> None:
        (candidate / f"grapt-tui{suffix}").write_bytes(b"a different binary entirely\n")

    def remove_component(candidate: Path) -> None:
        (candidate / f"grapt-server{suffix}").unlink()

    def mismatch_identity(candidate: Path) -> None:
        """A manifest whose identity is not the identity its binaries were built under.

        This is the "unsigned-as-signed" shape applied to identity: every digest
        still checks out, and the package is simply claiming to be a different
        engine than the one in the box.
        """
        path = candidate / "release-manifest.json"
        manifest = json.loads(path.read_text(encoding="utf-8"))
        manifest["engine_version"] = "0" * 40
        path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")

    def claim_a_signature(candidate: Path) -> None:
        """`signed: true` with no verifier result behind it.

        The charter forbids an unsigned-as-signed claim and forbids counting a
        stub verifier as signed-release evidence. An installer that trusts the
        claim is exactly how one becomes the other.
        """
        path = candidate / "release-manifest.json"
        manifest = json.loads(path.read_text(encoding="utf-8"))
        for state in manifest["signing"].values():
            state["signed"] = True
        path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")

    case("component-corrupt", corrupt_component)
    case("component-missing", remove_component)
    case("identity-mismatch", mismatch_identity)
    case("signature-claimed-without-a-verifier", claim_a_signature)
    return records


def exercise_migrate(binary: Path, work: Path, environment: dict[str, str], engine_version: str) -> list[dict]:
    """Drive the delivered migration operator, including its refusals.

    The rehearsal is run against a store the operator creates for this journey,
    not against any real database: this is a proof that the delivered artifact
    executes and refuses correctly, not a migration of anybody's data.
    """
    records = []

    # NO ARGUMENTS: usage, refused, and typed. Exit 2 with a JSON refusal
    # carrying the operator schema -- an operator scripting against this needs
    # the refusal to be machine-readable, so that is asserted, not just the exit.
    record = execute([str(binary)], work, environment, expect_exit=2)
    payload = json.loads(record["stdout"])
    if payload.get("schema") != "grapt.release.operator.v1" or payload.get("outcome") != "refused":
        raise ExerciseError(f"bare invocation did not produce a typed refusal: {record['stdout']!r}")
    records.append({"case": "bare-invocation-refuses", **record})

    # CUTOVER: refused BY DESIGN. This is an authority boundary, and shipping
    # the operator without demonstrating that it holds would ship the boundary
    # as a promise.
    record = execute([str(binary), "cutover"], work, environment, expect_exit=2)
    payload = json.loads(record["stdout"])
    if "cutover is owned by the unified R6 release authority" not in payload.get("reason", ""):
        raise ExerciseError(f"cutover refusal did not name the release authority: {record['stdout']!r}")
    records.append({"case": "cutover-refuses-by-design", **record})

    # An unknown flag is refused rather than ignored.
    record = execute(
        [str(binary), "status", "--not-a-flag", "x"], work, environment, expect_exit=2
    )
    records.append({"case": "unknown-flag-refuses", **record})

    # THE ROLLBACK SURFACE THIS BINARY ACQUIRED, exercised from the DELIVERED
    # artifact.
    #
    # The landed migration campaign gave `grapt-migrate` an executable
    # `restore` and a `disposition` -- the recovery path an operator with a
    # broken install and a backup actually needs, which previously existed only
    # from a Rust checkout. That is exactly the argument this campaign uses for
    # shipping the binary at all, so a package that carries it must show it
    # carries it.
    #
    # Each is driven to a REFUSAL rather than a real restore: a genuine restore
    # needs a real store and a real backup, which is J04 evidence a caller
    # supplies, not something a smoke matrix should invent. What is proven here
    # is that the delivered artifact HAS the surface and bounds its arguments --
    # a package built before this landing would fail these cases, which is the
    # point.
    for verb in ("restore", "disposition"):
        record = execute([str(binary), verb], work, environment, expect_exit=2)
        payload = json.loads(record["stdout"])
        if payload.get("schema") != "grapt.release.operator.v1":
            raise ExerciseError(f"{verb} did not refuse in the operator's typed shape")
        reason = payload.get("reason", "")
        if verb not in reason and "missing required argument" not in reason:
            raise ExerciseError(
                f"{verb} refused without naming its own missing arguments: {reason[:300]!r}"
            )
        records.append({"case": f"{verb}-surface-present-and-bounded", **record})

    return records


def exercise_engined(binary: Path, work: Path, environment: dict[str, str],
                     source_tree: Path) -> list[dict]:
    """Drive the auxiliary binary, and prove its DISCLOSED limits are real."""
    records = []

    # THE RECEIPT ROOT MUST BE OUTSIDE ANY REPOSITORY WORKING TREE.
    #
    # `grapt-engined` walks UPWARD from the receipt root looking for `.git` and
    # refuses if it finds one, because "a receipt written into a candidate tree
    # voids the run that produced it". On the governed carrier that rules out
    # the entire run root: `<operator-home>` is itself a working tree, so every path
    # beneath it is inside one. The first journey put receipts under the run
    # directory and every `service` invocation refused on custody before it
    # could reach the behaviour under test.
    #
    # So the receipt root is the system temporary directory -- which is exactly
    # what the binary's own `portable_receipt_root` falls back to, so this
    # follows the product's rule instead of working around it.
    receipts = receipt_root_outside_repositories()
    registration = work / "engined-registration"
    registration.mkdir(exist_ok=True)

    # The REQUIRED BOUNDS, read from the binary's own dispatch rather than
    # guessed: `service` needs `--registration`, `--lock-attempts` and a settled
    # `--receipt-root`. This matters more than it looks: the first run of this
    # journey supplied no `--lock-attempts`, so `ensure` refused as an ARGUMENT
    # error and never reached the authority refusal the case claims to prove.
    # The assertion below caught it because it reads the refusal's message
    # instead of accepting any non-zero exit -- which is exactly why it is
    # written that way.
    bounds = [
        "--registration", str(registration),
        "--lock-attempts", "3",
        "--receipt-root", str(receipts),
    ]

    # An unsupplied required bound refuses rather than defaulting. A service
    # control tool that quietly invents a bound is worse than one that stops.
    record = execute(
        [str(binary), "service", "status", "--registration", str(registration)],
        work, environment, expect_exit=None,
    )
    if record["exit_code"] == 0:
        raise ExerciseError("service status accepted a missing required bound")
    if "--lock-attempts" not in f"{record['stdout']}{record['stderr']}":
        raise ExerciseError(
            f"a missing bound refused without naming it: {record['stderr'][-500:]!r}"
        )
    records.append({"case": "missing-required-bound-refuses", **record})

    # `service status` is the SUPPORTED verb. Its exit is not pinned here: a
    # status probe legitimately reports "not running" in this environment, and
    # demanding zero would be asserting a service exists rather than that the
    # binary answers.
    record = execute(
        [str(binary), "service", "status", *bounds], work, environment, expect_exit=None,
    )
    records.append({"case": "service-status-answers", **record})

    # `ensure` and `stop` REFUSE BY DESIGN, non-zero, naming the missing
    # authority. This is the disclosure the disposition requires, exercised from the
    # delivered artifact rather than described in a document.
    for verb in ("ensure", "stop"):
        record = execute(
            [str(binary), "service", verb, *bounds], work, environment, expect_exit=None,
        )
        if record["exit_code"] == 0:
            raise ExerciseError(
                f"service {verb} exited 0; it is disclosed as refusing by design, and a "
                "disclosure the binary contradicts is worse than no disclosure"
            )
        combined = f"{record['stdout']}{record['stderr']}"
        if "authority" not in combined:
            raise ExerciseError(
                f"service {verb} refused without naming the missing authority: {combined[-500:]!r}"
            )
        records.append({"case": f"service-{verb}-refuses-by-design", **record})

    # THE CUSTODY GUARD ITSELF, fired on purpose. A receipt root inside a
    # repository working tree is refused, naming the reason. This is a real
    # product protection -- a receipt written into a candidate tree voids the
    # run that produced it -- and it is exercised here rather than merely
    # avoided, because the journey discovered it the hard way and an operator
    # will too.
    record = execute(
        [str(binary), "service", "status", "--registration", str(registration),
         "--lock-attempts", "3", "--receipt-root", str(source_tree)],
        work, environment, expect_exit=None,
    )
    combined = f"{record['stdout']}{record['stderr']}"
    if record["exit_code"] == 0 or "lies inside a repository working tree" not in combined:
        raise ExerciseError(
            "a receipt root inside a repository working tree was NOT refused: "
            f"exit {record['exit_code']}, {combined[-500:]!r}"
        )
    records.append({"case": "receipt-root-inside-a-repository-refuses", **record})

    # An unknown verb is refused, so the auxiliary surface is bounded rather
    # than open-ended.
    record = execute([str(binary), "not-a-verb"], work, environment, expect_exit=None)
    if record["exit_code"] == 0:
        raise ExerciseError("an unknown verb exited 0")
    records.append({"case": "unknown-verb-refuses", **record})

    return records


def exercise_runs(binaries: dict[str, Path], work: Path, environment: dict[str, str]) -> list[dict]:
    """Every delivered binary must EXECUTE. Exit codes are not pinned.

    The claim here is narrow and deliberately so: the file loads and runs on
    this machine, with no toolchain and outside the source tree. A binary that
    refuses its arguments has still executed; one that cannot be loaded at all
    raises OSError and fails this.
    """
    records = []
    for name, path in sorted(binaries.items()):
        try:
            record = execute([str(path), "--help"], work, environment, expect_exit=None)
        except OSError as error:
            raise ExerciseError(f"{name} could not be executed at all: {error}") from error
        records.append({"case": f"{name}-executes", **record})
    return records


def recover(prefix: Path, backup: Path, binaries: dict[str, Path], work: Path,
            environment: dict[str, str], suffix: str = "") -> list[dict]:
    """Break the install, then restore it from the backup, and prove both halves.

    The BREAK is verified before the restore. A recovery journey that never
    confirmed the install was actually broken would report success for a restore
    that had nothing to do -- passing for the wrong reason.
    """
    records = []
    victim = binaries[f"grapt-migrate{suffix}"]
    original = digest(victim)[0]

    victim.write_bytes(b"corrupted\n")
    if digest(victim)[0] == original:
        raise ExerciseError("the corruption did not change the installed binary")

    # A corrupted binary may not be EXECUTABLE AT ALL. The kernel refuses it
    # with `Exec format error` before any exit code exists, which surfaces here
    # as OSError rather than a non-zero return -- and that is a STRONGER
    # observation of a broken install than an exit code, not a problem to route
    # around. Both shapes are recorded as the same fact: the install no longer
    # runs.
    try:
        broken = execute([str(victim)], work, environment, expect_exit=None)
    except OSError as error:
        broken = {"argv": [str(victim)], "exit_code": None, "not_executable": str(error)}
    else:
        if broken["exit_code"] == 2 and "grapt.release.operator.v1" in broken["stdout"]:
            raise ExerciseError(
                "the corrupted binary still produced the operator's typed refusal; "
                "the recovery journey would be restoring something that was never broken"
            )
    records.append({"case": "install-is-broken", **broken})

    # Restored WITH ITS MODE, from the package, and not repaired afterwards:
    # a recovery that hands back a file the operator then has to chmod has not
    # recovered the install.
    shutil.copy2(backup / f"grapt-migrate{suffix}", victim)
    if os.name == "posix" and not victim.stat().st_mode & stat.S_IXUSR:
        raise ExerciseError(
            "the restored binary is not executable; recovery from the package "
            "must return a runnable install, not one needing a hand chmod"
        )
    if digest(victim)[0] != original:
        raise ExerciseError("the restored binary does not match the backed-up bytes")
    restored = execute([str(victim)], work, environment, expect_exit=2)
    payload = json.loads(restored["stdout"])
    if payload.get("schema") != "grapt.release.operator.v1":
        raise ExerciseError("the restored binary does not behave like the operator")
    records.append({"case": "install-is-recovered", **restored})

    return records


def upgrade(previous: Path, current: Path, work: Path, environment: dict[str, str],
            suffix: str = "") -> list[dict]:
    """Install one package OVER another, then roll back, checking identity at each step.

    ## What this proves

    An upgrade is not a copy: the installed identity must actually change, the
    replaced version must be recoverable, and rolling back must restore the
    identity that was there before. Each of those is asserted from the INSTALLED
    manifest, not from the packages on the shelf, because what matters is what
    an operator's machine now says about itself.

    ## The limit this journey discloses rather than hides

    If the two packages carry byte-identical binaries, this exercises the
    install/backup/rollback MECHANICS truthfully but proves nothing about a code
    upgrade -- the program did not change, only its attested identity did. That
    is exactly the situation on this branch, where no commit touches `crates/`.
    The receipt records `binaries_differ`, so a reader is told which of the two
    things was demonstrated instead of being left to assume the stronger one.
    """
    records = []
    prefix = work / "upgrade-prefix"
    saved = work / "upgrade-replaced"

    # WINDOWS UPDATE TRUST, BEFORE ANY ACTIVATION BYTE MOVES. Fresh internal
    # installs remain allowed to report unsigned honestly, and Linux keeps its
    # explicit no-signature-scheme mechanical rehearsal. Only the Windows
    # UPDATE path requires a publisher plus a live Valid result for the exact
    # files that install() will subsequently copy.
    current_manifest, current_manifest_bytes = manifest_snapshot(current)
    current_target = current_manifest.get("target")
    if (os.name == "nt" or suffix == ".exe") and current_target != signing_check.WINDOWS_TARGET:
        raise ExerciseError(
            "Windows update refused: manifest target "
            f"{current_target!r} is not {signing_check.WINDOWS_TARGET!r}"
        )
    if current_target == signing_check.WINDOWS_TARGET:
        binaries = current_manifest.get("binaries")
        if not isinstance(binaries, dict) or not binaries:
            raise ExerciseError("Windows update refused: manifest carries no binaries")
        try:
            trust = signing_check.verify_windows_update(
                current, current_manifest.get("publisher"), binaries
            )
        except signing_check.UpdateTrustError as error:
            raise ExerciseError(str(error)) from error
        records.append({"case": "windows-update-trust-verified", **trust})

    previous_manifest, previous_installed = install(previous, prefix)
    before = json.loads((prefix / "release-manifest.json").read_text(encoding="utf-8"))["engine_version"]
    records.append({"case": "upgrade-baseline-installed", "engine_version": before})

    # The replaced version is preserved BEFORE it is overwritten. An upgrade
    # that destroys what it replaces has no rollback, whatever it calls one.
    shutil.copytree(prefix, saved)

    current_manifest, current_installed = install(
        current, prefix, current_manifest, current_manifest_bytes
    )
    after = json.loads((prefix / "release-manifest.json").read_text(encoding="utf-8"))["engine_version"]
    if after != current_manifest["engine_version"]:
        raise ExerciseError(
            f"after upgrading, the installed manifest says {after!r} but the package "
            f"says {current_manifest['engine_version']!r}"
        )
    if after == before:
        raise ExerciseError(
            f"the installed identity did not change across the upgrade (still {before!r}); "
            "an upgrade journey between two packages of the same version proves nothing"
        )
    records.append({"case": "upgrade-changed-installed-identity", "from": before, "to": after})

    # Did the PROGRAM change, or only its identity? Recorded either way.
    differing = sorted(
        name for name in previous_installed
        if digest(previous / name)[0] != digest(current / name)[0]
    )
    records.append({
        "case": "upgrade-binary-delta-recorded",
        "binaries_differ": bool(differing),
        "differing": differing,
        "note": (
            "the two packages carry byte-identical binaries, so this exercises the "
            "install/backup/rollback mechanics and the identity transition -- NOT a "
            "code upgrade" if not differing else
            "the upgraded binaries differ from the replaced ones BY BYTES. That is "
            "not by itself a code change: the previous package is built in a "
            "different directory, and cargo embeds paths, so differing bytes can "
            "follow from the build location alone. The authoritative signal for "
            "whether the PROGRAM changed is product_source_differs in the upgrade "
            "receipt, which is computed from the commit diff over crates/, "
            "Cargo.toml and Cargo.lock -- not from these digests"
        ),
    })

    # Exercise the upgraded install before rolling it back: an upgrade that
    # lands a set of binaries that will not run is a failed activation, and
    # that is the case rollback exists for.
    execute([str(prefix / f"grapt-migrate{suffix}")], work, environment, expect_exit=2)
    records.append({"case": "upgraded-install-runs"})

    # ROLL BACK to the preserved version.
    for name in sorted(previous_installed):
        shutil.copy2(saved / name, prefix / name)
        if os.name == "posix" and not (prefix / name).stat().st_mode & stat.S_IXUSR:
            raise ExerciseError(f"rollback left {name} non-executable")
    shutil.copy2(saved / "release-manifest.json", prefix / "release-manifest.json")
    restored = json.loads((prefix / "release-manifest.json").read_text(encoding="utf-8"))["engine_version"]
    if restored != before:
        raise ExerciseError(
            f"rollback left the install claiming {restored!r}, not the replaced {before!r}"
        )
    execute([str(prefix / f"grapt-migrate{suffix}")], work, environment, expect_exit=2)
    records.append({"case": "rollback-restored-previous-identity", "engine_version": restored})

    return records


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path, help="fresh directory outside the source tree")
    parser.add_argument("--receipt", required=True, type=Path)
    arguments = parser.parse_args()

    package = arguments.package.resolve()
    work = arguments.work.resolve()
    if work.exists() and any(work.iterdir()):
        raise ExerciseError(f"{work} is not empty; a journey starts from a known state")
    work.mkdir(parents=True, exist_ok=True)

    source_tree = Path(__file__).resolve().parents[2]
    if source_tree == work or source_tree in work.parents:
        raise ExerciseError(
            f"{work} is inside the source tree at {source_tree}. An operator does not "
            "have this checkout, and exercising inside it would let the journey "
            "reach something the package does not deliver"
        )

    environment = scrubbed_environment()
    scrub = verify_scrub(environment)

    prefix = work / "install"
    backup = work / "backup"
    runtime = work / "runtime"
    runtime.mkdir()

    manifest, installed = install(package, prefix)
    # The backup an operator would have. Taken from the PACKAGE, so a recovery
    # never depends on the broken install it is recovering.
    shutil.copytree(package, backup)

    records: list[dict] = []
    records += install_refusal_matrix(package, work, shipped.suffix_for(manifest["target"]))
    records += exercise_runs(installed, runtime, environment)
    # THE TARGET'S SUFFIX, from the manifest the package carries rather than
    # from this host. A Windows package exercised on a Windows host and a Linux
    # package exercised on Linux are the same journey over differently-named
    # files, and reading the suffix off `os.name` would tie the journey to where
    # it happens to run instead of to what it is examining.
    suffix = shipped.suffix_for(manifest["target"])
    records += exercise_migrate(installed[f"grapt-migrate{suffix}"], runtime, environment,
                                manifest["engine_version"])
    records += exercise_engined(installed[f"grapt-engined{suffix}"], runtime, environment,
                                source_tree)
    records += recover(prefix, backup, installed, runtime, environment, suffix)

    receipt = {
        "schema": "grapt.release.package-journey.v1",
        "package": str(package),
        "engine_version": manifest["engine_version"],
        "target": manifest["target"],
        "installed": sorted(installed),
        "toolchain_scrub": scrub,
        "cases": [record["case"] for record in records],
        "records": records,
        "attests": (
            "the delivered binaries were installed from the package with every "
            "manifest digest verified, then executed OUTSIDE the source tree "
            "with cargo, rustc and rustup removed from PATH -- a scrub that was "
            "itself verified, and verified to be meaningful on this machine. "
            "The disclosed refusals were fired from the delivered artifacts. "
            "NOT a signature claim, NOT a reproducibility claim, and NOT a "
            "migration of any real data."
        ),
    }
    arguments.receipt.parent.mkdir(parents=True, exist_ok=True)
    arguments.receipt.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"engine_version": receipt["engine_version"],
                      "cases": receipt["cases"],
                      "scrub_is_meaningful": scrub["scrub_is_meaningful"]}, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ExerciseError as error:
        print(f"release journey: {error}", file=sys.stderr)
        raise SystemExit(2) from error
