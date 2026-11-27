#!/usr/bin/env python3
"""Actual Linux UPR package/session/forward-store journey, using isolated profiles."""

import argparse
import base64
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import sys
from contextlib import ExitStack
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "native-tui"))
from scripted_profile import ScriptedProfile
import subprocess
import time
import urllib.error
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "build"))
from gate import stop_group

MARKER = b"Grapt UPR disposable profile v1\n"


def command(arguments, root, environment, label, timeout=30, success=True):
    arguments = [str(value) for value in arguments]
    outputs = [root / f"{label}.{stream}" for stream in ("stdout", "stderr")]
    with outputs[0].open("xb") as stdout, outputs[1].open("xb") as stderr:
        child = subprocess.Popen(arguments, cwd=root, env=environment, stdout=stdout,
                                 stderr=stderr, start_new_session=True)
        try:
            code = child.wait(timeout=timeout)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            stop_group(child)
            raise
    if any(path.stat().st_size > 1024 * 1024 for path in outputs):
        raise RuntimeError(f"{label}: fixture output exceeded 1 MiB")
    result = subprocess.CompletedProcess(arguments, code, *(path.read_text() for path in outputs))
    if success and result.returncode:
        raise RuntimeError(f"{label} refused ({result.returncode}): {result.stderr[:4096]}")
    return result


def request(profile, route, body=None):
    values = dict(line.split("=", 1) for line in (profile / "registration").read_text().splitlines() if "=" in line)
    credential = (profile / "basic.credential").read_text()
    headers = {"Authorization": "Basic " + base64.b64encode(credential.encode()).decode()}
    data = None if body is None else json.dumps(body).encode()
    if data is not None:
        headers["Content-Type"] = "application/json"
    query = urllib.request.Request(values["endpoint"].rstrip("/") + route, data=data, headers=headers)
    try:
        response = urllib.request.urlopen(query, timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        value = response.read(1024 * 1024 + 1)
        if len(value) > 1024 * 1024 or response.status != 200:
            raise RuntimeError(f"UPR request refused: {route} ({response.status})")
        return json.loads(value)


@contextmanager
def serving(product, profile, root, environment, label):
    with (root / f"{label}.stdout").open("wb") as stdout, (root / f"{label}.stderr").open("wb") as stderr:
        child = subprocess.Popen([str(product), "serve-terminal", "--service", str(profile), "--port", "0"],
                                 cwd=root, env=environment, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr)
        try:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError(f"UPR {label}: product exited before readiness")
                try:
                    values = dict(line.split("=", 1) for line in (profile / "registration").read_text().splitlines() if "=" in line)
                    ready = json.loads((profile / "serve-ready.json").read_text())
                    if ready.get("observed_endpoint") == values.get("endpoint"):
                        break
                except FileNotFoundError:
                    pass
                time.sleep(0.05)
            else:
                raise RuntimeError(f"UPR {label}: product readiness exceeded 15 seconds")
            yield child
            child.stdin.close()
            code = child.wait(timeout=15)
            if code:
                raise RuntimeError(f"UPR {label}: product drain failed ({code})")
        finally:
            if child.poll() is None:
                if not child.stdin.closed:
                    child.stdin.close()
                try:
                    child.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)


def identity(package):
    manifest = json.loads((package / "release-manifest.json").read_text())
    value = manifest["engine_version"]
    if len(value) != 40 or any(character not in "0123456789abcdef" for character in value):
        raise RuntimeError("UPR package identity is invalid")
    return value


def snapshot(profile):
    # Store bytes and sidecar presence are evidence; absence is not an empty file.
    return {name: hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None
            for name in ("database.sqlite3", "database.sqlite3-wal", "database.sqlite3-shm", "database.sqlite3-journal")
            for path in (profile / name,)}


def installation(prefix):
    return {"pointer": os.readlink(prefix / "current"),
            "state": (prefix / "state.json").read_bytes().hex(),
            "history": (prefix / "history.jsonl").read_bytes().hex(),
            "intent": (prefix / "transition.json").read_bytes().hex() if (prefix / "transition.json").exists() else None}


def main():
    with ExitStack() as stack:
        return run(stack)


def run(stack):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-a", required=True, type=Path)
    parser.add_argument("--package-b", required=True, type=Path)
    parser.add_argument("--forward-test-bin", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    args = parser.parse_args()
    packages = [args.package_a.resolve(strict=True), args.package_b.resolve(strict=True)]
    a, b = [identity(package) for package in packages]
    if a == b:
        raise RuntimeError("UPR requires two genuine distinct build identities")
    forward_test = args.forward_test_bin.resolve(strict=True)
    root = args.scratch.resolve()
    root.mkdir(mode=0o700)  # Never reuse or overwrite an existing profile.
    environment = dict(os.environ)
    for key, name in [("GRAPT_TEST_HOME", "home"), ("GRAPT_CONFIG_DIR", "configuration")]:
        directory = root / name
        directory.mkdir(mode=0o700)
        environment[key] = str(directory)
    prefix, profile = root / "installation", root / "profile"
    cli = packages[1] / "grapt"
    rows = []

    def update(package, label):
        command([cli, "update", "--from", package, "--prefix", prefix], root, environment, label, timeout=120)
        expected = identity(package)
        current = prefix / "current/bin"
        version = command([current / "grapt", "--version"], root, environment, label + "-version").stdout
        state = json.loads((prefix / "state.json").read_text())
        if expected not in version or state["current"] != expected or identity(current) != expected:
            raise RuntimeError(f"UPR {label}: installed build identity disagrees")
        rows.append({"case": label, "build": expected})
        return current

    current = update(packages[0], "install-A")
    stack.enter_context(ScriptedProfile(current / "grapt-server", profile, environment,
        [{"tool":"read", "arguments":{"path":"example.txt"}}]))
    (profile / "example.txt").write_text("42\n", encoding="utf-8")
    with serving(current / "grapt-server", profile, root, environment, "session-A"):
        session = request(profile, "/api/session", {"location": {"directory": str(profile)}})["data"]
        session_id = session["id"]
        command([current / "grapt", "run", "--service", profile, "--session", session_id, "read example.txt"],
                root, environment, "read-A")
        messages = request(profile, f"/api/session/{session_id}/message?order=asc")
        reads = [part for message in messages.get("data", []) for part in message.get("content", [])
                 if part.get("type") == "tool" and part.get("name") == "read"]
        if len(reads) != 1 or reads[0].get("state", {}).get("status") != "completed":
            raise RuntimeError("UPR baseline did not persist a completed read")
    update(packages[1], "update-B")
    with serving(prefix / "current/bin/grapt-server", profile, root, environment, "session-B"):
        if request(profile, f"/api/session/{session_id}/message?order=asc") != messages:
            raise RuntimeError("UPR update lost or changed durable session history")
    command([cli, "revert", "--prefix", prefix, "--profile", profile], root, environment, "revert-A")
    if identity(prefix / "current/bin") != a or json.loads((prefix / "state.json").read_text())["current"] != a:
        raise RuntimeError("UPR revert did not restore A")
    version = command([prefix / "current/bin/grapt", "--version"], root, environment, "revert-version").stdout
    if a not in version:
        raise RuntimeError("UPR reverted executable did not attest A")
    with serving(prefix / "current/bin/grapt-server", profile, root, environment, "session-reverted-A"):
        if request(profile, f"/api/session/{session_id}/message?order=asc") != messages:
            raise RuntimeError("UPR revert lost or changed durable session history")
    rows.append({"case": "revert-A", "build": a, "session": session_id, "history_preserved": True})

    current = update(packages[1], "before-forward-refusal")
    future = root / "future-profile"
    command([current / "grapt-server", "init-profile", "--service", future], root, environment, "forward-init", timeout=90)
    (future / "upr-fixture-marker").write_bytes(MARKER)
    forward_environment = dict(environment, GRAPT_UPR_FORWARD_PROFILE=str(future))
    command([forward_test, "--ignored", "--nocapture"], root, forward_environment, "forward-migration", timeout=120)
    store_before, install_before = snapshot(future), installation(prefix)
    refused = command([cli, "revert", "--prefix", prefix, "--profile", future], root, environment,
                      "forward-revert-refusal", success=False)
    if refused.returncode != 2 or "revert refused" not in refused.stderr or "grapt-migrate disposition" not in refused.stderr:
        raise RuntimeError("UPR future-store revert lacked its recovery refusal")
    if snapshot(future) != store_before or installation(prefix) != install_before:
        raise RuntimeError("UPR future-store refusal changed store or installation")
    older = command([packages[0] / "grapt-server", "serve", "--service", future, "--port", "0"], root,
                    environment, "forward-older-start", timeout=15, success=False)
    if older.returncode == 0 or snapshot(future) != store_before:
        raise RuntimeError("UPR older start failed to protect another profile's future store")
    if "grapt-migrate disposition" not in older.stderr or "grapt-migrate restore" not in older.stderr:
        raise RuntimeError("UPR older start omitted the migration recovery commands")
    rows.append({"case": "forward-store-refusal", "store_unchanged": True,
                 "installation_unchanged": True, "older_start_exit": older.returncode})
    (root / "witness.json").write_text(json.dumps({"passed": True, "package_a": a, "package_b": b,
        "rows": rows, "external_provider_calls": 0}, indent=2) + "\n")
    print("UPR actual A→B→A and forward-store refusals passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"UPR native journey failed: {error}", file=sys.stderr)
        raise SystemExit(1)
