#!/usr/bin/env python3
"""One ordinary Linux producer fixture: background tools, auto-compaction and restart.

Run through the normal heavy scheduler. Uses a fresh disposable profile, verified
ordinary package and local TLS/MCP peers in a private network/PID namespace.
This synthetic journey does not measure a real producer seat-day.
"""
import argparse
import json
from pathlib import Path
import shutil
import sqlite3
import ssl
import subprocess
import sys

from acceptance_peer import CAPABILITY, PRIVATE_CURSOR, Server
from headless_acceptance import PACKET, SESSION, require, wait, write
from ordinary_profile import OrdinaryProfile, open_terminal
from sustained_provider import SustainedModel, TASK, QUEUED, RESTART
from tui_acceptance import Terminal, command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]


def committed(journey):
    path = journey.profile / "database.sqlite3"
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=5) as connection:
        return [(kind, json.loads(data)) for kind, data in connection.execute(
            "SELECT type,data FROM event WHERE aggregate_id=? ORDER BY seq", (SESSION,))]


def background(journey, text, name):
    result = journey.cli(["run", "--service", str(journey.profile), "--session", SESSION,
                          "--background", "--json", text], name)
    require(result.returncode == 0, name + " CLI admission failed")
    receipt = json.loads(result.stdout)
    require(receipt.get("session") == SESSION and receipt.get("message"), "admission lost its exact Session/input")
    return receipt


def idle(journey):
    wait(lambda: SESSION not in journey.api("GET", "/api/session/active")["data"], "producer Session idle", seconds=40)


def run(args):
    sys.path.insert(0, str(ROOT / "scripts/release"))
    import exercise_package
    manifest, _ = exercise_package.install(args.package, args.scratch / "install")
    require(manifest["engine_version"] == args.source and manifest["profile"] == "release", "wrong package source/profile")
    require(set(manifest["binaries"]) == {"grapt", "grapt-server", "grapt-tui", "grapt-engined", "grapt-migrate"}, "incomplete ordinary package")
    args.cli, args.product, args.tui = [args.scratch / "install" / name for name in ("grapt", "grapt-server", "grapt-tui")]
    require("testkit/fixture" not in command([args.product, "--version"]), "testkit product cannot witness ordinary reliability")
    model = SustainedModel()
    provider = Server(0, model.dispatch)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.scratch / "tls/server.crt", args.scratch / "tls/server.key")
    provider.socket = context.wrap_socket(provider.socket, server_side=True)
    journey, terminal = None, None
    result = {"passed": False, "package_source": args.source, "ordinary_package": True,
              "real_seat_day": "not measured", "paid_calls": 0, "windows": "not measured"}
    mail = TuiMail()
    with provider, Server(4877, mail.dispatch, with_connection=True) as mail_peer:
        try:
            journey = OrdinaryProfile(args, f"https://127.0.0.1:{provider.server_port}")
            (journey.profile / "calc.py").write_text("def add(a, b):\n    return abs(a) + abs(b)\n")
            (journey.profile / "test_calc.py").write_text(
                "import unittest\nimport calc\nclass Calc(unittest.TestCase):\n"
                "    def test_signed(self): self.assertEqual(calc.add(-2, 5), 3)\n"
                "    def test_zero(self): self.assertEqual(calc.add(0, 0), 0)\n")
            for n in range(8):
                (journey.profile / f"evidence-{n}.txt").write_text(
                    f"evidence-{n}: signed addition task\n" + ("x" * 79 + "\n") * 450)
            journey.start(0)
            require(journey.provision().returncode == 0, "synthetic provision failed")
            terminal = Terminal(args, journey)
            open_terminal(terminal, 0)
            first = background(journey, TASK, "initial-background")
            terminal.expect("SUSTAIN_TOOL_BEGIN")
            terminal.capture("background-tool-output")
            wait(lambda: model.summary_entered.is_set(), "automatic compaction summary", seconds=45)
            terminal.capture("automatic-compaction")
            require(SESSION in journey.api("GET", "/api/session/active")["data"], "summary is not holding the producer")
            queued = background(journey, QUEUED, "input-during-compaction")
            require(first["message"] != queued["message"], "queued input reused the original identity")
            model.summary_release.set()
            terminal.expect("SUSTAIN_QUEUED_DONE")
            idle(journey)
            terminal.capture("continued-after-compaction")
            require((journey.profile / "worklog.txt").read_text() == "addition verified; queued input completed once\n", "queued work did not complete")
            before = journey.history()
            require("SUSTAIN_INITIAL_DONE" in json.dumps(before), "original producer work never completed")
            rows = committed(journey)
            compactions = [value for kind, value in rows if kind == "session.compaction.ended.2"]
            require(compactions and all(value.get("trigger") == "policyThreshold" for value in compactions),
                    "no committed automatic policy compaction")
            terminal.close()
            journey.stop()
            journey.start(1)
            require(journey.history() == before, "restart changed durable history before further work")
            journey.packet.write_bytes(PACKET)
            journey.api("PUT", f"/api/session/{SESSION}/stateengine/capability", {"capability": CAPABILITY})
            open_terminal(terminal, 1)
            resumed = background(journey, RESTART, "continue-after-restart")
            terminal.expect("SUSTAIN_RESTART_DONE")
            idle(journey)
            terminal.capture("useful-work-after-restart")
            require("return a + b" in (journey.profile / "calc.py").read_text(), "restart lost the addition fix")
            require("return a - b" in (journey.profile / "calc.py").read_text(), "further subtraction work missing")
            terminal.close()
            journey.stop()
            rows = committed(journey)
            admitted = [value["inputID"] for kind, value in rows if kind == "session.input.admitted.1"]
            require(sorted(admitted) == sorted(x["message"] for x in (first, queued, resumed)), "lost or duplicated original/queued/restart input")
            require(not any(kind in ("session.execution.failed.1", "session.execution.interrupted.1") for kind, _ in rows), "recorded producer failure or interruption")
            require(model.stage == 11 and len(model.handles) == 2 and not provider.errors and not mail_peer.errors,
                    "provider sequence or background tool lifetime failed")
            serialized = json.dumps(model.bodies)
            require(CAPABILITY not in serialized and PRIVATE_CURSOR not in serialized, "private handles reached provider")
            result.update(passed=True, session=SESSION, automatic_compactions=len(compactions),
                summary_requests=model.summaries, physical_requests=model.count(), queued_input_preserved=True,
                background_tool_output=True, continued_after_restart=True, useful_tests_passed=True)
        finally:
            model.summary_release.set()
            if terminal:
                if terminal.session:
                    try: terminal.capture("failure")
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                if journey.child and journey.child.poll() is None:
                    try: journey.stop()
                    except (OSError, RuntimeError, subprocess.TimeoutExpired): pass
                journey.cleanup()
                write(journey.root / "model-requests.json", model.bodies)
                write(journey.root / "observations.json", {"actions": model.observed, "errors": provider.errors,
                    "mail_errors": mail_peer.errors, "handles": model.handles})
                write(journey.root / "events.json", committed(journey))
            write(args.scratch / "result.json", result)
    print(json.dumps(result), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--source", required=True, help="the qualified package's source commit")
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--inside", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.inside:
        return run(args)
    args.package, args.driver = args.package.resolve(strict=True), args.driver.resolve(strict=True)
    args.scratch = args.scratch.resolve()
    require(args.scratch != ROOT and ROOT not in args.scratch.parents, "scratch must be outside the source tree")
    args.scratch.mkdir(mode=0o700)
    shutil.copy2(args.driver, args.scratch / "grapt-tui-driver")
    subprocess.run([sys.executable, str(ROOT / "scripts/native-tui/fixture-ca.py"), "--root", str(args.scratch / "tls")], check=True, timeout=30)
    subprocess.run(["bwrap", "--die-with-parent", "--new-session", "--unshare-pid", "--unshare-net",
        "--ro-bind", "/", "/", "--tmpfs", "/home", "--tmpfs", "/tmp", "--tmpfs", "/var/tmp",
        "--proc", "/proc", "--dev", "/dev", "--bind", str(args.scratch), str(args.scratch),
        "--ro-bind", str(args.package), str(args.package), "--chdir", str(args.scratch),
        "/usr/bin/env", "-i", "PATH=/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE=1", f"HOME={args.scratch}/empty-home",
        sys.executable, str(Path(__file__)), "--inside", "--package", str(args.package), "--source", args.source,
        "--driver", str(args.scratch / "grapt-tui-driver"), "--scratch", str(args.scratch)], check=True, timeout=540)


if __name__ == "__main__":
    main()
