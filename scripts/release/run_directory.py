#!/usr/bin/env python3
"""Actual new-run location witness; isolated loopback provider, no paid calls."""

import argparse
import base64
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


def command(arguments, cwd, timeout=30, environment=None):
    result = subprocess.run(arguments, cwd=cwd, env=environment, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"fixture command failed: {arguments[0]} {arguments[1]}: {result.stderr[:4096]}")
    return result.stdout.strip()


def git(root, *arguments):
    return command(["git", "-C", str(root), *arguments], root)


def request(endpoint, credential, route, body=None):
    data = None if body is None else json.dumps(body).encode()
    headers = {"Authorization": "Basic " + base64.b64encode(credential.encode()).decode()}
    if data is not None:
        headers["Content-Type"] = "application/json"
    query = urllib.request.Request(endpoint.rstrip("/") + route, data=data, headers=headers)
    try:
        response = urllib.request.urlopen(query, timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        value = response.read(1024 * 1024 + 1)
        if len(value) > 1024 * 1024:
            raise RuntimeError("fixture response exceeded 1 MiB")
        return response.status, json.loads(value)


def main():
    with ExitStack() as stack:
        return run(stack)


def run(stack):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", required=True, type=Path)
    parser.add_argument("--product", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--require-narrowing", action="store_true")
    args = parser.parse_args()
    cli, product = args.cli.resolve(strict=True), args.product.resolve(strict=True)
    root = args.scratch.resolve()
    root.mkdir(mode=0o700)  # Exclusive fixture: never overwrite user data.
    environment = dict(os.environ)
    for key, name in [("GRAPT_TEST_HOME", "home"), ("GRAPT_CONFIG_DIR", "global-config")]:
        directory = root / name
        directory.mkdir()
        environment[key] = str(directory)
    profile, repository = root / "profile", root / "repository"
    repository.mkdir()
    subdir = repository / "src"
    subdir.mkdir()
    (subdir / "note.txt").write_text("caller directory witness\n", encoding="utf-8")
    (repository / "grapt.jsonc").write_text('{"permission_preset":"inspect"}\n', encoding="utf-8")
    git(repository, "init", "--quiet")
    git(repository, "add", "--", "src/note.txt", "grapt.jsonc")
    git(repository, "-c", "user.name=Witness", "-c", "user.email=witness@invalid", "commit", "--quiet", "-m", "fixture root")
    expected_project = git(repository, "rev-parse", "HEAD")
    # Fixture preparation writes/syncs a fresh store before the measured journey.
    # Observed SATA fsync delays exceed 30s under unrelated store tests.
    stack.enter_context(ScriptedProfile(product, profile, environment,
        [{"tool":"read", "arguments":{"path":"note.txt"}}]))
    # A distinct profile project proves this is not merely a global identity.
    (profile / "profile-note.txt").write_text("service root witness\n", encoding="utf-8")
    git(profile, "init", "--quiet")
    git(profile, "add", "--", "profile-note.txt")
    git(profile, "-c", "user.name=Witness", "-c", "user.email=witness@invalid", "commit", "--quiet", "-m", "different fixture root")
    child = None
    try:
        with (root / "product.stdout").open("wb") as stdout, (root / "product.stderr").open("wb") as stderr:
            child = subprocess.Popen([str(product), "serve-terminal", "--service", str(profile), "--port", "0"],
                                     cwd=root, env=environment, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr)
            endpoint = None
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError("actual product exited before registration: " + (root / "product.stderr").read_text()[:4096])
                try:
                    record = (profile / "registration").read_text(encoding="utf-8")
                    values = dict(line.split("=", 1) for line in record.splitlines() if "=" in line)
                    endpoint = values.get("endpoint")
                except FileNotFoundError:
                    pass
                if endpoint:
                    try:
                        ready = json.loads((profile / "serve-ready.json").read_text(encoding="utf-8"))
                        if ready.get("observed_endpoint") == endpoint:
                            break
                    except FileNotFoundError:
                        pass
                endpoint = None
                time.sleep(0.05)
            if not endpoint:
                raise RuntimeError("actual product did not publish matching readiness within 15 seconds")
            credential = (profile / "basic.credential").read_text(encoding="utf-8")
            arguments = [str(cli), "run", "--service", str(profile), "--new", "--json", "read note.txt"]
            try:
                run = subprocess.run(arguments, cwd=subdir, env=environment,
                                     capture_output=True, text=True, timeout=30)
            except subprocess.TimeoutExpired as error:
                # subprocess.run kills and reaps the CLI. Preserve partial output,
                # then inspect the created session before diagnosing the timeout:
                # the omitted location is observable even when the wrong root stalls.
                output = lambda value: value.decode(errors="replace") if isinstance(value, bytes) else value or ""
                run = subprocess.CompletedProcess(arguments, -1, output(error.stdout), output(error.stderr))
            (root / "run.stdout").write_text(run.stdout, encoding="utf-8")
            (root / "run.stderr").write_text(run.stderr, encoding="utf-8")
            status, sessions = request(endpoint, credential, "/api/session")
            if status != 200 or len(sessions.get("data", [])) != 1:
                raise RuntimeError("actual run did not create exactly one session")
            session = sessions["data"][0]
            actual_directory = session.get("location", {}).get("directory")
            if actual_directory != str(subdir.resolve()):
                raise RuntimeError(f"RUN-CWD location: expected caller subdir {subdir.resolve()}, got {actual_directory}")
            if session.get("projectID") != expected_project:
                raise RuntimeError("RUN-CWD project: session was not assigned the caller repository's root identity")
            if run.returncode:
                raise RuntimeError(f"actual provider run failed ({run.returncode}): {run.stderr[:4096]}")
            status, messages = request(endpoint, credential, f'/api/session/{session["id"]}/message?order=asc')
            (root / "messages.json").write_text(json.dumps(messages, indent=2) + "\n", encoding="utf-8")
            reads = [part for message in messages.get("data", []) for part in message.get("content", [])
                     if part.get("type") == "tool" and part.get("name") == "read"]
            if status != 200 or len(reads) != 1 or reads[0].get("state", {}).get("status") != "completed":
                raise RuntimeError("RUN-CWD effect: actual read of the caller's file did not complete")
            if reads[0].get("state", {}).get("input", {}).get("path") != "note.txt":
                raise RuntimeError("RUN-CWD effect: actual read did not use the relative fixture path")
            preset_status = None  # Slice A supplies this route on the Train 27 composition.
            if args.require_narrowing:
                preset_status, preset = request(endpoint, credential, f'/api/session/{session["id"]}/preset', {"preset":"maintainer"})
                if preset_status not in (403, 409):
                    raise RuntimeError(f"RUN-CWD authority: expected a narrowing refusal, got {preset_status}")
                diagnostic = str(preset.get("message", "")).lower()
                if not any(word in diagnostic for word in ("narrow", "widen", "inspect")):
                    raise RuntimeError("RUN-CWD authority: refusal did not name the caller project's preset floor")
            receipt = {"passed":True, "directory":actual_directory, "project_id":session["projectID"],
                       "session_id":session["id"], "run_exit":run.returncode, "read_completed":True, "preset_status":preset_status,
                       "narrowing_required":args.require_narrowing, "external_provider_calls":0}
            child.stdin.close()
            code = child.wait(timeout=15)
            if code:
                raise RuntimeError(f"actual product did not drain successfully: {code}")
            receipt["product_exit_before_cleanup"] = code
            (root / "witness.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
            print(json.dumps(receipt))
    finally:
        if child is not None and child.poll() is None:
            if child.stdin is not None and not child.stdin.closed:
                child.stdin.close()
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)


if __name__ == "__main__":
    main()
