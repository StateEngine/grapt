"""Prepare and serve an ISOLATED product service root for native-TUI evidence.

J01 owes a real external response, which means a product configured against a
real provider rather than the contract double or a deterministic fixture. This
builds that product a service root of its own.

## Why it does not reuse an existing dogfood root

The provisioned dogfood roots under `<evidence-root>/retained-desktop-*`
are another campaign's PRESERVED EVIDENCE, and they hold live stores. Opening
one with a binary built from a different commit would migrate a database this
campaign does not own, which the project law forbids in as many words. So this
copies the provider INPUTS -- the configuration files, not the store -- into a
fresh root, and the store is created from nothing by the product itself.

## Fresh initialization

`initialize` against an absent store creates a current database and serves it.
Preparation requires its successful served receipt and registration release,
then selects `open-ready` for subsequent launches. A configuration fault, a
legacy migration refusal or a generic successful exit cannot satisfy this guard.

## Credentials

The provider credential is COPIED as a file and never read, printed, logged or
passed as an argument. Nothing in this script or its receipt carries a secret.

Usage:

    python service.py prepare --root <fresh dir> --inputs <provider inputs dir>
                              --binary <grapt-server>
    python service.py serve   --root <dir> --binary <grapt-server> [--window-ms N]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

# The provider inputs a real-provider root needs, by name. Copied verbatim
# through the product's own configuration boundary.
INPUTS = ("grapt.jsonc", "provider.credential", "model-catalog.json", "account-models.json")

# This root's OWN HTTP credential, which is not the provider's and is not a
# secret worth protecting: it authenticates a loopback socket that only exists
# while this evidence run is serving.
#
# The username is `opencode` because the RETAINED TUI refuses any other one for
# a local API attach, and the comparative journey needs both clients on the same
# product. The native client cares only that the record parses.
USERNAME = "opencode"
SECRET = "native-tui-r5"

READY_POLLS = 600
READY_SLEEP = 0.05


class ServiceError(RuntimeError):
    """A refusal this script can name."""


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def authority(config: dict, value: str) -> dict:
    out = dict(config)
    out["store"] = dict(out.get("store", {}))
    out["store"]["authority"] = value
    return out


def initialized_receipt(exit_code: int, stdout: str | bytes, root: Path) -> dict:
    """Fresh Initialize must serve a Ready store and release its registration."""
    try:
        receipt = json.loads(stdout)
    except (ValueError, UnicodeDecodeError) as error:
        raise ServiceError("fresh initialization returned no launch receipt") from error
    if (exit_code != 0 or not isinstance(receipt, dict)
            or receipt.get("schema") != "grapt.product.launch.v1"
            or receipt.get("outcome") != "served"
            or receipt.get("registration_released") is not True):
        raise ServiceError("fresh initialization did not serve and release its registration")
    if not (root / "database.sqlite3").is_file():
        raise ServiceError("fresh initialization left no Ready database")
    return receipt


def prepare(root: Path, inputs: Path, binary: Path, window_ms: int) -> dict:
    if root.exists():
        raise ServiceError(
            f"{root} already exists; a service root is prepared once, and reusing one "
            "would mean this run's evidence describes a store another run created"
        )
    for name in INPUTS:
        if not (inputs / name).is_file():
            raise ServiceError(f"provider input {name} is absent from {inputs}")
    if not binary.is_file():
        raise ServiceError(f"the product binary is absent: {binary}")

    root.mkdir(parents=True)
    for name in INPUTS:
        shutil.copy2(inputs / name, root / name)

    # FILES THE CONFIG ITSELF NAMES, beyond the fixed four.
    #
    # `INPUTS` is a fixed list, and a newer inputs root declares a SECOND
    # provider account whose credential and model discovery live in files of
    # their own. Copying only the fixed four left `grapt.jsonc` pointing at
    # names that were not there, and the product refused to boot at all:
    # "openrouter-account-models.json is unreadable". Measured against
    # the dogfood ops smoke run's inputs.
    #
    # So the extra files are taken FROM THE CONFIG rather than from a second
    # hardcoded list, which is what keeps this working when an account is added
    # again. A name the config declares and the inputs do not hold is refused
    # here, by name, rather than becoming a boot failure several steps later.
    declared = json.loads((root / "grapt.jsonc").read_text(encoding="utf-8"))
    for account in declared.get("provider", {}).get("accounts", []) or []:
        for key in ("credential", "discovery"):
            named = account.get(key)
            if not named:
                continue
            if not (inputs / named).is_file():
                raise ServiceError(
                    f"the config declares account {account.get('id', '?')!r} whose {key} "
                    f"names {named}, which is absent from {inputs}"
                )
            shutil.copy2(inputs / named, root / named)

    # Project discovery must stop HERE rather than at an ambient ancestor, or
    # the product resolves this evidence root's project to whatever repository
    # happens to contain it.
    initialized = subprocess.run(
        ["git", "init", "--quiet", str(root)],
        capture_output=True,
        text=True,
    )
    if initialized.returncode != 0:
        raise ServiceError(f"the fixture git boundary was refused: {initialized.stderr}")

    (root / "service.conf").write_text(
        "slot=product\n"
        "instance=native-tui-delta\n"
        "environment=native-tui-r5\n"
        "version=0.1.0\n"
        f"serve_window_ms={window_ms}\n"
        "provider_mode=production\n"
        "provider_connect_ms=10000\n"
        "provider_response_head_ms=60000\n"
        "provider_active_attempts=4\n",
        encoding="utf-8",
    )
    # No trailing newline: the product refuses a credential file that is not
    # exactly one LF-free `username:secret` record, and it says so by name.
    (root / "basic.credential").write_text(f"{USERNAME}:{SECRET}", encoding="utf-8")

    config = json.loads((root / "grapt.jsonc").read_text(encoding="utf-8"))
    (root / "grapt.jsonc").write_text(
        json.dumps(authority(config, "initialize"), indent=2), encoding="utf-8"
    )

    # Preparation serves only long enough to observe successful initialization.
    # Restore the caller's service window before the interactive launch.
    service_path = root / "service.conf"
    service_config = service_path.read_text(encoding="utf-8")
    service_path.write_text(service_config.replace(
        f"serve_window_ms={window_ms}\n", "serve_window_ms=100\n"), encoding="utf-8")
    try:
        first = subprocess.run(
            [str(binary), "serve", "--service", str(root), "--port", "0"],
            capture_output=True, text=True, timeout=60,
        )
    finally:
        service_path.write_text(service_config, encoding="utf-8")
    initialization = initialized_receipt(first.returncode, first.stdout, root)
    (root / "grapt.jsonc").write_text(
        json.dumps(authority(config, "open-ready"), indent=2), encoding="utf-8"
    )
    record = {
        "initialization": initialization,
        "root": str(root.resolve()),
        "inputs": str(inputs.resolve()),
        "provider": config.get("provider", {}),
        "catalog": config.get("catalog", {}),
        "binary": str(binary.resolve()),
        "binary_sha256": digest(binary),
    }
    # Persisted, not merely printed. The `build` string in a served root's
    # readiness receipt comes from the COPIED CONFIG, so it identifies the
    # inputs and not the executable that opened the store. Without this file a
    # finished evidence root cannot say which product produced it, and a reader
    # asking that question later has to guess from timestamps -- which is
    # exactly the position this run's predecessor left me in.
    (root / "service-prepared.json").write_text(
        json.dumps(record, indent=2), encoding="utf-8"
    )
    return record


# The refusal that means "this store is past the authority the config names".
MIGRATED = "does not authorize first-contact state Ready"

# How much of the product's own output a refusal carries back. Bounded because
# this string is raised in an exception message and matched against, and kept
# from BOTH ENDS -- see `bounded`.
DIAGNOSTIC_BYTES = 64 * 1024


def bounded(text: str) -> str:
    """Keep BOTH ENDS of an over-long diagnostic, never just the tail.

    Tail-only truncation was wrong and a witness caught it: a child that states
    its reason and THEN floods -- which is what a refusing observer at startup
    does -- had that reason cut off, so the caller raised a screenful of noise
    as the reason a run stopped. The LAST thing written is usually why a process
    stopped; the FIRST thing is why it never started. Both are kept.
    """
    if len(text) <= DIAGNOSTIC_BYTES:
        return text
    half = DIAGNOSTIC_BYTES // 2
    elided = len(text) - 2 * half
    return f"{text[:half]}" + f"\n... [{elided} characters elided] ...\n" + f"{text[-half:]}"



def diagnostic(root: Path, log_suffix: str = "") -> str:
    """What the product printed before it exited, read from the files it wrote.

    THIS CANNOT COME FROM `communicate()`. `launch` redirects both streams to
    FILES so nothing blocks on an undrained pipe (B3), and
    `communicate()` on a file-backed child returns `(None, None)` — so the
    `stdout + stderr` that used to stand here raised `TypeError: unsupported
    operand type(s) for +: 'NoneType' and 'NoneType'`. A refusal the caller was
    written to READ and act on became a crash inside the helper instead, and the
    store-authority ladder below it became unreachable: a product that correctly
    refused `Migrate` on an already-`Ready` store could never be advanced to
    `open-ready`, because the branch that does it was never reached. Measured
    during a canonical post-land restart.

    A REFUSAL THAT CANNOT BE READ MUST NOT LOOK LIKE ONE THAT DID NOT HAPPEN.
    Each stream is reported as itself: its text, or `<absent>` if the product
    never created it, or `<unreadable: reason>` if it exists and cannot be read.
    Folding either into an empty string would make an unreadable refusal
    indistinguishable from a silent exit, and would then be matched against
    `MIGRATED` — deciding a store's authority on evidence nobody could read.
    """
    parts = []
    for stream in ("product-stdout", "product-stderr"):
        name = f"{stream}{log_suffix}.log"
        path = root / name
        if not path.exists():
            parts.append(f"[{name}] <absent>")
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as error:
            parts.append(f"[{name}] <unreadable: {error.strerror or error}>")
            continue
        if not text:
            parts.append(f"[{name}] <empty>")
            continue
        parts.append(f"[{name}]\n{bounded(text)}")
    return "\n".join(parts)


def launch(root: Path, binary: Path, loopback: str | None = None,
           log_suffix: str = "") -> subprocess.Popen | str:
    """Start the product once. Returns the child, or the refusal it printed.

    `loopback` points the production provider path at a scripted endpoint
    through the product's own named testkit seam. Everything else stays
    production: the transport, the streaming decode, the durable capture and
    both clients. Only the model is replaced, which is what lets a comparative
    run be repeated without model spend. Omit it for a real provider.
    """
    receipt = root / "serve-ready.json"
    if receipt.exists():
        receipt.unlink()
    environment = dict(os.environ)
    if loopback is not None:
        environment["GRAPT_LIVE_HEADLESS_LOOPBACK"] = loopback
    # DISTINCT LOGS PER INVOCATION when a caller asks for them.
    #
    # These names are fixed and the mode is "w", so a SECOND launch against the
    # same root TRUNCATES the first one's logs. That is harmless while a root is
    # a scratch fixture and destructive once it is evidence: probing a preserved
    # store destroyed the original run's product output, and the only reason the
    # pre-teardown state survived was that a diagnostic had already been printed
    # elsewhere.
    #
    # `log_suffix` defaults to "" so every existing caller keeps the exact
    # behaviour it has today; a caller reopening EVIDENCE passes a distinct
    # suffix and cannot overwrite what it came to read.
    out = (root / f"product-stdout{log_suffix}.log").open("w", encoding="utf-8")
    err = (root / f"product-stderr{log_suffix}.log").open("w", encoding="utf-8")
    child = subprocess.Popen(
        [str(binary), "serve", "--service", str(root), "--port", "0"],
        # THE PRODUCT'S OUTPUT GOES TO FILES, NEVER TO A PIPE NOBODY READS.
        #
        # This was `subprocess.PIPE` for both streams, and this process then
        # EXITS and leaves the product serving — so nothing ever drained them.
        # A pipe with no reader fills (4 KB on Windows) and the next write
        # BLOCKS, inside whatever thread the product was using, and a turn that
        # had already produced a good Step ends up recorded as a failed
        # execution.
        #
        # Measured, in one service root, changing nothing else: with pipes, two
        # consecutive comparative runs added four `session.execution.failed`
        # rows each and no successes; with these files, the next two runs added
        # four `session.execution.succeeded` rows each and no failures. Twelve
        # failures, then zero. That is B3, and its cause was here in
        # the harness rather than in the product.
        #
        # Keeping the output is worth more than discarding it anyway: an
        # observer gets the product's own diagnostics beside the run.
        stdout=out,
        stderr=err,
        text=True,
        env=environment,
    )
    # THE PARENT'S COPIES ARE CLOSED IMMEDIATELY. The child has its own
    # descriptors, so keeping these open buys nothing and costs two leaked
    # handles per launch -- and on Windows an open handle makes the file
    # undeletable, so a caller cleaning up a root got
    # `PermissionError: [WinError 32] ... being used by another process` for a
    # product that had already exited. Found by the ladder witness, not in the
    # field.
    out.close()
    err.close()
    for _ in range(READY_POLLS):
        if child.poll() is not None:
            # The child is gone and its output is in the files it was given, so
            # it is read from there rather than from a pipe that does not exist.
            # `communicate()` is still called, to reap the process rather than
            # leave a zombie, but its return value is (None, None) here and is
            # deliberately not used.
            child.communicate()
            return diagnostic(root, log_suffix)
        if receipt.is_file():
            try:
                json.loads(receipt.read_text(encoding="utf-8"))
                return child
            except json.JSONDecodeError:
                pass  # still being written
        time.sleep(READY_SLEEP)
    child.kill()
    raise ServiceError(
        f"the product published no readiness receipt within "
        f"{round(READY_POLLS * READY_SLEEP, 1)}s"
    )


def serve(root: Path, binary: Path, loopback: str | None = None) -> subprocess.Popen:
    """Serve, following the store authority ladder the product enforces.

    Fresh roots prepared here already select `open-ready`. A retained legacy
    root may still select `migrate`; advance that authority only on the specific
    Ready refusal after its migrations have completed. Unrelated configuration
    and store failures must remain failures.
    """
    outcome = launch(root, binary, loopback)
    if not isinstance(outcome, str):
        return outcome
    if MIGRATED not in outcome:
        raise ServiceError(f"the product refused to serve:\n{outcome[-4000:]}")

    # KEEP THE REFUSAL. The next launch reopens the same two log files with
    # mode "w" and truncates them, so without this the evidence that the store
    # WAS advanced -- and on what stated reason -- is destroyed by the very
    # relaunch it authorised, leaving a successful root that cannot say it ever
    # climbed a rung.
    (root / "product-refusal-migrate.log").write_text(outcome, encoding="utf-8")

    config = json.loads((root / "grapt.jsonc").read_text(encoding="utf-8"))
    (root / "grapt.jsonc").write_text(
        json.dumps(authority(config, "open-ready"), indent=2), encoding="utf-8"
    )
    outcome = launch(root, binary, loopback)
    if isinstance(outcome, str):
        raise ServiceError(
            f"the product refused to serve a migrated store:\n{outcome[-4000:]}"
        )
    return outcome


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["prepare", "serve"])
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--inputs", type=Path)
    parser.add_argument("--window-ms", type=int, default=3_600_000)
    parser.add_argument(
        "--loopback",
        help="scripted provider endpoint, so a run costs no model spend",
    )
    arguments = parser.parse_args()

    if arguments.action == "prepare":
        if arguments.inputs is None:
            raise ServiceError("prepare needs --inputs naming the provider input directory")
        record = prepare(arguments.root, arguments.inputs, arguments.binary, arguments.window_ms)
        print(json.dumps(record, indent=2))
        return 0

    child = serve(arguments.root, arguments.binary, arguments.loopback)
    receipt = json.loads((arguments.root / "serve-ready.json").read_text(encoding="utf-8"))
    print(json.dumps({"pid": child.pid, "ready": receipt}, indent=2))
    # The caller owns the lifetime from here: this process exits and leaves the
    # product serving under its declared window.
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ServiceError as error:
        print(f"native-tui service: {error}", file=sys.stderr)
        sys.exit(2)
