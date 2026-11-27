"""Drive the reconnect comparison through a relay that can cut the connection.

Both clients find a product the same way: the readiness receipt in a service
root gives the endpoint, and `basic.credential` gives the credential. That is
the whole contract between a client and a running product, so a service root
holding those two files and naming a DIFFERENT address is a legitimate way to
put something in the path — no client is deceived about anything it can check,
and the product's own root is never touched.

The relay forwards bytes to the real product and, on command, drops every
socket it holds. That is the reconnect condition: the connection breaks while
the server stays exactly where it was.

Why not restart the product instead: it admits only an ephemeral port
(`IA0-1R admits only --port 0`), so it necessarily returns at a different
address. Neither client is built to follow an endpoint change, and testing that
would be testing configuration rather than recovery.

Usage:
    python reconnect-run.py --service-root <root> --native <exe>
                            --ts-root <oracle-root> --evidence <fresh dir>
"""

from __future__ import annotations

import argparse
import json
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent

# Bounded because it goes into the exit message, and kept from BOTH ENDS -- see
# `bounded`.
DIAGNOSTIC_BYTES = 64 * 1024


def bounded(text: str) -> str:
    """Keep BOTH ENDS of an over-long diagnostic, never just the tail.

    Tail-only truncation was wrong and a witness caught it: a child that states
    its reason and THEN floods -- which is what a refusing observer at startup
    does -- had that reason cut off, so the caller raised a screenful of noise
    as the reason a run stopped. The LAST thing written is usually why a process
    stopped; the FIRST thing is why it never started. Both are kept.
    """
    if len(text) <= DIAGNOSTIC_BYTES:
        return text
    half = DIAGNOSTIC_BYTES // 2
    elided = len(text) - 2 * half
    return f"{text[:half]}" + f"\n... [{elided} characters elided] ...\n" + f"{text[-half:]}"


def child_diagnostic(directory: Path, stem: str) -> str:
    """What a child printed, read from the files it was given.

    The relay's streams are FILES so nothing blocks on an undrained pipe;
    `communicate()` then returns `(None, None)`, so this reads from disk.
    Absent, unreadable and empty are reported as themselves rather than as an
    empty string.
    """
    parts = []
    for stream in ("stdout", "stderr"):
        path = directory / f"{stem}-{stream}.log"
        if not path.exists():
            parts.append(f"[{path.name}] <absent>")
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError as error:
            parts.append(f"[{path.name}] <unreadable: {error.strerror or error}>")
            continue
        parts.append(f"[{path.name}] <empty>" if not text else f"[{path.name}]\n{bounded(text)}")
    return "\n".join(parts)


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service-root", required=True, type=Path)
    parser.add_argument("--native", required=True, type=Path)
    parser.add_argument("--ts-root", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument(
        "--scenario",
        type=Path,
        default=HERE / "scenarios" / "j04-reconnect.json",
    )
    arguments = parser.parse_args()

    receipt = json.loads(
        (arguments.service_root / "serve-ready.json").read_text(encoding="utf-8")
    )
    product = str(receipt["observed_endpoint"])
    product_port = int(product.rsplit(":", 1)[1])

    relay_port = free_port()
    control = arguments.evidence.parent / f"relay-{relay_port}.control"
    control.parent.mkdir(parents=True, exist_ok=True)
    for stale in (control, control.with_suffix(".cut"), control.with_suffix(".stop")):
        stale.unlink(missing_ok=True)

    # FILES, NOT UNDRAINED PIPES. The relay proxies the whole reconnect journey
    # and nothing reads its streams, so a pipe would fill and block it — and a
    # blocked relay is a CUT the scenario did not ask for, which would be read
    # as the client failing to reconnect.
    relay_out = (control.parent / f"relay-{relay_port}-stdout.log").open("w", encoding="utf-8")
    relay_err = (control.parent / f"relay-{relay_port}-stderr.log").open("w", encoding="utf-8")
    relay = subprocess.Popen(
        [
            sys.executable,
            str(HERE / "cut-proxy.py"),
            "--listen",
            f"127.0.0.1:{relay_port}",
            "--target",
            f"127.0.0.1:{product_port}",
            "--control",
            str(control),
        ],
        stdout=relay_out,
        stderr=relay_err,
        text=True,
    )
    # The parent's copies close at once; the relay holds its own.
    relay_out.close()
    relay_err.close()
    for _ in range(200):
        if control.exists():
            break
        if relay.poll() is not None:
            relay.communicate()
            raise SystemExit(
                "the relay exited before listening:\n"
                + child_diagnostic(control.parent, f"relay-{relay_port}")
            )
        time.sleep(0.05)
    else:
        relay.kill()
        raise SystemExit("the relay never announced itself")

    # A service root that names the relay. Only the two files a client reads.
    view = arguments.evidence.parent / f"service-view-{relay_port}"
    view.mkdir(parents=True, exist_ok=True)
    receipt["observed_endpoint"] = f"http://127.0.0.1:{relay_port}"
    (view / "serve-ready.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    shutil.copy2(arguments.service_root / "basic.credential", view / "basic.credential")

    scenario = json.loads(arguments.scenario.read_text(encoding="utf-8"))
    for step in scenario["steps"]:
        if "cut" in step:
            step["cut"] = str(control)
    pinned = arguments.evidence.parent / f"scenario-reconnect-{relay_port}.json"
    pinned.write_text(json.dumps(scenario, indent=2, ensure_ascii=False), encoding="utf-8")

    try:
        outcome = subprocess.run(
            [
                sys.executable,
                str(HERE / "compare.py"),
                "--scenario",
                str(pinned),
                "--native",
                str(arguments.native),
                "--service-root",
                str(view),
                # The clients are pointed at the RELAY view, which holds only
                # the two files a client reads. The durable store is in the
                # real service root, and the session binding has to be read
                # there or it is not read at all.
                "--store-root",
                str(arguments.service_root),
                "--ts-root",
                str(arguments.ts_root),
                "--evidence",
                str(arguments.evidence),
            ],
            text=True,
        )
    finally:
        control.with_suffix(".stop").write_text("stop", encoding="utf-8")
        try:
            relay.wait(timeout=10)
        except subprocess.TimeoutExpired:
            relay.kill()
    return outcome.returncode


if __name__ == "__main__":
    sys.exit(main())
