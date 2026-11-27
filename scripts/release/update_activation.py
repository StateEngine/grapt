#!/usr/bin/env python3
"""Deterministic start-versus-activation witness."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

from update_journey import command, identity, installation
from update_liveness import busy, wait_for
from gate import stop_group


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["package-a", "package-b", "diagnostic-cli", "scratch"]:
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    a, b = args.package_a.resolve(strict=True), args.package_b.resolve(strict=True)
    diagnostic = args.diagnostic_cli.resolve(strict=True)
    root = args.scratch.resolve()
    root.mkdir(mode=0o700)
    env = dict(os.environ)
    for key, name in [("GRAPT_TEST_HOME", "home"), ("GRAPT_CONFIG_DIR", "configuration")]:
        path = root / name
        path.mkdir(mode=0o700)
        env[key] = str(path)
    prefix = root / "installation"
    cli = b / "grapt"
    for package, label in [(a, "install-A"), (b, "prestage-B")]:
        command([cli, "update", "--from", package, "--prefix", prefix], root, env, label, timeout=120)
    command([cli, "revert", "--prefix", prefix, "--profile", root / "absent-profile"],
            root, env, "reset-A", timeout=120)
    before = installation(prefix)
    barrier = root / "barrier"
    barrier.mkdir(mode=0o700)
    diag_env = dict(env, GRAPT_UPR_DIAGNOSTIC_PHASE="intent", GRAPT_UPR_DIAGNOSTIC_ROOT=str(barrier))
    updater = starter = None
    try:
        with (root / "updater.stdout").open("xb") as out, (root / "updater.stderr").open("xb") as err:
            updater = subprocess.Popen([str(diagnostic), "update", "--from", str(b), "--prefix", str(prefix)],
                                       cwd=root, env=diag_env, stdout=out, stderr=err, start_new_session=True)
            def observed():
                if updater.poll() is not None:
                    raise RuntimeError("UPR activation updater exited before its intent barrier")
                return (barrier / "phase").exists() and (barrier / "phase").read_text() == "intent"
            wait_for(observed, 120, "UPR pre-publication activation barrier was not observed")
            held = installation(prefix)
            if held["pointer"] != f"versions/{identity(a)}" or any(held[key] != before[key] for key in ["state", "history"]):
                raise RuntimeError("UPR activation barrier crossed pointer publication early")
            busy(cli, b, prefix, root, env, "competing-writer")
            with (root / "starter.stdout").open("xb") as sout, (root / "starter.stderr").open("xb") as serr:
                starter = subprocess.Popen([str(prefix / "current/bin/grapt"), "--version"],
                                          cwd=root, env=env, stdout=sout, stderr=serr, start_new_session=True)
                time.sleep(0.2)
                if starter.poll() is not None or (root / "starter.stdout").stat().st_size:
                    raise RuntimeError("UPR start did not wait at the held activation boundary")
                (barrier / "release").touch(exist_ok=False)
                if updater.wait(timeout=120) or starter.wait(timeout=5):
                    raise RuntimeError("UPR activation or bounded waiting start failed")
            if identity(b) not in (root / "starter.stdout").read_text():
                raise RuntimeError("UPR waiting start did not select complete B")
            if os.readlink(prefix / "current") != f"versions/{identity(b)}" or (prefix / "transition.json").exists():
                raise RuntimeError("UPR activation left a partial transition")
    finally:
        for child in [starter, updater]:
            if child is not None and child.poll() is None:
                stop_group(child)
    (root / "witness.json").write_text(json.dumps({"passed": True, "package_a": identity(a),
        "package_b": identity(b), "diagnostic_cli": str(diagnostic),
        "barrier": "after prefix EX and intent sync, before pointer publication",
        "start_waited_without_output": True, "selected_complete_B": True,
        "second_writer_refused": True, "product_deadline_seconds": 5,
        "external_provider_calls": 0}, indent=2) + "\n")
    print("UPR deterministic activation barrier and bounded waiting start passed")


if __name__ == "__main__":
    main()
