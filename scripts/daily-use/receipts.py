"""Bounded reads, and receipt schemas checked rather than trusted.

Split out of `assemble_candidate.py` because two independent defects lived in
what looked like incidental helpers, and both were vacuous checks -- code that
ran, raised nothing, and established nothing.

THE FIRST DEFECT, and it is the reason this file exists. The assembler
validated the reused desktop host with

    if receipt.get("sha256") not in (None, host_sha):
        raise ValueError("the host receipt does not describe these host bytes")

The governed receipt has no top-level `sha256`. It nests the artifact under
`host`, as `{"host": {"bytes": ..., "sha256": ...}}`. So `receipt.get("sha256")`
returned `None` against EVERY real receipt, `None` was an accepted value, and
the check passed unconditionally. It would have accepted a receipt for entirely
different bytes, a receipt with no artifact identity at all, and an empty JSON
object. I wrote it, and the manifest it produced named a provenance nothing had
verified.

THE SECOND DEFECT is allocation before bounds. `digest` read the whole file
into memory and then asked whether it was too large, which is the check
happening after the cost it exists to prevent. The governed host receipt here
is 257 KB; nothing in the argument vector bounds what a caller may point at.
Size is now read from the directory entry and refused before a byte is read,
and hashing streams in fixed blocks so peak memory does not track file size.
"""

import hashlib
import json
import re
import stat
from pathlib import Path

#: Largest artifact this assembly will hash. The desktop host is ~27 MB.
MAX_ARTIFACT_BYTES = 64 * 1024 * 1024

#: Largest receipt this assembly will parse. The governed host receipt carries
#: a per-file renderer inventory and is ~257 KB; JSON parsing allocates several
#: times the input, so this is deliberately far below the artifact bound.
MAX_RECEIPT_BYTES = 8 * 1024 * 1024

_READ_BLOCK = 1024 * 1024

_SHA256 = re.compile(r"\A[0-9a-f]{64}\Z")
_COMMIT = re.compile(r"\A[0-9a-f]{40}\Z")


class ProvenanceError(ValueError):
    """A receipt did not establish what the manifest would have claimed."""


def _ordinary_file(path: Path) -> None:
    """Refuse anything that is not an ordinary, non-reparse file.

    `resolve()` and `is_file()` FOLLOW LINKS, so neither answers this. A
    junction or symlink pointed at something else is a different artifact than
    the path we were handed, and provenance that resolves through one is
    provenance for a file the caller did not name.
    """
    try:
        raw = path.lstat()
    except OSError as error:
        raise ProvenanceError(f"{path} cannot be inspected: {error}") from error
    if stat.S_ISLNK(raw.st_mode):
        raise ProvenanceError(f"{path} is a symbolic link, not an ordinary file")
    if getattr(raw, "st_reparse_tag", 0):
        raise ProvenanceError(f"{path} is a reparse point, not an ordinary file")
    if not stat.S_ISREG(raw.st_mode):
        raise ProvenanceError(f"{path} is not an ordinary file")


def _bounded_size(path: Path, bound: int) -> int:
    """The file's size from its own directory entry, refused before any read.

    THIS IS ONLY A PREFLIGHT. It is not, by itself, a bound: the file may grow
    between this `stat` and the read that follows. Every caller must ALSO bound
    what it actually consumes -- see `read_bounded` and `digest`.
    """
    _ordinary_file(path)
    size = path.stat().st_size
    if size > bound:
        raise ProvenanceError(f"{path} is {size} bytes, over the {bound}-byte bound")
    return size


def read_bounded(path: Path, bound: int) -> bytes:
    """Read at most `bound + 1` bytes, and refuse at `bound + 1`.

    THE DEFECT THIS CLOSES, found by executing the code. I checked
    `stat` and then called `read_text`, which is unbounded. Given a file that
    reports one byte at `stat` and supplies twenty-one at read, `load_receipt`
    ACCEPTED the oversized payload -- the bound was a preflight I had described
    as a limit. No enormous file is needed to show it; the two observations are
    simply of different instants.

    Reading `bound + 1` is deliberate. It is the smallest read that can tell
    "exactly at the bound" from "over it", and it caps what an adversarial or
    merely racing file can make us allocate at one byte over the limit.
    """
    _bounded_size(path, bound)
    with path.open("rb") as handle:
        payload = handle.read(bound + 1)
    if len(payload) > bound:
        raise ProvenanceError(
            f"{path} supplied more than the {bound}-byte bound after its size was checked"
        )
    return payload


def copy_bounded(source: Path, destination: Path, bound: int = MAX_ARTIFACT_BYTES) -> int:
    """Copy a file, refusing at the bound rather than after it.

    `shutil.copy2` copies whatever is there. Preflighting the size and then
    handing the file to an unbounded copy is the same defect as preflighting
    and then handing it to an unbounded read: the file may grow in between,
    and the cost the bound exists to prevent is paid anyway. This streams and
    stops the moment the copy would exceed the bound, removing the partial
    output so no oversized artifact is left behind to be hashed later.
    """
    _bounded_size(source, bound)
    written = 0
    try:
        with source.open("rb") as reader, destination.open("wb") as writer:
            while True:
                block = reader.read(_READ_BLOCK)
                if not block:
                    break
                written += len(block)
                if written > bound:
                    raise ProvenanceError(
                        f"{source} exceeded the {bound}-byte bound while being copied"
                    )
                writer.write(block)
    except ProvenanceError:
        destination.unlink(missing_ok=True)
        raise
    return written


def digest(path: Path, bound: int = MAX_ARTIFACT_BYTES) -> tuple[str, int]:
    """`(sha256, bytes)` for a file, streamed under a pre-checked bound.

    The length returned is COUNTED WHILE HASHING, not taken from `stat`. They
    are two observations of the same file at different instants, and a mismatch
    means the file changed under us -- which is refused rather than reported as
    a hash of one length and a size of another.
    """
    declared = _bounded_size(path, bound)
    hasher = hashlib.sha256()
    counted = 0
    with path.open("rb") as handle:
        while True:
            block = handle.read(_READ_BLOCK)
            if not block:
                break
            counted += len(block)
            if counted > bound:
                raise ProvenanceError(f"{path} grew past the {bound}-byte bound while hashing")
            hasher.update(block)
    if counted != declared:
        raise ProvenanceError(f"{path} changed size while hashing: {declared} then {counted}")
    return hasher.hexdigest(), counted


def load_receipt(path: Path) -> dict:
    """Parse a receipt under the receipt bound, as an object.

    A receipt that is a list, a string or a number is malformed here rather
    than becoming a `dict`-shaped `.get` that returns `None` for everything --
    which is precisely how the original check became vacuous.
    """
    # BOUNDED BEFORE DECODING, and before parsing. Decoding an unbounded
    # payload to text is itself the allocation the bound exists to prevent.
    payload = read_bounded(path, MAX_RECEIPT_BYTES)
    try:
        parsed = json.loads(payload.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise ProvenanceError(f"{path} is not readable JSON: {error}") from error
    if not isinstance(parsed, dict):
        raise ProvenanceError(f"{path} is a {type(parsed).__name__}, not a receipt object")
    return parsed


def _field(receipt: dict, path: Path, *keys: str):
    """Fetch a REQUIRED nested field, naming the receipt and the exact path.

    There is no default. A missing field raises; it does not become `None` for
    a later comparison to wave through.
    """
    current = receipt
    walked: list[str] = []
    for key in keys:
        walked.append(key)
        if not isinstance(current, dict) or key not in current:
            raise ProvenanceError(f"{path} has no {'.'.join(walked)}")
        current = current[key]
    return current


def required_sha256(receipt: dict, path: Path, *keys: str) -> str:
    value = _field(receipt, path, *keys)
    if not isinstance(value, str) or not _SHA256.match(value):
        raise ProvenanceError(f"{path}: {'.'.join(keys)} is not a sha256 digest: {value!r}")
    return value


def required_commit(receipt: dict, path: Path, *keys: str) -> str:
    value = _field(receipt, path, *keys)
    if not isinstance(value, str) or not _COMMIT.match(value):
        raise ProvenanceError(f"{path}: {'.'.join(keys)} is not a commit identity: {value!r}")
    return value


def required_bytes(receipt: dict, path: Path, *keys: str) -> int:
    value = _field(receipt, path, *keys)
    # `bool` is an `int` in Python, and `True == 1` would satisfy a naive
    # comparison against a one-byte artifact.
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ProvenanceError(f"{path}: {'.'.join(keys)} is not a byte count: {value!r}")
    return value


def required_text(receipt: dict, path: Path, *keys: str) -> str:
    value = _field(receipt, path, *keys)
    if not isinstance(value, str) or not value:
        raise ProvenanceError(f"{path}: {'.'.join(keys)} is not a non-empty string: {value!r}")
    return value


def must_match_artifact(
    label: str,
    receipt_path: Path,
    receipt_sha: str,
    receipt_bytes: int,
    observed: tuple[str, int],
    artifact_path: Path,
) -> None:
    """The receipt must describe THESE bytes: both length and hash.

    Length is checked as well as hash, and not because a sha256 collision is
    expected. It is checked because the two fields are written independently,
    and a receipt whose length disagrees with its own digest is a receipt whose
    generator is not describing one artifact -- which is worth refusing on its
    own terms.
    """
    observed_sha, observed_bytes = observed
    if receipt_bytes != observed_bytes:
        raise ProvenanceError(
            f"{label}: {receipt_path} says {receipt_bytes} bytes, {artifact_path} is {observed_bytes}"
        )
    if receipt_sha != observed_sha:
        raise ProvenanceError(
            f"{label}: {receipt_path} says sha256 {receipt_sha}, {artifact_path} is {observed_sha}"
        )
