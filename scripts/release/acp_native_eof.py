#!/usr/bin/env python3
"""Witness Windows control-pipe EOF before any fixture termination."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--product", type=Path, required=True)
    parser.add_argument("--product-sha256", required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "win32":
        raise RuntimeError("this witness requires native Windows execution")
    product = args.product.resolve(strict=True)
    with product.open("rb") as source:
        if hashlib.file_digest(source, "sha256").hexdigest() != args.product_sha256:
            raise RuntimeError("product artifact differs from the cross-build receipt")
    service = args.scratch / "acp-control-eof"
    environment = dict(os.environ)
    for key, name in [("GRAPT_TEST_HOME", "home"), ("GRAPT_CONFIG_DIR", "global-config")]:
        directory = args.scratch / name
        directory.mkdir()
        environment[key] = str(directory)
    initialized = subprocess.run(
        [str(product), "init-profile", "--service", str(service)],
        env=environment, capture_output=True, timeout=15, check=True,
    )
    receipt = json.loads(initialized.stdout)
    if receipt.get("outcome") != "setup-required":
        raise RuntimeError("native fixture did not initialize an empty profile")
    subprocess.run(["git", "init", "--quiet", str(service)], timeout=5, check=True)
    child = None
    report = {"platform": sys.platform, "product_sha256": args.product_sha256, "passed": False}
    try:
        with (args.output / "product-stdout.log").open("wb") as stdout, (args.output / "product-stderr.log").open("wb") as stderr:
            child = subprocess.Popen(
                [str(product), "serve-terminal", "--service", str(service), "--port", "0"],
                env=environment, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr,
            )
            report["pid"] = child.pid
            ready_path = service / "serve-ready.json"
            ready = None
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError(f"product exited before readiness: {child.returncode}")
                try:
                    with ready_path.open("rb") as source:
                        data = source.read(8193)
                    if len(data) > 8192:
                        raise RuntimeError("readiness exceeded its byte bound")
                    ready = json.loads(data)
                    if ready.get("pid") == child.pid:
                        break
                except FileNotFoundError:
                    pass
                except json.JSONDecodeError:
                    pass
                time.sleep(0.01)
            else:
                raise RuntimeError("product did not publish matching readiness")
            if child.poll() is not None:
                raise RuntimeError("product exited before control EOF")
            child.stdin.close()
            code = child.wait(timeout=10)
            report["exit_code_before_fixture_cleanup"] = code
            if code != 0:
                raise RuntimeError(f"control EOF produced a failed exit: {code}")
        for name in ["product-stdout.log", "product-stderr.log"]:
            if (args.output / name).stat().st_size > 1024 * 1024:
                raise RuntimeError(f"{name} exceeded its diagnostic byte bound")
        with product.open("rb") as source:
            if hashlib.file_digest(source, "sha256").hexdigest() != args.product_sha256:
                raise RuntimeError("product artifact changed during execution")
        report["passed"] = True
    finally:
        # This backstop can never supply the successful exit witness above.
        if child is not None and child.poll() is None:
            report["fixture_forced_cleanup"] = True
            child.kill()
            child.wait(timeout=5)
        (args.output / "witness.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report))


if __name__ == "__main__":
    main()
