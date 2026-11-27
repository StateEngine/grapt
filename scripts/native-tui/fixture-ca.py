"""A disposable certificate authority for one qualification run.

## Why this exists

The packaged product refuses a non-https provider base at every precedence
level, deliberately — `crates/grapt-provider/src/route.rs` says so in its own
words, and there is no `allow_insecure_endpoint` switch "because a switch is
what turns a guard into a suggestion". The accepted loopback fixture, however,
speaks plain HTTP. Two accepted things that cannot meet.

They meet if the FIXTURE comes up to the guard's standard instead of the guard
coming down to the fixture's. That needs a certificate the product will trust,
and the only supply route that does not mutate the machine is the process
environment: `rustls-platform-verifier` 0.7 selects `others::Verifier` on Linux,
which calls `rustls_native_certs::load_native_certs()`, which reads
`SSL_CERT_FILE` / `SSL_CERT_DIR`. So a CA that exists only inside a run
directory, handed to one child process, is enough — and it dies with the run.

WINDOWS IS DIFFERENT AND THIS SCRIPT DOES NOT PRETEND OTHERWISE. Under
`cfg(windows)` the same crate selects `windows::Verifier`, which reads the
platform store and no environment variable at all. Trusting this CA on Windows
would mean writing to the machine's certificate store, which is exactly the
invasive change this design exists to avoid. The receipt records that limit so a
reader cannot mistake a Linux capability for a cross-platform one.

## What it produces

    <root>/ca.crt      the trust anchor, the ONLY file to hand out
    <root>/ca.key      DESTROYED as soon as the leaf is signed; see below
    <root>/server.crt  leaf for the fixture listener, SAN IP:127.0.0.1
    <root>/server.key
    <root>/fixture-ca.json  the receipt

Generation shells out to `openssl`, not to a Python library, because the carrier
that runs the real journey is not guaranteed to have `cryptography` installed
and IS guaranteed to have `openssl`. A fixture that fails to build on the host
where it matters is not a fixture.

Usage:

    python3 fixture-ca.py --root <a disposable directory>
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

# Bounded on purpose. This certificate exists for one run of one journey; a long
# validity would invite reuse, and a reused disposable CA is just a CA.
DAYS = 2

# The fixture listens on IPv4 loopback and nothing else, so the leaf names
# exactly that. An IP SAN rather than a DNS name, because the product is
# configured with a literal `https://127.0.0.1:<port>` base and rustls matches
# the address form it was given.
SUBJECT_ALT_NAME = "IP:127.0.0.1"

# `ca.key` is deliberately NOT here: it is destroyed after signing the one leaf,
# so requiring it afterwards would be requiring the exposure this removes.
FILES = ("ca.crt", "server.crt", "server.key")


def run(argv: list[str]) -> None:
    """One openssl invocation, with its stderr preserved on failure.

    `openssl` reports the actual reason on stderr and returns a bare non-zero
    exit; discarding it would turn every distinct failure into the same
    unhelpful "command failed".
    """
    completed = subprocess.run(argv, capture_output=True, text=True)
    if completed.returncode != 0:
        raise SystemExit(
            f"REFUSED: {' '.join(argv[:3])} exited {completed.returncode}\n"
            f"{completed.stderr.strip()}"
        )


def private(path: Path) -> None:
    """Best-effort 0600. Silent on Windows, where the mode is not the control."""
    try:
        os.chmod(path, 0o600)
    except OSError:
        pass


def generate(root: Path) -> dict:
    ca_key = root / "ca.key"
    ca_crt = root / "ca.crt"
    server_key = root / "server.key"
    server_csr = root / "server.csr"
    server_crt = root / "server.crt"
    extension = root / "leaf.ext"

    run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt",
         "rsa_keygen_bits:2048", "-out", str(ca_key)])
    private(ca_key)
    # THE CA EXTENSIONS ARE SPELLED OUT, NOT LEFT TO openssl's DEFAULT CONFIG.
    # Measured: without them OpenSSL 3.2 issues a self-signed certificate with
    # no `keyUsage`, and verification fails with "CA cert does not include key
    # usage extension" — a message that reads like a client problem and is
    # entirely an issuance one. Two different verifiers would also disagree
    # about whether to accept it, which is worse than failing.
    run(["openssl", "req", "-x509", "-new", "-key", str(ca_key),
         "-sha256", "-days", str(DAYS), "-out", str(ca_crt),
         "-subj", "/CN=Grapt qualification fixture CA",
         "-addext", "basicConstraints=critical,CA:TRUE",
         "-addext", "keyUsage=critical,keyCertSign,cRLSign",
         # NAME CONSTRAINTS, because the LEAF's SAN constrains the leaf and says
         # nothing about what the issuer may go on to sign. A trusted CA with no
         # constraints can mint a certificate for any name for as long as it is
         # trusted, and this one's private key sits in the run root. Constraining
         # the ISSUER is a request that a verifier narrow what this anchor can
         # vouch for.
         #
         # A REQUEST, NOT A CONTROL, and deliberately not relied on. Nothing here
         # claims it restricts DNS or other name types in practice, nor that any
         # verifier — the Windows one included — enforces it on a trusted root.
         # Proving that would need a store mutation, which is not authorized.
         # THE REAL BOUND IS BELOW: the signing key is destroyed after this one
         # leaf, so the CA can issue nothing further whatever a verifier does.
         "-addext", "nameConstraints=critical,permitted;IP:127.0.0.1/255.255.255.255"])

    run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt",
         "rsa_keygen_bits:2048", "-out", str(server_key)])
    private(server_key)
    run(["openssl", "req", "-new", "-key", str(server_key), "-out",
         str(server_csr), "-subj", "/CN=127.0.0.1"])

    # THE SAN IS THE WHOLE POINT. A leaf carrying only a CN is rejected by
    # rustls outright — it has never honoured CN as a name source — so a
    # certificate that looks correct to a human reading the subject would fail
    # every handshake with an opaque error.
    extension.write_text(
        "basicConstraints=CA:FALSE\n"
        "keyUsage=digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\n"
        f"subjectAltName={SUBJECT_ALT_NAME}\n",
        encoding="utf-8",
    )
    run(["openssl", "x509", "-req", "-in", str(server_csr), "-CA", str(ca_crt),
         "-CAkey", str(ca_key), "-CAcreateserial", "-days", str(DAYS),
         "-sha256", "-extfile", str(extension), "-out", str(server_crt)])

    server_csr.unlink()
    extension.unlink()

    # THE CA SIGNING KEY IS DESTROYED THE MOMENT IT IS NO LONGER NEEDED.
    #
    # It signed exactly one leaf and has no further use, while its continued
    # existence is the real exposure: anything that can read it can mint a
    # certificate for any name for as long as this anchor is trusted anywhere.
    # Keeping it "in case" would be keeping the one file that makes a
    # short-lived loopback fixture into a general-purpose signing authority.
    ca_key.unlink()
    if ca_key.exists():
        raise SystemExit(
            f"REFUSED: {ca_key} still exists after deletion. The CA signing key must not "
            "outlive its one issuance; stop rather than proceed with it on disk."
        )

    return {
        "contract": "grapt.qualification.fixture-ca.v1",
        "root": str(root),
        "ca_file": str(ca_crt),
        "server_cert": str(server_crt),
        "server_key": str(server_key),
        "subject_alt_name": SUBJECT_ALT_NAME,
        "ca_name_constraints": "critical, permitted IP:127.0.0.1/255.255.255.255",
        "ca_name_constraints_enforcement": (
            "UNPROVED. No claim is made that this restricts DNS or other name types in "
            "practice, or that any particular verifier — the Windows one included — enforces "
            "it on a trusted root. Testing that would need a store mutation, which is not "
            "authorized. Treat it as a request, not a control."
        ),
        "ca_key_disposition": (
            "DELETED immediately after the one leaf was signed, and its absence verified. "
            "The signing key is the actual exposure while an anchor is trusted."
        ),
        "ca_authority_limit": (
            "the leaf SAN constrains the LEAF only and says nothing about what the issuer "
            "may sign. With the signing key destroyed, this CA can issue nothing further; "
            "that is the real bound. Removing the anchor is the control."
        ),
        "days": DAYS,
        "validity_is": (
            "a backstop, not containment: expiry handling differs across trust "
            "implementations. Removing the anchor is the control."
        ),
        "openssl": openssl_version(),
        "trust_supply": {
            "linux": "SSL_CERT_FILE=<ca_file> on the product process only",
            "windows": (
                "NOT AVAILABLE. rustls-platform-verifier selects windows::Verifier "
                "under cfg(windows); it reads the platform store and no environment "
                "variable. Supplying this CA there would mean mutating host trust."
            ),
        },
        "disposal": "the whole root is removed with the run; it holds a private key",
    }


def openssl_version() -> str:
    completed = subprocess.run(["openssl", "version"], capture_output=True, text=True)
    return completed.stdout.strip() or "unknown"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True,
                        help="disposable directory to hold the CA and leaf")
    parser.add_argument("--replace", action="store_true",
                        help="remove an existing root instead of refusing")
    arguments = parser.parse_args()

    if shutil.which("openssl") is None:
        # NAMED, not generic. An operator who sees this knows the one thing to
        # install, rather than that "something went wrong".
        raise SystemExit("REFUSED: openssl is not on PATH; the fixture CA cannot be built")

    root: Path = arguments.root
    if root.exists() and any(root.iterdir()):
        # REFUSE RATHER THAN OVERWRITE. This directory holds a private key; a
        # second run silently replacing the first one's material is how a
        # listener and the CA a client was told to trust drift apart, and the
        # handshake failure that follows says nothing about the cause.
        if not arguments.replace:
            raise SystemExit(
                f"REFUSED: {root} is not empty; pass --replace to discard it deliberately"
            )
        shutil.rmtree(root)
    root.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(root, 0o700)
    except OSError:
        pass

    receipt = generate(root)
    missing = [name for name in FILES if not (root / name).exists()]
    if missing:
        raise SystemExit(f"REFUSED: openssl reported success but did not write {missing}")
    (root / "fixture-ca.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(receipt["ca_file"], flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
