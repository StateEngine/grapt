"""Add a second provider account to an already-prepared installation.

WHY THIS EXISTS. `scripts/dogfood/windows.py prepare` copies exactly four
inputs — grapt.jsonc, provider.credential, model-catalog.json and
account-models.json — and has no slot for a second account's credential or
discovery file. A configuration declaring one is refused during native
initialization:

    platform configuration could not be loaded:
    openrouter-account-models.json is unreadable

So a two-provider installation cannot be prepared in one step. This is the
ORDINARY ADD-PROVIDER PATH instead: prepare normally with the first provider,
then stage the second account's files into the same service root and swap in
the configuration that declares it. That is what a user adding a provider to an
existing installation does.

`windows.py` and the accepted distribution are untouched; this only writes into
a service root that is already prepared.

WHAT IT REFUSES, because a staging step that writes anywhere is a worse problem
than the one it solves:

  * a destination that escapes the service root, checked after resolution so a
    `..` in a declared name cannot walk out;
  * an input that is not an ordinary file, or is over its bound;
  * a service root that was never prepared, or one with a live service holding
    it — the product must read the new configuration at startup, so writing
    under a running service would leave the file and the process disagreeing.

CREDENTIAL CONTENT IS NEVER READ OR PRINTED. Files are copied by bytes and
reported by name and length only.
"""

import argparse
import json
import shutil
from pathlib import Path

from receipts import ProvenanceError, digest, read_bounded

#: An account credential is a token, not a document.
MAX_CREDENTIAL_BYTES = 64 * 1024

#: A discovery document lists a provider's models; the OpenAI one is ~266 KB.
MAX_DISCOVERY_BYTES = 8 * 1024 * 1024


def contained(root: Path, name: str) -> Path:
    """Resolve `name` under `root`, refusing anything that escapes it."""
    if not name or "\x00" in name:
        raise ProvenanceError(f"{name!r} is not a usable file name")
    resolved = (root / name).resolve()
    if not resolved.is_relative_to(root.resolve()):
        raise ProvenanceError(f"{name} resolves outside the service root")
    return resolved


def stage(args: argparse.Namespace) -> int:
    root = Path(args.root).resolve()
    service = root / "service"
    if not (root / "installation.json").is_file() or not service.is_dir():
        raise ProvenanceError(f"{root} is not a prepared installation")
    if (service / "serve-ready.json").exists():
        raise ProvenanceError(
            "a service is published against this root; stop it before adding an "
            "account, or the configuration on disk and the running process disagree"
        )

    config_bytes = read_bounded(Path(args.config).resolve(), MAX_DISCOVERY_BYTES)
    config = json.loads(config_bytes.decode("utf-8"))
    accounts = config.get("provider", {}).get("accounts")
    if not isinstance(accounts, list) or not accounts:
        raise ProvenanceError("the configuration declares no additional accounts")

    staged = []
    for account in accounts:
        for field, source, bound in (
            ("credential", args.credential, MAX_CREDENTIAL_BYTES),
            ("discovery", args.discovery, MAX_DISCOVERY_BYTES),
        ):
            declared = account.get(field)
            if not declared:
                raise ProvenanceError(f"account {account.get('id')!r} declares no {field}")
            destination = contained(service, declared)
            payload = read_bounded(Path(source).resolve(), bound)
            destination.write_bytes(payload)
            staged.append((declared, len(payload)))

    shutil.copyfile(Path(args.config).resolve(), service / "grapt.jsonc")
    print(
        json.dumps(
            {
                "root": str(root),
                "accounts": [account.get("id") for account in accounts],
                # NAME AND LENGTH ONLY. One of these is a credential.
                "staged": [{"name": name, "bytes": size} for name, size in staged],
                "config_sha256": digest(service / "grapt.jsonc")[0],
            },
            indent=2,
        )
    )
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, help="a prepared installation root")
    parser.add_argument("--config", required=True, help="the configuration declaring both accounts")
    parser.add_argument("--credential", required=True, help="the added account's credential file")
    parser.add_argument("--discovery", required=True, help="the added account's discovery document")
    raise SystemExit(stage(parser.parse_args()))


if __name__ == "__main__":
    main()
