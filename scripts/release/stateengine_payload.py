"""Pinned, separately licensed StateEngine payloads; no download or npm install."""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import signal
import stat
import subprocess
import sys
import tempfile

SCHEMA = "stateengine.se-payload.v1"
NATIVE_SCHEMA = "stateengine.native-payload/v1"
# The committed pins sit beside StateEngine-supplied content and keep their spelling
# until producer and consumer move together.
PIN_SCHEMA = "secode.stateengine-payload-pins.v1"  # grapt-rename: keep
MANIFEST = "stateengine-payload.json"
NATIVE_MANIFEST = "native-payload.json"
TARGETS = {"x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"}
MAX_FILES = 8192
MAX_DEPTH = 16
MAX_MANIFEST = 2 * 1024 * 1024
MAX_FILE = 160 * 1024 * 1024
MAX_TOTAL = 512 * 1024 * 1024
SMOKE_SECONDS = 15


class PayloadError(RuntimeError):
    """An invalid payload is refused before its entrypoint executes."""


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise PayloadError("StateEngine JSON contains a duplicate key")
        result[key] = value
    return result


def linked(info):
    return stat.S_ISLNK(info.st_mode) or bool(getattr(info, "st_file_attributes", 0) & 0x400)


def regular(path, cap):
    info = path.lstat()
    if linked(info) or not stat.S_ISREG(info.st_mode):
        raise PayloadError("StateEngine input is linked or not a regular file")
    if info.st_size > cap:
        raise PayloadError("StateEngine input exceeds its byte bound")
    return info


def load(path):
    regular(path, MAX_MANIFEST)
    with path.open("rb") as source:
        raw = source.read(MAX_MANIFEST + 1)
    if len(raw) > MAX_MANIFEST:
        raise PayloadError("StateEngine manifest exceeds its byte bound")
    try:
        document = json.loads(raw, object_pairs_hook=unique)
    except (ValueError, UnicodeError) as error:
        raise PayloadError("StateEngine JSON is malformed") from error
    if not isinstance(document, dict):
        raise PayloadError("StateEngine JSON must be an object")
    return document, raw


def relative(value):
    if (not isinstance(value, str) or not value or len(value) > 1024
            or "\\" in value or ":" in value or any(ord(c) < 32 for c in value)):
        raise PayloadError("StateEngine file has an unsafe relative path")
    path = PurePosixPath(value)
    if (path.is_absolute() or path.as_posix() != value
            or any(part in {".", ".."} for part in path.parts)
            or len(path.parts) > MAX_DEPTH):
        raise PayloadError("StateEngine file has an unsafe relative path")
    return path


def sha(value, length=64):
    return isinstance(value, str) and re.fullmatch(f"[0-9a-f]{{{length}}}", value) is not None


def fingerprint(path):
    before = regular(path, MAX_FILE)
    digest = hashlib.sha256()
    count = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            count += len(chunk)
            if count > MAX_FILE:
                raise PayloadError("StateEngine file grew beyond its byte bound")
            digest.update(chunk)
    after = regular(path, MAX_FILE)
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
            after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns) or count != before.st_size:
        raise PayloadError("StateEngine file changed while hashing")
    return {"sha256": digest.hexdigest(), "bytes": count}


def inventory(root):
    found = set()
    pending = [(root, 0)]
    entries = 0
    while pending:
        directory, depth = pending.pop()
        info = directory.lstat()
        if linked(info) or not stat.S_ISDIR(info.st_mode) or depth > MAX_DEPTH:
            raise PayloadError("StateEngine directory is linked or exceeds its nesting bound")
        for child in directory.iterdir():
            entries += 1
            if entries > MAX_FILES * 2:
                raise PayloadError("StateEngine directory census exceeds its bound")
            info = child.lstat()
            if linked(info):
                raise PayloadError("StateEngine payload contains a symlink or reparse point")
            if stat.S_ISDIR(info.st_mode):
                pending.append((child, depth + 1))
            elif stat.S_ISREG(info.st_mode):
                name = child.relative_to(root).as_posix()
                relative(name)
                found.add(name)
            else:
                raise PayloadError("StateEngine payload contains a nonregular file")
    return found


def manifest_name(root):
    found = []
    for name in (MANIFEST, NATIVE_MANIFEST):
        try:
            (root / name).lstat()
        except FileNotFoundError:
            continue
        found.append(name)
    if len(found) > 1:
        raise PayloadError("StateEngine payload has ambiguous manifests")
    if not found:
        raise FileNotFoundError("StateEngine payload manifest is missing")
    return found[0]


def verify(root, target, pin=None):
    if target not in TARGETS:
        raise PayloadError("StateEngine target is unsupported")
    info = root.lstat()
    if linked(info) or not stat.S_ISDIR(info.st_mode):
        raise PayloadError("StateEngine payload root must be a real directory")
    manifest = manifest_name(root)
    native = manifest == NATIVE_MANIFEST
    document, raw = load(root / manifest)
    digest = hashlib.sha256(raw).hexdigest()
    if (document.get("schema") != (NATIVE_SCHEMA if native else SCHEMA)
            or (native and (document.get("runtime") != "rust-embedded" or "node" in document))
            or document.get("name") != "stateengine"
            or document.get("target") != target or not sha(document.get("source_commit"), 40)
            or not isinstance(document.get("version"), str)
            or re.fullmatch(r"[0-9A-Za-z][0-9A-Za-z.+-]{0,63}", document["version"]) is None):
        raise PayloadError("StateEngine manifest identity is unsupported")
    observed_pin = {"manifest_sha256": digest, "version": document["version"],
                    "source_commit": document["source_commit"]}
    if pin is not None and pin != observed_pin:
        raise PayloadError("StateEngine manifest does not match its committed pin")
    files = document.get("files")
    if not isinstance(files, dict) or not 1 <= len(files) <= MAX_FILES:
        raise PayloadError("StateEngine file inventory exceeds its count bound")
    folded = set()
    total = 0
    for name, attestation in files.items():
        relative(name)
        if name in {MANIFEST, NATIVE_MANIFEST} or name.casefold() in folded:
            raise PayloadError("StateEngine inventory has a reserved or duplicate file")
        folded.add(name.casefold())
        if (not isinstance(attestation, dict) or set(attestation) != {"sha256", "bytes"}
                or not sha(attestation.get("sha256")) or type(attestation.get("bytes")) is not int
                or not 0 <= attestation["bytes"] <= MAX_FILE):
            raise PayloadError("StateEngine file attestation is malformed or exceeds its bound")
        total += attestation["bytes"]
        if total > MAX_TOTAL:
            raise PayloadError("StateEngine payload exceeds its total byte bound")
    required = [document.get("entry"), document.get("license_path"),
                document.get("notices_path")]
    if not native:
        node = document.get("node")
        if (not isinstance(node, dict) or not isinstance(node.get("version"), str)
                or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", node["version"])
                or not isinstance(node.get("source_url"), str)
                or not node["source_url"].startswith("https://nodejs.org/")
                or len(node["source_url"]) > 2048 or not sha(node.get("source_sha256"))):
            raise PayloadError("StateEngine bundled Node provenance is malformed")
        required += [node.get("license_path"),
                     "node.exe" if target.endswith("windows-msvc") else "node"]
    for name in required:
        relative(name)
        if name not in files:
            raise PayloadError("StateEngine entry, runtime or license is absent from its inventory")
    if inventory(root) != set(files) | {manifest}:
        raise PayloadError("StateEngine payload has unlisted or missing files")
    for name, attestation in files.items():
        if fingerprint(root / name) != attestation:
            raise PayloadError(f"StateEngine digest or byte count differs: {name}")
    return document, observed_pin


async def version_output(entry, environment, directory):
    options = ({"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP}
               if os.name == "nt" else {"start_new_session": True})
    command = ([sys.executable, str(Path(__file__).with_name('windows_version_worker.py')), str(entry)]
               if os.name == 'nt' else [str(entry), '--version'])
    process = await asyncio.create_subprocess_exec(
        *command, env=environment, cwd=directory,
        stdin=subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT, limit=4096, **options)

    async def collect():
        output = bytearray()
        while chunk := await process.stdout.read(4097 - len(output)):
            output.extend(chunk)
            if len(output) > 4096:
                raise PayloadError("StateEngine version output exceeds its byte bound")
        return await process.wait(), bytes(output)

    try:
        return await asyncio.wait_for(collect(), timeout=SMOKE_SECONDS)
    except TimeoutError as error:
        raise PayloadError("StateEngine version smoke exceeded its time bound") from error
    finally:
        # A version process can exit while a descendant retains stdout. On
        # POSIX, retire that owned process group even after its leader exits.
        if os.name != "nt":
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        if process.returncode is None:
            if os.name == "nt":
                # Killing the retained worker closes its private job handle;
                # Windows retires the whole tree without a PID-based lookup.
                process.kill()
            else:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            await asyncio.wait_for(process.wait(), timeout=5)


def smoke(root, document):
    windows = document["target"].endswith("windows-msvc")
    if windows != (os.name == "nt"):
        raise PayloadError("StateEngine version smoke must run on its target platform")
    with tempfile.TemporaryDirectory(prefix="grapt-se-smoke-") as temporary:
        home = Path(temporary) / "home"
        home.mkdir()
        trap = Path(temporary) / "bin"
        trap.mkdir()
        name = "node.cmd" if windows else "node"
        (trap / name).write_text("@exit /b 77\r\n" if windows else "#!/bin/sh\nexit 77\n")
        if not windows:
            (trap / name).chmod(0o700)
        environment = {key: value for key, value in os.environ.items()
                       if not key.upper().startswith(("STATEENGINE_", "SECODE_STATEENGINE_", "NODE_"))
                       and key.upper() != "SECODE_SESSION_ID"}
        environment.update(HOME=str(home), USERPROFILE=str(home),
                           LOCALAPPDATA=str(home / "local"), APPDATA=str(home / "roaming"),
                           XDG_CONFIG_HOME=str(home / "config"), XDG_DATA_HOME=str(home / "data"),
                           XDG_CACHE_HOME=str(home / "cache"),
                           TEMP=str(home / "temp"), TMP=str(home / "temp"),
                           PATH=str(trap) if windows else str(trap) + os.pathsep + os.defpath)
        code, output = asyncio.run(version_output(root / document["entry"], environment, root))
        if code or document["version"].encode() not in output:
            raise PayloadError("StateEngine version smoke failed or used ambient Node")
        if any(home.iterdir()):
            raise PayloadError("StateEngine version smoke wrote user state")
        return {"exit": 0, "output": output.decode("utf-8", errors="strict").strip()}


def copy_regular(source, destination, expected):
    """Copy a retained regular input, without following a swapped symlink."""
    before = regular(source, MAX_FILE)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    flags |= getattr(os, "O_BINARY", 0)
    descriptor = os.open(source, flags)
    with os.fdopen(descriptor, "rb") as reader:
        opened = os.fstat(reader.fileno())
        if (not stat.S_ISREG(opened.st_mode)
                or (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino)):
            raise PayloadError("StateEngine copy input changed identity")
        digest = hashlib.sha256()
        count = 0
        with destination.open("xb") as writer:
            while chunk := reader.read(1024 * 1024):
                count += len(chunk)
                if count > expected["bytes"]:
                    raise PayloadError("StateEngine copy input grew beyond its attested size")
                digest.update(chunk)
                writer.write(chunk)
        after = regular(source, MAX_FILE)
        if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
                after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
            raise PayloadError("StateEngine copy input changed while copying")
        if {"sha256": digest.hexdigest(), "bytes": count} != expected:
            raise PayloadError("StateEngine copied input differs from its attestation")
    destination.chmod(0o755 if before.st_mode & 0o111 else 0o644)


def assemble(repository, output, target, payload):
    if payload is None:
        return {"status": "not-included", "reason": "no pinned StateEngine payload supplied"}
    pins, _raw = load(repository / "packages/stateengine/payload-pins.json")
    pin = pins.get("targets", {}).get(target)
    if pins.get("schema") != PIN_SCHEMA or not isinstance(pin, dict):
        raise PayloadError("No StateEngine payload is pinned for this target")
    document, observed = verify(payload, target, pin)
    manifest = manifest_name(payload)
    destination = output / "stateengine"
    destination.mkdir(parents=True)
    for name in [manifest, *document["files"]]:
        copied = destination / name
        copied.parent.mkdir(parents=True, exist_ok=True)
        expected = document["files"].get(name, {
            "sha256": observed["manifest_sha256"],
            "bytes": regular(payload / manifest, MAX_MANIFEST).st_size,
        })
        copy_regular(payload / name, copied, expected)
    # Verify copied bytes, including the pinned manifest, before executing them.
    copied_document, _pin = verify(destination, target, pin)
    probe = smoke(destination, copied_document)
    verify(destination, target, pin)
    assets = {"stateengine/" + name: row for name, row in document["files"].items()}
    assets["stateengine/" + manifest] = fingerprint(destination / manifest)
    return {"status": "included", "version": document["version"],
            "source_commit": document["source_commit"], "target": target,
            "entry": "stateengine/" + document["entry"], **observed,
            "license": "separately licensed; see stateengine/" + document["license_path"],
            "smoke": probe, "assets": assets}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload", required=True, type=Path)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--pins", required=True, type=Path)
    arguments = parser.parse_args()
    _document, pin = verify(arguments.payload, arguments.target)
    pins = {"schema": PIN_SCHEMA, "targets": {}}
    if arguments.pins.exists():
        pins, _raw = load(arguments.pins)
        if pins.get("schema") != PIN_SCHEMA or not isinstance(pins.get("targets"), dict):
            raise PayloadError("Existing StateEngine pin document is malformed")
    pins["targets"][arguments.target] = pin
    arguments.pins.parent.mkdir(parents=True, exist_ok=True)
    arguments.pins.write_text(json.dumps(pins, sort_keys=True, indent=2) + "\n")
    print(f"Pinned StateEngine {pin['version']} for {arguments.target}")


if __name__ == "__main__":
    main()
