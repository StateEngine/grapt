#!/usr/bin/env python3
"""Linux UPR native prefix exclusion: parent death, competing updates and start race."""

import argparse
import ctypes
import fcntl
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import time

from update_journey import command, identity, installation
from gate import stop_group


def wait_for(predicate, seconds, diagnostic):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.01)
    raise RuntimeError(diagnostic)


def stopped(child, predicate, diagnostic):
    def observe():
        if child.poll() is not None:
            raise RuntimeError(diagnostic + ": process exited before observation")
        return predicate()
    wait_for(observe, 15, diagnostic)
    child.send_signal(signal.SIGSTOP)
    wait_for(lambda: "T" in Path(f"/proc/{child.pid}/status").read_text().split("State:", 1)[1].splitlines()[0],
             1, diagnostic + ": process did not stop")


def busy(cli, package, prefix, root, environment, label):
    before = installation(prefix)
    result = command([cli, "update", "--from", package, "--prefix", prefix], root, environment, label, success=False)
    if result.returncode != 2 or "installation is in use" not in result.stderr:
        raise RuntimeError(f"UPR {label}: a live prefix holder did not exclude update")
    if installation(prefix) != before:
        raise RuntimeError(f"UPR {label}: busy refusal changed the installation")


def hold_observed(prefix):
    # Test tooling probes the same kernel lock; this is not service liveness.
    with (prefix / "lock").open("rb") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return False
        except BlockingIOError:
            return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-a", required=True, type=Path)
    parser.add_argument("--package-b", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--product-lifetime-mutant", type=Path,
                        help="compiled diagnostic product; replaces only the scratch installed product")
    args = parser.parse_args()
    if sys.platform != "linux" or not hasattr(signal, "pidfd_send_signal"):
        parser.error("this witness requires Linux pidfds")
    packages = [args.package_a.resolve(strict=True), args.package_b.resolve(strict=True)]
    a, b = [identity(package) for package in packages]
    if a == b:
        raise RuntimeError("UPR requires two genuine distinct packages")
    # Reap our exact service after its CLI dies, instead of leaving an orphan.
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "fixture subreaper could not be installed")
    root = args.scratch.resolve()
    root.mkdir(mode=0o700)
    environment = dict(os.environ)
    for key, name in [("GRAPT_TEST_HOME", "home"), ("GRAPT_CONFIG_DIR", "configuration")]:
        directory = root / name
        directory.mkdir(mode=0o700)
        environment[key] = str(directory)
    prefix, profile = root / "installation", root / "other-profile"
    cli = packages[1] / "grapt"
    command([cli, "update", "--from", packages[0], "--prefix", prefix], root, environment, "install-A", timeout=120)
    command([prefix / "current/bin/grapt-server", "init-profile", "--service", profile], root,
            environment, "profile-init", timeout=90)
    # Stage both real versions before timing a start's fixed five-second wait.
    # Package copying is a separate witness; this race measures activation.
    command([cli, "update", "--from", packages[1], "--prefix", prefix], root,
            environment, "prestage-B", timeout=120)
    command([cli, "revert", "--prefix", prefix, "--profile", profile], root,
            environment, "before-race-A")
    rows = []

    # Start racing an update, and a second updater, while the first owns the lock.
    updater = starter = None
    try:
        with (root / "updater.stdout").open("xb") as out, (root / "updater.stderr").open("xb") as err:
            updater = subprocess.Popen([str(cli), "update", "--from", str(packages[1]), "--prefix", str(prefix)],
                                       cwd=root, env=environment, stdout=out, stderr=err, start_new_session=True)
            stopped(updater, lambda: hold_observed(prefix), "UPR updater lock was not observed")
            if os.readlink(prefix / "current") != f"versions/{a}":
                raise RuntimeError("UPR race barrier missed the pre-activation boundary")
            busy(cli, packages[1], prefix, root, environment, "competing-updater")
            with (root / "starter.stdout").open("xb") as start_out, (root / "starter.stderr").open("xb") as start_err:
                starter = subprocess.Popen([str(prefix / "current/bin/grapt"), "--version"],
                                          cwd=root, env=environment, stdout=start_out, stderr=start_err, start_new_session=True)
                time.sleep(0.2)  # Within the product's fixed five-second wait bound.
                if starter.poll() is not None or (root / "starter.stdout").stat().st_size:
                    raise RuntimeError("UPR start did not wait for the active transition")
                updater.send_signal(signal.SIGCONT)
                if updater.wait(timeout=120) or starter.wait(timeout=10):
                    raise RuntimeError("UPR activation race failed to finish")
            version = (root / "starter.stdout").read_text()
            if b not in version or os.readlink(prefix / "current") != f"versions/{b}":
                raise RuntimeError("UPR start did not select the complete new version")
            rows.append({"case": "activation-race", "selected": b, "second_updater_refused": True})
    finally:
        for child in (starter, updater):
            if child is not None and child.poll() is None:
                stop_group(child)

    if args.product_lifetime_mutant is not None:
        mutant = args.product_lifetime_mutant.resolve(strict=True)
        if not mutant.is_file() or mutant.stat().st_size > 512 * 1024 * 1024:
            raise RuntimeError("UPR product mutant must be a bounded compiled regular file")
        # Deliberate diagnostic corruption of this scratch installation only.
        # Its original package manifest is retained, never rehashed or relabelled.
        shutil.copy2(mutant, prefix / "current/bin/grapt-server")
        (root / "product-mutant-input.json").write_text(json.dumps({
            "artifact": str(mutant), "class": "diagnostic mutant; installed digest intentionally invalid"}, indent=2))

    # Stop the real CLI once its exact product child is observed. Readiness must
    # then be published by that product while its parent cannot make progress.
    parent, product_pid, product_fd = None, None, None
    try:
        with (root / "parent.stdout").open("xb") as out, (root / "parent.stderr").open("xb") as err:
            product = prefix / "current/bin/grapt-server"
            parent = subprocess.Popen([str(prefix / "current/bin/grapt"), "service", "start", "--service",
                                       str(profile), "--product", str(product)], cwd=root, env=environment,
                                      stdout=out, stderr=err)
            def own_child():
                nonlocal product_pid, product_fd
                for value in Path(f"/proc/{parent.pid}/task/{parent.pid}/children").read_text().split():
                    pid = int(value)
                    try:
                        if Path(f"/proc/{pid}/exe").resolve(strict=True) == product.resolve(strict=True):
                            product_fd = os.pidfd_open(pid)
                            product_pid = pid
                            return True
                    except FileNotFoundError:
                        pass
                return False
            stopped(parent, own_child, "UPR exact product child was not observed")
            def ready():
                if select.select([product_fd], [], [], 0)[0]:
                    raise RuntimeError("UPR product exited while its parent was stopped")
                try:
                    values = dict(line.split("=", 1) for line in (profile / "registration").read_text().splitlines() if "=" in line)
                    observed = json.loads((profile / "serve-ready.json").read_text())
                    return observed.get("observed_endpoint") == values.get("endpoint")
                except FileNotFoundError:
                    return False
            wait_for(ready, 15, "UPR child did not independently publish readiness")
            busy(cli, packages[0], prefix, root, environment, "before-parent-loss")
            parent.kill()
            if parent.wait(timeout=5) != -signal.SIGKILL:
                raise RuntimeError("UPR parent did not die from the injected SIGKILL")
            if select.select([product_fd], [], [], 0)[0]:
                raise RuntimeError("UPR product did not survive CLI-parent death")
            busy(cli, packages[0], prefix, root, environment, "after-parent-loss")
            signal.pidfd_send_signal(product_fd, signal.SIGTERM)
            wait_for(lambda: select.select([product_fd], [], [], 0)[0], 15, "UPR product did not stop")
            os.waitpid(product_pid, 0)
            product_pid = None
            command([cli, "update", "--from", packages[0], "--prefix", prefix], root,
                    environment, "after-product-death", timeout=120)
            if identity(prefix / "current/bin") != a:
                raise RuntimeError("UPR product death did not release update exclusion")
            rows.append({"case": "parent-and-product-death", "parent_sigkill": True,
                         "product_survived_parent": True, "update_refused_after_parent": True,
                         "update_allowed_after_product": True})
    finally:
        if parent is not None and parent.poll() is None:
            parent.kill()
            parent.wait(timeout=5)
        if product_fd is not None:
            if not select.select([product_fd], [], [], 0)[0]:
                signal.pidfd_send_signal(product_fd, signal.SIGKILL)
                wait_for(lambda: select.select([product_fd], [], [], 0)[0], 5, "UPR cleanup product did not stop")
            if product_pid is not None:
                os.waitpid(product_pid, 0)
            os.close(product_fd)
    (root / "witness.json").write_text(json.dumps({"passed": True, "package_a": a,
        "package_b": b, "rows": rows, "external_provider_calls": 0}, indent=2) + "\n")
    print("UPR actual prefix liveness and activation race passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"UPR native liveness failed: {error}", file=sys.stderr)
        raise SystemExit(1)
