#!/usr/bin/env python3
"""Native staging exclusion, available starts, and kernel release after SIGKILL."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

from update_journey import command, identity, installation
from update_liveness import wait_for
from gate import stop_group


def prefix_available(prefix):
    with (prefix / "lock").open("rb") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fcntl.flock(handle, fcntl.LOCK_UN)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-a", type=Path, required=True)
    parser.add_argument("--package-b", type=Path, required=True)
    parser.add_argument("--diagnostic-cli", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    args = parser.parse_args()
    packages = [args.package_a.resolve(strict=True), args.package_b.resolve(strict=True)]
    a, b = [identity(package) for package in packages]
    diagnostic = args.diagnostic_cli.resolve(strict=True)
    if a == b or sys.platform != "linux":
        raise RuntimeError("UPR requires Linux and distinct genuine packages")
    root = args.scratch.resolve()
    root.mkdir(mode=0o700)
    env = dict(os.environ)
    for key, name in [("GRAPT_TEST_HOME", "home"), ("GRAPT_CONFIG_DIR", "configuration")]:
        path = root / name
        path.mkdir(mode=0o700)
        env[key] = str(path)
    cli = packages[1] / "grapt"
    rows = []
    for phase in ["staging", "copy"]:
        case = root / phase
        case.mkdir(mode=0o700)
        prefix = case / "installation"
        barrier = case / "barrier"
        barrier.mkdir(mode=0o700)
        command([cli, "update", "--from", packages[0], "--prefix", prefix], case, env, "install-A", timeout=120)
        before = installation(prefix)
        diag_env = dict(env, GRAPT_UPR_DIAGNOSTIC_PHASE=phase,
                        GRAPT_UPR_DIAGNOSTIC_ROOT=str(barrier))
        child = None
        try:
            with (case / "writer.stdout").open("xb") as out, (case / "writer.stderr").open("xb") as err:
                child = subprocess.Popen([str(diagnostic), "update", "--from", str(packages[1]),
                                          "--prefix", str(prefix)], cwd=case, env=diag_env,
                                         stdout=out, stderr=err, start_new_session=True)
                def observed():
                    if child.poll() is not None:
                        raise RuntimeError("UPR staging writer exited before its barrier")
                    return (barrier / "phase").exists() and (barrier / "phase").read_text() == phase
                wait_for(observed, 120, "UPR staging barrier was not observed")
                with (prefix / ".staging/lock").open("rb") as handle:
                    try:
                        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    except BlockingIOError:
                        pass
                    else:
                        raise RuntimeError("UPR staging writer did not retain exclusion")
                start = command([prefix / "current/bin/grapt", "--version"], case, env,
                                "start-during-preparation", timeout=5)
                if a not in start.stdout:
                    raise RuntimeError("UPR staging start did not select complete A")
                prefix_available(prefix)
                refused = command([cli, "update", "--from", packages[1], "--prefix", prefix],
                                  case, env, "second-writer", timeout=5, success=False)
                if refused.returncode != 2 or "installation is in use" not in refused.stderr:
                    raise RuntimeError("UPR second writer did not refuse at staging")
                prefix_available(prefix)
                if installation(prefix) != before:
                    raise RuntimeError("UPR preparation or refused writer changed activation records")
                lock_identity = (prefix / ".staging/lock").stat().st_ino
                child.kill()
                if child.wait(timeout=5) != -signal.SIGKILL:
                    raise RuntimeError("UPR staging writer was not killed at its observed barrier")
            with (prefix / ".staging/lock").open("rb") as handle:
                fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(handle, fcntl.LOCK_UN)
            command([cli, "update", "--from", packages[1], "--prefix", prefix], case, env,
                    "retry-after-kill", timeout=120)
            if identity(prefix / "current/bin") != b or (prefix / "versions/.partial").exists():
                raise RuntimeError("UPR staging crash did not recover cleanly to B")
            if (prefix / ".staging/lock").stat().st_ino != lock_identity:
                raise RuntimeError("UPR retry replaced its stale lock instead of reacquiring it")
            rows.append({"phase": phase, "start_selected": a, "prefix_free_before_and_after_refusal": True,
                         "second_writer_refused": True, "SIGKILL_released_stale_lock": True,
                         "retry_selected": b})
        finally:
            if child is not None and child.poll() is None:
                stop_group(child)
    (root / "witness.json").write_text(json.dumps({"passed": True, "package_a": a,
        "package_b": b, "diagnostic_cli": str(diagnostic), "rows": rows,
        "external_provider_calls": 0}, indent=2) + "\n")
    print("UPR staging exclusion, available starts and SIGKILL recovery passed")


if __name__ == "__main__":
    main()
