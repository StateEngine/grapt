"""Run a REAL signature check against delivered artifacts and report what it finds.

## Why this exists rather than a constant

`package.py` reported `"signed": false` from a hardcoded function. That was
truthful — there is no signing identity in this tree — but it was an ASSERTION,
and J03 asks for something else: *"Produce executable signing checks and the
actual per-target result."*

A constant cannot distinguish these three states, and they are not the same:

    unsigned        a verifier ran and reported no signature
    unverifiable    no verifier exists for this target on this host
    unknown         nobody looked

The old constant said "unsigned" in all three cases. This runs a verifier where
one exists and reports the state it actually observed, including which of the
three it is.

## What it will not do

It will not fabricate a signature, and it will not treat its own absence of
tooling as evidence of anything about the artifact. `unverifiable` is reported
as `unverifiable`, never as `unsigned` — collapsing the two is exactly how "we
did not check" becomes "we checked and it was fine".

It will not report a package as signed on the strength of a manifest field. The
only thing that can make `signed` true here is a verifier saying so about the
bytes.

## Per target

    x86_64-pc-windows-msvc   Authenticode, via PowerShell's
                             Get-AuthenticodeSignature. A real verifier that is
                             present on any Windows host, so on Windows the
                             result is a MEASUREMENT.
    x86_64-unknown-linux-gnu ELF binaries carry no Authenticode equivalent and
                             this tree applies no detached-signature scheme, so
                             there is nothing to verify. Reported as
                             `no-signature-scheme`, which is a statement about
                             the release process rather than about the bytes.

Signing IDENTITIES are enumerated separately from signature STATE, because
"this machine could sign" and "this artifact is signed" are different questions
and conflating them is how an available certificate becomes a claimed signature.
"""

from __future__ import annotations

import argparse
import contextlib
import ctypes
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import shipped

WINDOWS_TARGET = "x86_64-pc-windows-msvc"
LINUX_TARGET = "x86_64-unknown-linux-gnu"
POWERSHELL_TIMEOUT = 120
MAX_PUBLISHER_BYTES = 1024


class UpdateTrustError(RuntimeError):
    """A Windows update trust refusal with one named cause."""


def digest(path: Path) -> tuple[str, int]:
    """Hash one artifact without loading it into memory."""
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


def _open_windows_deny_write(path: Path) -> tuple[object, object] | None:
    """Hold one Windows path readable but not reopenable for write/delete."""
    if os.name != "nt":
        return None
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    create_file = kernel32.CreateFileW
    create_file.argtypes = [
        wintypes.LPCWSTR,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.HANDLE,
    ]
    create_file.restype = wintypes.HANDLE
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = [wintypes.HANDLE]
    close_handle.restype = wintypes.BOOL
    handle = create_file(
        str(path.resolve()),
        0x80000000,  # GENERIC_READ
        0x00000001,  # FILE_SHARE_READ: deny write and delete opens
        None,
        3,  # OPEN_EXISTING
        0x00000080,  # FILE_ATTRIBUTE_NORMAL
        None,
    )
    if handle == ctypes.c_void_p(-1).value:
        code = ctypes.get_last_error()
        raise OSError(code, "could not lock verification copy against writes", str(path))
    return handle, close_handle


@contextlib.contextmanager
def immutable_verification_copy(source: Path, effective_os: str):
    """Yield a private copy that the verifier can read but cannot rewrite.

    Removing write permission makes the invariant executable on non-Windows
    test hosts. The live Windows path additionally holds a CreateFile handle
    that shares reads only, so another ordinary open cannot write, replace, or
    delete the file while Authenticode has its pathname.
    """
    with tempfile.TemporaryDirectory(prefix="grapt-authenticode-") as raw_root:
        root = Path(raw_root)
        verified = root / source.name
        shutil.copy2(source, verified)
        original_mode = verified.stat().st_mode
        locked_mode = original_mode & ~(stat.S_IWUSR | stat.S_IWGRP | stat.S_IWOTH)
        verified.chmod(locked_mode)
        windows_lock = None
        try:
            if effective_os == "nt":
                windows_lock = _open_windows_deny_write(verified)
            yield verified
        finally:
            if windows_lock is not None:
                handle, close_handle = windows_lock
                close_handle(handle)
            if verified.exists():
                verified.chmod(original_mode)


def powershell(script: str) -> tuple[int, str, str]:
    for executable in ("pwsh", "powershell"):
        try:
            done = subprocess.run(
                [executable, "-NoProfile", "-NonInteractive", "-Command", script],
                capture_output=True, text=True, timeout=POWERSHELL_TIMEOUT, check=False,
            )
        except (FileNotFoundError, subprocess.TimeoutExpired):
            continue
        return done.returncode, done.stdout, done.stderr
    return 127, "", "no PowerShell interpreter is available on this host"


def available_identities() -> dict:
    """Signing identities this host could sign WITH. Never private key material.

    Enumerated separately from signature state on purpose: "this machine could
    sign" and "this artifact is signed" are different questions, and answering
    the second with the first is how an available certificate turns into a
    claimed signature.
    """
    if os.name != "nt":
        return {
            "enumerated": False,
            "reason": "certificate stores are a Windows concept; not enumerable here",
            "count": 0,
        }
    code, out, err = powershell(
        "$c = @(Get-ChildItem Cert:\\CurrentUser\\My -CodeSigningCert -ErrorAction "
        "SilentlyContinue) + @(Get-ChildItem Cert:\\LocalMachine\\My -CodeSigningCert "
        "-ErrorAction SilentlyContinue); "
        "$c | ForEach-Object { [pscustomobject]@{ subject = $_.Subject; "
        "thumbprint = $_.Thumbprint; notAfter = $_.NotAfter.ToString('o') } } | "
        "ConvertTo-Json -Compress -AsArray"
    )
    if code != 0:
        return {"enumerated": False, "reason": err.strip()[:300], "count": 0}
    try:
        rows = json.loads(out.strip() or "[]")
    except json.JSONDecodeError:
        return {"enumerated": False, "reason": "identity enumeration was not JSON", "count": 0}
    # Thumbprints and subjects only. A thumbprint identifies a certificate; it
    # is not a key and cannot sign anything.
    return {"enumerated": True, "count": len(rows), "identities": rows}


def authenticode(path: Path) -> dict:
    """Verify one file with a real Authenticode verifier."""
    verified = path.resolve()
    # PowerShell single-quoted literals escape one quote by doubling it. Paths
    # are data: a package directory containing a quote must not become script.
    literal = str(verified).replace("'", "''")
    code, out, err = powershell(
        f"$s = Get-AuthenticodeSignature -LiteralPath '{literal}'; "
        "[pscustomobject]@{ status = $s.Status.ToString(); "
        "statusMessage = $s.StatusMessage; "
        "signer = if ($s.SignerCertificate) { $s.SignerCertificate.Subject } else { $null }; "
        "thumbprint = if ($s.SignerCertificate) { $s.SignerCertificate.Thumbprint } else { $null } "
        "} | ConvertTo-Json -Compress"
    )
    if code != 0:
        return {
            "state": "unverifiable",
            "verifier": "Get-AuthenticodeSignature",
            "verified_file": str(verified),
            "detail": err.strip()[:300] or "the verifier did not complete",
        }
    try:
        result = json.loads(out.strip())
    except json.JSONDecodeError:
        return {"state": "unverifiable", "verifier": "Get-AuthenticodeSignature",
                "verified_file": str(verified),
                "detail": "verifier output was not JSON"}
    status = result.get("status")
    return {
        # `Valid` is the ONLY status that makes this signed. Every other status
        # -- NotSigned, HashMismatch, UnknownError, NotTrusted -- is not a
        # signature this release would stand behind, and lumping them together
        # as "not valid" would lose which one it was.
        "state": "signed" if status == "Valid" else "unsigned",
        "verifier": "Get-AuthenticodeSignature",
        "verified_file": str(verified),
        "status": status,
        "status_message": result.get("statusMessage"),
        "signer": result.get("signer"),
        "thumbprint": result.get("thumbprint"),
    }


def check_target(package: Path, target: str, host_os: str | None = None) -> dict:
    effective_os = os.name if host_os is None else host_os
    suffix = shipped.suffix_for(target)
    names = shipped.names(suffix)
    per_binary: dict[str, dict] = {}

    for name in names:
        path = package / name
        if not path.is_file():
            per_binary[name] = {"state": "absent", "detail": f"{path} is not in the package"}
            continue
        if target == WINDOWS_TARGET:
            if effective_os != "nt":
                # HONEST DEGRADATION. A Windows artifact checked from a
                # non-Windows host is UNVERIFIED, which is not the same as
                # unsigned, and reporting it as unsigned would turn "we did not
                # check" into "we checked and it was fine".
                per_binary[name] = {
                    "state": "unverifiable",
                    "verifier": None,
                    "verified_file": str(path.resolve()),
                    "detail": "Authenticode verification requires a Windows host; "
                              "this check ran elsewhere",
                }
            else:
                source_file = str(path.resolve())
                try:
                    with immutable_verification_copy(path, effective_os) as verified:
                        before = digest(verified)
                        try:
                            observed = authenticode(verified)
                        except OSError as error:
                            observed = {
                                "state": "verification-copy-write-attempt",
                                "verifier": "Get-AuthenticodeSignature",
                                "verified_file": str(verified.resolve()),
                                "detail": (
                                    "immutable verification copy rejected a write: "
                                    f"{error}"
                                ),
                            }
                        after = digest(verified)
                except OSError as error:
                    observed = {
                        "state": "unverifiable",
                        "verifier": "Get-AuthenticodeSignature",
                        "verified_file": None,
                        "source_file": source_file,
                        "detail": f"could not make an immutable verification copy: {error}",
                    }
                else:
                    observed = {
                        **observed,
                        "source_file": source_file,
                        "verification_copy": "private-read-only",
                    }
                    if before != after:
                        observed = {
                            **observed,
                            "state": "changed-during-verification",
                            "sha256_before": before[0],
                            "bytes_before": before[1],
                            "sha256_after": after[0],
                            "bytes_after": after[1],
                            "detail": "the verification copy changed while Authenticode examined it",
                        }
                    else:
                        observed = {
                            **observed,
                            "verified_sha256": after[0],
                            "verified_bytes": after[1],
                        }
                per_binary[name] = observed
        elif target == LINUX_TARGET:
            per_binary[name] = {
                "state": "no-signature-scheme",
                "verifier": None,
                "detail": "ELF binaries carry no Authenticode equivalent and this "
                          "release applies no detached-signature scheme, so there is "
                          "nothing to verify. This describes the release process, "
                          "not the bytes",
            }
        else:
            per_binary[name] = {"state": "unknown", "detail": f"no check is defined for {target}"}

    states = {entry["state"] for entry in per_binary.values()}
    return {
        "target": target,
        "binaries": per_binary,
        "all_signed": states == {"signed"},
        "states_observed": sorted(states),
    }


def validate_windows_update(
    package: Path, publisher: object, binaries: dict[str, dict], observation: dict
) -> dict:
    """Bind a Windows update to its declared publisher and exact verified files.

    This validator is shared by package assembly and activation. Tests may feed
    a deterministic observation, but the production caller obtains the same
    shape only from :func:`check_target`, which invokes Authenticode on Windows.
    """
    if not isinstance(publisher, str):
        raise UpdateTrustError("Windows update refused: artifact declares no publisher")
    if not publisher.strip():
        raise UpdateTrustError("Windows update refused: artifact declares no publisher")
    if len(publisher.encode("utf-8")) > MAX_PUBLISHER_BYTES:
        raise UpdateTrustError(
            f"Windows update refused: publisher exceeds {MAX_PUBLISHER_BYTES} bytes"
        )
    if observation.get("target") != WINDOWS_TARGET:
        raise UpdateTrustError(
            f"Windows update refused: verifier observed {observation.get('target')!r}, "
            f"not {WINDOWS_TARGET!r}"
        )
    observed = observation.get("binaries")
    if not isinstance(observed, dict):
        raise UpdateTrustError("Windows update refused: verifier returned no binary map")
    expected = set(binaries)
    if set(observed) != expected:
        missing = sorted(expected - set(observed))
        extra = sorted(set(observed) - expected)
        raise UpdateTrustError(
            f"Windows update refused: verifier file set differs; missing={missing}, extra={extra}"
        )

    accepted: dict[str, dict] = {}
    for name in sorted(binaries):
        row = observed[name]
        if not isinstance(row, dict):
            raise UpdateTrustError(f"Windows update refused: {name} verifier row is malformed")
        state = row.get("state")
        if state == "absent":
            raise UpdateTrustError(f"Windows update refused: {name} is absent")
        if state == "unverifiable":
            raise UpdateTrustError(f"Windows update refused: {name} is unverifiable")
        if state == "changed-during-verification":
            raise UpdateTrustError(
                f"Windows update refused: {name} changed during signature verification"
            )
        if state == "verification-copy-write-attempt":
            raise UpdateTrustError(
                f"Windows update refused: {name} immutable verification copy was rewritten"
            )
        if state == "unsigned":
            raise UpdateTrustError(f"Windows update refused: {name} is unsigned")
        wanted = str((package / name).resolve())
        if row.get("source_file") != wanted:
            raise UpdateTrustError(
                f"Windows update refused: {name} verifier sourced "
                f"{row.get('source_file')!r}, not exact package file {wanted!r}"
            )
        if row.get("verification_copy") != "private-read-only":
            raise UpdateTrustError(
                f"Windows update refused: {name} was not checked from an immutable "
                "private verification copy"
            )
        if state != "signed" or row.get("status") != "Valid":
            raise UpdateTrustError(
                f"Windows update refused: {name} verification is not Valid "
                f"(state={state!r}, status={row.get('status')!r})"
            )
        if row.get("verifier") != "Get-AuthenticodeSignature":
            raise UpdateTrustError(
                f"Windows update refused: {name} used verifier {row.get('verifier')!r}"
            )
        if row.get("signer") != publisher:
            raise UpdateTrustError(
                f"Windows update refused: {name} signer {row.get('signer')!r} "
                f"does not match declared publisher {publisher!r}"
            )
        attested = binaries[name]
        if not isinstance(attested, dict):
            raise UpdateTrustError(
                f"Windows update refused: {name} manifest row is malformed"
            )
        if (
            row.get("verified_sha256") != attested.get("sha256")
            or row.get("verified_bytes") != attested.get("bytes")
        ):
            raise UpdateTrustError(
                f"Windows update refused: {name} verified bytes do not match manifest "
                f"{attested.get('sha256')}/{attested.get('bytes')}"
            )
        accepted[name] = row

    return {
        "schema": "grapt.release.windows-update-trust.v1",
        "target": WINDOWS_TARGET,
        "publisher": publisher,
        "binaries": accepted,
        "attests": (
            "every activated Windows binary has the same byte digest as an immutable "
            "private copy checked Valid by Authenticode and signed by the "
            "artifact-declared publisher"
        ),
    }


def verify_windows_update(
    package: Path, publisher: object, binaries: dict[str, dict]
) -> dict:
    """Run the real verifier, then apply the update trust contract."""
    resolved = package.resolve()
    return validate_windows_update(
        resolved, publisher, binaries, check_target(resolved, WINDOWS_TARGET)
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--receipt", type=Path, default=None)
    arguments = parser.parse_args()

    package = arguments.package.resolve()
    result = check_target(package, arguments.target)
    receipt = {
        "schema": "grapt.release.signing-check.v1",
        "package": str(package),
        "host_os": os.name,
        "identities": available_identities(),
        **result,
        "attests": (
            "a signature verifier was RUN where one exists for this target on "
            "this host, and the state reported per binary is what it observed. "
            "`unsigned` means a verifier looked and found no signature; "
            "`unverifiable` means no verifier was available here and is NOT a "
            "statement about the bytes; `no-signature-scheme` means this release "
            "applies none for this target. Signing identities are enumerated "
            "separately, because a machine that COULD sign has not thereby "
            "signed anything."
        ),
    }
    if arguments.receipt:
        arguments.receipt.parent.mkdir(parents=True, exist_ok=True)
        arguments.receipt.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n",
                                     encoding="utf-8")
    print(json.dumps(receipt, indent=2, sort_keys=True))
    # A package that is not signed is not a failure of this CHECK. The check
    # fails only if it could not run at all.
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
