"""Freeze the exact bytes an attended observation will run, outside the author root.

An observer must be able to say afterwards WHAT they watched. My working tree
moves; a bundle does not. This copies the binary, the harness scripts and the
scenarios into a directory of their own, records a SHA256 for every file and the
commit each came from, and refuses to overwrite an existing bundle — a bundle
that could be rewritten after it was observed would prove nothing.

Nothing is built here. The binary is copied as it stands, so the digest in the
receipt is the digest of the artefact that runs.

Usage:
    python prepare-observation.py --binary <grapt-tui.exe> --into <fresh dir>
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

SCRIPTS = (
    "compare.py",
    "journey.py",
    "service.py",
    "reconnect-run.py",
    "cut-proxy.py",
    "event-probe.py",
    # THE CORRELATORS AND THEIR OWN WITNESSES. Bundle-07 named `correlate.py`
    # in its reproduction command and did not contain it, so the one command it
    # offered a reader could not be run from the bundle.
    "correlate.py",
    "harness-selftest.py",
    "mutate.py",
    # THE SCRIPTED PROVIDER. Without it the bundle can only be driven against a
    # real provider, which is billable and unrepeatable; with it an observer can
    # run every dimension at the declared pacing for nothing.
    "loopback-provider.py",
)
SCENARIOS = (
    "j04-comparative.json",
    "j04-large-history.json",
    "j04-reconnect.json",
    "j03-approval.json",
    "j03-refusal.json",
    "j02-input.json",
    "j02-input-product.json",
    "j01-real-provider.json",
    # r3: the two dimensions this revision added.
    "j04-subagent.json",
    "j04-forms.json",
)


class PrepareError(RuntimeError):
    """A refusal this script can name."""


def branch_of() -> str:
    """The branch this bundle was built from, from Git rather than from memory."""
    named = subprocess.run(
        ["git", "rev-parse", "--abbrev-ref", "HEAD"],
        cwd=HERE,
        capture_output=True,
        text=True,
    )
    if named.returncode != 0:
        raise PrepareError(f"the author branch could not be read: {named.stderr.strip()}")
    branch = named.stdout.strip()
    if not branch or branch == "HEAD":
        raise PrepareError("the author root is on a detached HEAD; name the branch first")
    return branch


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def commit_of(path: Path) -> str | None:
    """The last commit that touched this path, or None if it is uncommitted."""
    result = subprocess.run(
        ["git", "log", "-1", "--format=%H", "--", str(path)],
        capture_output=True,
        text=True,
        cwd=str(HERE),
    )
    value = result.stdout.strip()
    return value or None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--into", required=True, type=Path)
    parser.add_argument("--receipts", nargs="*", type=Path, default=[])
    # The PRODUCT binary travels too. The runbook has to name a product, and a
    # path into my working tree would move under the observer; the previous
    # runbook instead named another campaign's preserved distribution.
    parser.add_argument("--product-binary", type=Path)
    arguments = parser.parse_args()

    if arguments.into.exists():
        raise PrepareError(
            f"{arguments.into} already exists; a bundle is written once, because one that "
            "could be rewritten after it was observed would prove nothing"
        )
    if not arguments.binary.is_file():
        raise PrepareError(f"the client binary is absent: {arguments.binary}")

    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], capture_output=True, text=True, cwd=str(HERE)
    ).stdout.strip()
    dirty = subprocess.run(
        ["git", "status", "--porcelain=v1"], capture_output=True, text=True, cwd=str(HERE)
    ).stdout.strip()

    (arguments.into / "scripts").mkdir(parents=True)
    (arguments.into / "scenarios").mkdir(parents=True)

    files: dict[str, dict] = {}

    target = arguments.into / "grapt-tui.exe"
    shutil.copy2(arguments.binary, target)
    files["grapt-tui.exe"] = {
        "sha256": digest(target),
        "bytes": target.stat().st_size,
        "source": str(arguments.binary.resolve()),
        "note": "copied as built; not rebuilt here",
    }

    if arguments.product_binary:
        if not arguments.product_binary.is_file():
            raise PrepareError(f"the product binary is absent: {arguments.product_binary}")
        product = arguments.into / "grapt-server.exe"
        shutil.copy2(arguments.product_binary, product)
        files["grapt-server.exe"] = {
            "sha256": digest(product),
            "bytes": product.stat().st_size,
            "source": str(arguments.product_binary.resolve()),
            "note": "the product this client is observed against; copied as built",
        }

    for name in SCRIPTS:
        source = HERE / name
        if not source.is_file():
            raise PrepareError(f"harness script absent: {source}")
        copy = arguments.into / "scripts" / name
        shutil.copy2(source, copy)
        files[f"scripts/{name}"] = {
            "sha256": digest(copy),
            "bytes": copy.stat().st_size,
            "commit": commit_of(source),
        }

    for name in SCENARIOS:
        source = HERE / "scenarios" / name
        if not source.is_file():
            raise PrepareError(f"scenario absent: {source}")
        copy = arguments.into / "scenarios" / name
        shutil.copy2(source, copy)
        files[f"scenarios/{name}"] = {
            "sha256": digest(copy),
            "bytes": copy.stat().st_size,
            "commit": commit_of(source),
        }

    # RECEIPTS travel with the bundle. A packet that carries the executable and
    # the scripts but not the evidence they produced leaves a reader unable to
    # compare what they watched against what was recorded.
    if arguments.receipts:
        (arguments.into / "receipts").mkdir(parents=True, exist_ok=True)
        for source in sorted(arguments.receipts):
            if not source.exists():
                raise PrepareError(f"receipt absent: {source}")
            if source.is_dir():
                # A RUN DIRECTORY IS NOT A RECEIPT. These hold a staged harness
                # with `node_modules` and, for the retained side, a `.git` --
                # copying one produced a 99MB "evidence" bundle whose bulk was
                # somebody else's dependencies. Refuse rather than filter, so
                # the caller names the files they actually mean.
                intruders = [
                    child.name
                    for child in source.rglob("*")
                    if child.is_dir() and child.name in {"node_modules", ".git"}
                ]
                if intruders:
                    raise PrepareError(
                        f"{source} contains {sorted(set(intruders))}; that is a run "
                        f"directory, not a receipt. Pass the receipt FILES instead."
                    )
                copy = arguments.into / "receipts" / source.name
                shutil.copytree(source, copy)
                for member in sorted(copy.rglob("*")):
                    if member.is_file():
                        files[f"receipts/{member.relative_to(arguments.into / 'receipts').as_posix()}"] = {
                            "sha256": digest(member),
                            "bytes": member.stat().st_size,
                        }
            else:
                copy = arguments.into / "receipts" / source.name
                shutil.copy2(source, copy)
                files[f"receipts/{source.name}"] = {
                    "sha256": digest(copy),
                    "bytes": copy.stat().st_size,
                }

    runbook = HERE / "ATTENDED-RUNBOOK.md"
    copy = arguments.into / "ATTENDED-RUNBOOK.md"
    shutil.copy2(runbook, copy)
    files["ATTENDED-RUNBOOK.md"] = {
        "sha256": digest(copy),
        "bytes": copy.stat().st_size,
        "commit": commit_of(runbook),
    }

    readiness = {
        "contract": "native-tui-observation-bundle/v1",
        "campaign": "NATIVE-TUI-R5-01",
        "bundle_root": str(arguments.into.resolve()),
        # ASKED, not asserted. This was the literal "native-delta", which is the
        # name of the PRESERVED r1 candidate — a different commit and a
        # different thing entirely. A manifest that tells an observer they are
        # watching the preserved branch when they are watching r3's is worse
        # than one that says nothing, and the head recorded beside it made the
        # pair self-contradictory.
        "author_branch": branch_of(),
        "author_head": head,
        "author_tree_clean": dirty == "",
        "uncommitted_paths": dirty.splitlines(),
        "files": files,
        "note": (
            "These are the exact bytes an attended run executes, and this file is how "
            "you check that: every entry carries a SHA256, so a changed file is "
            "detectable. It is HASH-PINNED, not immutable — the directory is ordinary "
            "and writable, and a run with bytecode writing enabled had already dropped "
            "__pycache__ into an earlier one. Re-hash before attesting anything from it, "
            "and set PYTHONDONTWRITEBYTECODE=1. Each entry also carries the commit its "
            "source was last changed in, so the runbook's commit and the client's commit "
            "can be cited separately."
        ),
    }
    (arguments.into / "READINESS.json").write_text(
        json.dumps(readiness, indent=2), encoding="utf-8"
    )
    print(json.dumps(readiness, indent=2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except PrepareError as error:
        print(f"prepare-observation: {error}", file=sys.stderr)
        sys.exit(2)
