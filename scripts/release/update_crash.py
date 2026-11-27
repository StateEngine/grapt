#!/usr/bin/env python3
"""Native UPR crash/recovery and stat-to-open FIFO race, with diagnostic barriers."""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys

from update_journey import MARKER, command, identity, installation, snapshot
from update_liveness import wait_for
from gate import stop_group


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-a", required=True, type=Path)
    parser.add_argument("--package-b", required=True, type=Path)
    parser.add_argument("--diagnostic-cli", required=True, type=Path)
    parser.add_argument("--refusal-profile", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--phase", action="append", choices=["intent", "pointer", "state", "copy", "open"])
    args = parser.parse_args()
    packages = [args.package_a.resolve(strict=True), args.package_b.resolve(strict=True)]
    a, b = [identity(package) for package in packages]
    if a == b:
        raise RuntimeError("UPR requires distinct real packages")
    diagnostic = args.diagnostic_cli.resolve(strict=True)
    refused_profile = args.refusal_profile.resolve(strict=True)
    if (refused_profile / "upr-fixture-marker").read_bytes() != MARKER:
        raise RuntimeError("UPR refusal profile must be the disposable forward-migrated fixture")
    store_before = snapshot(refused_profile)
    root = args.scratch.resolve()
    root.mkdir(mode=0o700)
    environment = dict(os.environ)
    for key, name in [("GRAPT_TEST_HOME", "home"), ("GRAPT_CONFIG_DIR", "configuration")]:
        directory = root / name
        directory.mkdir(mode=0o700)
        environment[key] = str(directory)
    cli = packages[1] / "grapt"
    rows = []
    for phase in args.phase or ["intent", "pointer", "state", "copy", "open"]:
        case = root / phase
        case.mkdir(mode=0o700)
        barrier = case / "barrier"
        barrier.mkdir(mode=0o700)
        prefix = case / "installation"
        command([cli, "update", "--from", packages[0], "--prefix", prefix], case, environment, "install-A", timeout=120)
        if phase != "copy":
            command([cli, "update", "--from", packages[1], "--prefix", prefix], case, environment, "prestage-B", timeout=120)
            # An absent scratch profile is a valid read-only check, not a live profile.
            command([cli, "revert", "--prefix", prefix, "--profile", case / "absent-profile"],
                    case, environment, "before-crash-A")
        source = packages[1]
        if phase == "open":
            source = case / "package"
            # Hard links avoid duplicating the large immutable package. The fault
            # replaces a directory entry only and never writes a linked file.
            shutil.copytree(packages[1], source, copy_function=os.link)
        before = installation(prefix)
        diagnostic_environment = dict(environment, GRAPT_UPR_DIAGNOSTIC_PHASE=phase,
                                      GRAPT_UPR_DIAGNOSTIC_ROOT=str(barrier),
                                      GRAPT_UPR_DIAGNOSTIC_INPUT=str(source / "grapt"))
        child = None
        try:
            with (case / "diagnostic.stdout").open("xb") as out, (case / "diagnostic.stderr").open("xb") as err:
                child = subprocess.Popen([str(diagnostic), "update", "--from", str(source), "--prefix", str(prefix)],
                                         cwd=case, env=diagnostic_environment, stdout=out, stderr=err, start_new_session=True)
                def observed():
                    if child.poll() is not None:
                        raise RuntimeError(f"UPR {phase}: updater exited before the diagnostic boundary")
                    try:
                        return (barrier / "phase").read_text() == phase
                    except FileNotFoundError:
                        return False
                wait_for(observed, 120, f"UPR {phase}: diagnostic boundary was not observed")
                if phase == "open":
                    (source / "grapt").rename(source / "grapt.original")
                    os.mkfifo(source / "grapt", mode=0o600)
                    (barrier / "release").touch(exist_ok=False)
                    try:
                        code = child.wait(timeout=2)
                    except subprocess.TimeoutExpired as error:
                        stop_group(child)
                        raise RuntimeError("UPR FIFO replacement blocked the updater") from error
                    refusal = (case / "diagnostic.stderr").read_text()
                    if code != 2 or "input changed while it was opened" not in refusal:
                        raise RuntimeError("UPR replaced FIFO did not receive its input-race refusal")
                    if installation(prefix) != before:
                        raise RuntimeError("UPR FIFO replacement changed the active installation")
                    rows.append({"phase": phase, "refused_without_blocking": True, "installation_unchanged": True})
                    continue
                child.kill()
                if child.wait(timeout=5) != -signal.SIGKILL:
                    raise RuntimeError(f"UPR {phase}: updater was not killed at its observed boundary")
            if phase == "copy":
                partial = prefix / "versions/.partial/bin/grapt-server"
                if not partial.is_file() or partial.stat().st_size == 0 or (prefix / "versions" / b).exists():
                    raise RuntimeError("UPR interrupted copy did not leave an inactive partial")
                if installation(prefix) != before:
                    raise RuntimeError("UPR interrupted copy changed the active installation")
                command([cli, "update", "--from", packages[1], "--prefix", prefix], case,
                        environment, "retry-copy", timeout=120)
                if identity(prefix / "current/bin") != b or (prefix / "versions/.partial").exists():
                    raise RuntimeError("UPR interrupted copy did not retry cleanly")
                rows.append({"phase": phase, "active_unchanged_after_kill": True, "retry_selected": b})
                continue
            command([cli, "revert", "--prefix", prefix, "--profile", refused_profile], case,
                    environment, "recover-before-refusal", success=False)
            expected_current, expected_previous = (a, b) if phase == "intent" else (b, a)
            state = json.loads((prefix / "state.json").read_text())
            if state["current"] != expected_current or state["previous"] != expected_previous:
                raise RuntimeError(f"UPR {phase}: recovery lost the actual predecessor")
            if os.readlink(prefix / "current") != f"versions/{expected_current}" or (prefix / "transition.json").exists():
                raise RuntimeError(f"UPR {phase}: recovery left inconsistent pointer/intent")
            recovered = installation(prefix)
            second = command([cli, "revert", "--prefix", prefix, "--profile", refused_profile], case,
                             environment, "second-recovery-refusal", success=False)
            if second.returncode != 2 or "revert refused" not in second.stderr:
                raise RuntimeError(f"UPR {phase}: the forward profile did not prevent another effect")
            if installation(prefix) != recovered or snapshot(refused_profile) != store_before:
                raise RuntimeError(f"UPR {phase}: second recovery changed durable state")
            rows.append({"phase": phase, "killed_at_observed_boundary": True, "current": expected_current,
                         "previous": expected_previous, "second_recovery_idempotent": True})
        finally:
            if child is not None and child.poll() is None:
                stop_group(child)
    (root / "witness.json").write_text(json.dumps({"passed": True, "package_a": a, "package_b": b,
        "diagnostic_cli": str(diagnostic), "rows": rows, "external_provider_calls": 0}, indent=2) + "\n")
    print("UPR observed crash boundaries, inactive copy and FIFO race passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"UPR native crash fixture failed: {error}", file=sys.stderr)
        raise SystemExit(1)
