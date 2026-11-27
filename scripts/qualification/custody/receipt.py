"""Project-local PLR evidence custody. No global campaign state is created."""

import hashlib
import json
import os
import subprocess
from pathlib import Path


SCHEMA = "qualification-custody/v1"


class CustodyError(RuntimeError):
    pass


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git(repo, *args):
    result = subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        raise CustodyError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def candidate_identity(repo):
    repo = Path(repo).resolve(strict=True)
    return {
        "commit": git(repo, "rev-parse", "HEAD"),
        "tree": git(repo, "rev-parse", "HEAD^{tree}"),
        "status": git(repo, "status", "--porcelain", "--untracked-files=all"),
    }


def require_clean(identity, name):
    if identity["status"]:
        raise CustodyError(f"{name} is dirty: {identity['status'][:200]}")
    for key in ("commit", "tree"):
        if len(identity[key]) != 40:
            raise CustodyError(f"{name} has no exact {key}")


def harness_identity(paths):
    return {str(path): sha256_file(path) for path in paths}


def resolve_artifact(json_output, source_path, target_dir):
    """Take the executable named by Cargo's compiler-artifact JSON, never a glob."""
    expected = Path(source_path).resolve(strict=True)
    target = Path(target_dir).resolve(strict=True)
    matches = []
    for line in json_output.splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if row.get("reason") != "compiler-artifact" or not row.get("executable"):
            continue
        declared = row.get("target", {}).get("src_path")
        if not isinstance(declared, str) or Path(declared).resolve() != expected:
            continue
        artifact = Path(row["executable"]).resolve(strict=True)
        if not artifact.is_relative_to(target) or not artifact.is_file():
            raise CustodyError("compiler artifact is absent or outside isolated target")
        matches.append({
            "path": str(artifact),
            "sha256": sha256_file(artifact),
            "compiler_row_sha256": hashlib.sha256(line.encode("utf-8")).hexdigest(),
            "compiler_row_json": line,
        })
    if len(matches) != 1:
        raise CustodyError(f"expected one exact compiler artifact, found {len(matches)}")
    return matches[0]


def require_compiler_row(artifact, name):
    """Bind a recorded executable to the exact Cargo row that named it."""
    encoded = artifact.get("compiler_row_json")
    digest = artifact.get("compiler_row_sha256")
    if not isinstance(encoded, str) or not digest:
        raise CustodyError(f"{name}: compiler artifact row is absent")
    if hashlib.sha256(encoded.encode("utf-8")).hexdigest() != digest:
        raise CustodyError(f"{name}: compiler artifact row digest differs")
    try:
        row = json.loads(encoded)
    except json.JSONDecodeError as error:
        raise CustodyError(f"{name}: compiler artifact row is invalid JSON") from error
    if not isinstance(row, dict) or row.get("reason") != "compiler-artifact":
        raise CustodyError(f"{name}: compiler artifact row has wrong reason")
    if row.get("executable") != artifact["path"]:
        raise CustodyError(f"{name}: compiler row executable differs from recorded artifact")


def load_manifest(encoded):
    """Mutation rows by id, pinned to the digest of the bytes they came from."""
    try:
        manifest = json.loads(encoded)
    except ValueError as error:
        raise CustodyError("mutation manifest is invalid JSON") from error
    rows = manifest.get("mutations") if isinstance(manifest, dict) else None
    if not isinstance(rows, list) or not rows:
        raise CustodyError("mutation manifest has no rows")
    by_id = {}
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str):
            raise CustodyError("mutation manifest has a row without an id")
        if row["id"] in by_id:
            raise CustodyError(f"mutation manifest repeats {row['id']}")
        by_id[row["id"]] = row
    return {"sha256": hashlib.sha256(encoded).hexdigest(), "rows": by_id}


def read_manifest(path):
    try:
        encoded = Path(path).read_bytes()
    except OSError as error:
        raise CustodyError(f"absent or unreadable mutation manifest: {error}") from error
    return load_manifest(encoded)


def require_source_unchanged(record, name):
    """Only an explicit False is measured unchanged-source evidence."""
    observed = record.get("source_changed")
    if observed is not False:
        raise CustodyError(
            f"{name}: source changed during run" if observed is True else
            f"{name}: source-change observation is absent or malformed: {observed!r}"
        )


def verify_entry(entry, row):
    """One receipt entry against its declared row in the pinned manifest."""
    required = ("id", "expected_diagnostic", "diagnostic", "artifact",
                "build", "red", "green", "source_changed")
    missing = [key for key in required if key not in entry]
    if missing:
        raise CustodyError(f"{entry.get('id', '?')}: missing {','.join(missing)}")
    name = entry["id"]
    require_source_unchanged(entry, name)
    if not isinstance(row, dict) or row.get("id") != name:
        raise CustodyError(f"{name}: no declared row in the pinned manifest")
    declared = row.get("expected_diagnostic")
    if not isinstance(declared, str) or not declared.strip():
        raise CustodyError(f"{name}: the pinned manifest declares no expected diagnostic")
    artifact = entry["artifact"]
    if not all(artifact.get(key) for key in
               ("path", "sha256", "sha256_after", "compiler_row_sha256", "compiler_row_json")):
        raise CustodyError(f"{name}: executable custody is incomplete")
    require_compiler_row(artifact, name)
    if artifact["sha256"] != artifact["sha256_after"]:
        raise CustodyError(f"{name}: executed artifact digest changed")
    if entry["red"].get("command", [None])[0] != artifact["path"]:
        raise CustodyError(f"{name}: stale or unbound executed binary")
    if entry["build"].get("exit") != 0 or not entry["build"].get("command"):
        raise CustodyError(f"{name}: compiler artifact build did not succeed")
    if entry["red"].get("exit") in (None, 0) or entry["red"].get("tests") != 1:
        raise CustodyError(f"{name}: mutant did not run one red witness")
    if entry["expected_diagnostic"] != declared:
        raise CustodyError(f"{name}: receipt expectation differs from the pinned manifest row")
    if not isinstance(entry["diagnostic"], str) or declared not in entry["diagnostic"]:
        raise CustodyError(f"{name}: wrong mutant diagnostic")
    if entry["green"].get("exit") != 0 or entry["green"].get("tests") != 1:
        raise CustodyError(f"{name}: restored-green control did not pass")
    green_artifact = entry["green"].get("artifact", {})
    if (entry["green"].get("command", [None])[0] != green_artifact.get("path")
            or not green_artifact.get("sha256")
            or green_artifact.get("sha256") != green_artifact.get("sha256_after")):
        raise CustodyError(f"{name}: restored-green artifact is unbound")
    require_compiler_row(green_artifact, f"{name} restored green")
    if not entry["red"].get("output_sha256") or not entry["green"].get("output_sha256"):
        raise CustodyError(f"{name}: command output receipt is absent")
    for phase in ("red", "green"):
        duration = entry[phase].get("duration_seconds")
        if not isinstance(duration, (int, float)) or duration < 0 or duration > 1200:
            raise CustodyError(f"{name}: {phase} execution duration is absent or unbounded")


def verify_receipt(receipt, expected_ids, manifest):
    """`manifest` comes from load_manifest over the bytes the harness pinned."""
    if not isinstance(receipt, dict) or receipt.get("schema") != SCHEMA:
        raise CustodyError("absent or unsupported custody receipt")
    if receipt.get("outcome") != "accepted":
        raise CustodyError("custody receipt is not accepted")
    before = receipt.get("candidate", {}).get("before")
    after = receipt.get("candidate", {}).get("after")
    if not before or not after:
        raise CustodyError("candidate identity receipt is absent")
    require_clean(before, "candidate before")
    require_clean(after, "candidate after")
    if before != after:
        raise CustodyError("candidate changed during run")
    harness = receipt.get("harness", {})
    if not harness.get("before") or harness.get("before") != harness.get("after"):
        raise CustodyError("harness digest changed or is absent")
    if manifest["sha256"] not in harness["after"].values():
        raise CustodyError("mutation manifest is not the one the harness pinned")
    require_source_unchanged(receipt, "candidate")
    entries = receipt.get("entries")
    if not isinstance(entries, list) or not entries:
        raise CustodyError("mutation entries are absent")
    ids = [entry.get("id") for entry in entries]
    if expected_ids is not None and ids != list(expected_ids):
        raise CustodyError(f"mutation ids {ids!r} differ from requested {list(expected_ids)!r}")
    if len(ids) != len(set(ids)):
        raise CustodyError("duplicate mutation ids")
    for entry in entries:
        verify_entry(entry, manifest["rows"].get(entry.get("id")))
    if sum(entry[phase]["duration_seconds"] for entry in entries
           for phase in ("red", "green")) > 1200:
        raise CustodyError("20-minute witness execution budget exceeded")
    return True


def write_receipt(path, receipt, expected_ids, manifest):
    """Exclusive durable write and independent readback before success."""
    verify_receipt(receipt, expected_ids, manifest)
    path = Path(path).resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    encoded = (json.dumps(receipt, sort_keys=True, indent=2) + "\n").encode("utf-8")
    with open(path, "xb") as handle:
        handle.write(encoded)
        handle.flush()
        os.fsync(handle.fileno())
    reread = path.read_bytes()
    if reread != encoded:
        raise CustodyError("custody receipt readback differs from write")
    verify_receipt(json.loads(reread), expected_ids, manifest)
    return hashlib.sha256(reread).hexdigest()


def read_receipt(path, expected_ids, manifest):
    try:
        receipt = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CustodyError(f"absent or unreadable custody receipt: {error}") from error
    verify_receipt(receipt, expected_ids, manifest)
    return receipt
