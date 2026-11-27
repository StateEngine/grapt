#!/usr/bin/env python3
"""Native PTY packet/mail/compaction/restart journey with disposable peers only."""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import subprocess

from acceptance_peer import CAPABILITY, PRIVATE_CURSOR, Server
from headless_acceptance import Journey, PACKET, SESSION, digest, require, wait, write
from tui_acceptance_peer import TuiMail, TuiModel


def command(arguments, environment=None):
    result = subprocess.run([str(value) for value in arguments], env=environment,
                            capture_output=True, text=True, timeout=25)
    require(len(result.stdout) + len(result.stderr) <= 4 * 1024 * 1024, "PTY driver output bound")
    require(CAPABILITY not in result.stdout + result.stderr and PRIVATE_CURSOR not in result.stdout + result.stderr,
            "private binding reached terminal output")
    require(result.returncode == 0, "PTY command failed: " + result.stderr[-3000:])
    return result.stdout


def prepared_catalog_ready(screen):
    return ("Catalog:" not in screen and "Catalog refresh" not in screen
            and "agent agt_live_headless1" in screen and "model openai/gpt-live" in screen)


class Terminal:
    def __init__(self, args, journey):
        self.args, self.journey, self.session = args, journey, None

    def drive(self, operation, *arguments):
        return command([self.args.driver, operation, "--session", self.session, *arguments], self.journey.environment)

    def start(self, index):
        # Existing tmux servers may have ambient credentials/configuration.
        # Give the actual client an explicit empty environment and exact profile.
        environment = ["PATH=" + os.environ["PATH"], "TERM=xterm-256color",
            "GRAPT_TEST_HOME=" + self.journey.environment["GRAPT_TEST_HOME"],
            "GRAPT_CONFIG_DIR=" + self.journey.environment["GRAPT_CONFIG_DIR"]]
        self.session = command([self.args.driver, "start", "--seat", "seat-a", "--journey",
            f"row-a-{os.getpid()}-{index}", "--width", "140", "--height", "40",
            "--cwd", self.journey.profile, "--", "/usr/bin/env", "-i", *environment,
            self.args.tui, "--service", self.journey.profile, "--session", SESSION,
            "--directory", self.journey.profile], self.journey.environment).strip()
        self.expect("enter send")
        wait(lambda: prepared_catalog_ready(self.drive("capture")), "catalog and saved selections ready")
        self.capture("catalog-ready-" + str(index))

    def expect(self, text):
        return self.drive("wait", "--text", text, "--timeout-ms", "20000")

    def send(self, text):
        # The terminal deliberately treats fast unbracketed text+Enter as a
        # multiline paste. Use the actual terminal paste protocol, then submit.
        self.drive("send", "--text", "\x1b[200~" + text + "\x1b[201~")
        self.expect(text)
        self.drive("send", "Enter")

    def capture(self, name):
        screen = self.drive("capture")
        (self.journey.root / (name + ".screen.txt")).write_text(screen, encoding="utf-8")
        return screen

    def close(self):
        self.drive("send", "C-q")

        def exited():
            state = command(["tmux", "display-message", "-p", "-t", self.session,
                             "#{pane_dead} #{pane_dead_status}"]).strip()
            require(not state.startswith("1 ") or state == "1 0", "TUI exited unsuccessfully")
            return state == "1 0"

        wait(exited, "native TUI clean exit", seconds=15)
        self.drive("stop")
        self.session = None

    def cleanup(self):
        if self.session:
            self.drive("stop")
            self.session = None


def events(journey):
    path = journey.root / "tui-events.jsonl"
    if not path.exists():
        return []
    require(path.stat().st_size <= 65536, "TUI MCP evidence bound")
    return [json.loads(line) for line in path.read_text().splitlines()]


def run(args):
    model, mail = TuiModel(), TuiMail()
    result = {"passed": False, "paid_provider_calls": 0, "release_package": "not measured",
              "real_stateengine_binding": "not measured", "seat_day": "not measured",
              "prepared_launch_without_intervention": False}
    journey, terminal = None, None
    with Server(0, model.dispatch) as provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = Journey(args, "native-tui", provider.endpoint)
            config_path = journey.profile / "grapt.jsonc"
            config = json.loads(config_path.read_text())
            config["compaction"]["auto"] = False
            config["mcp"]["stateengine"]["command"] = [os.sys.executable,
                str(Path(__file__).with_name("tui_fixture_server.py")), "--calls-file",
                str(journey.root / "mcp-calls.txt"), "--tui-events", str(journey.root / "tui-events.jsonl"),
                "--busy-marker", str(journey.root / "busy-held")]
            write(config_path, config)
            (journey.profile / "fixture-note.txt").write_text("Disposable TUI workspace.\n", encoding="utf-8")
            command(["git", "-C", journey.profile, "add", "--", "fixture-note.txt"])
            committed = subprocess.run(["git", "-C", str(journey.profile), "-c", "user.name=Fixture",
                "-c", "user.email=fixture@invalid", "commit", "--quiet", "-F", "-", "--", "fixture-note.txt"],
                input="fixture root\n", capture_output=True, text=True, timeout=10)
            require(committed.returncode == 0, "fixture repository commit failed")
            journey.start(0)
            require(journey.provision().returncode == 0, "TUI fixture provision failed")
            require(model.count() == 0, "provision reached model")
            write(journey.root / "initial-catalogs.json", {name: journey.api("GET", "/api/" + name)
                for name in ("project", "agent", "model")})
            terminal = Terminal(args, journey)
            terminal.start(0)
            model.expect(("initial", ""))
            terminal.send("ROW_A_INITIAL: continue admitted scratch work.")
            terminal.expect("ROW_A_INITIAL_DONE")
            terminal.capture("initial-packet")
            require(model.count() == 1, "initial packet request count")
            journey.packet.unlink()  # Later steps must use the verified process custody.

            model.mail("msg_tui_idle")
            mail.publish("msg_tui_idle")
            terminal.expect("ROW_A_DONE_msg_tui_idle")
            terminal.capture("idle-mail")

            model.expect(("busy", ""))
            model.mail("msg_tui_busy")
            terminal.send("ROW_A_BUSY: hold this physical request for the busy-mail check.")
            require(model.busy_entered.wait(timeout=10), "busy provider request never entered")
            (journey.root / "busy-held").touch()
            mail.publish("msg_tui_busy")
            terminal.drive("send", "--text", "HOLD_DRAFT")
            terminal.expect("HOLD_DRAFT")
            require("HOLD_DRAFT" in terminal.capture("busy-draft"), "busy composer lost typed draft")
            require(model.count() == 5 and len(events(journey)) == 2, "mail executed while provider was held")
            (journey.root / "busy-held").unlink()
            model.busy_release.set()
            terminal.expect("ROW_A_DONE_msg_tui_busy")
            require("HOLD_DRAFT" in terminal.capture("busy-mail"), "mail completion lost unsent draft")
            terminal.drive("send", *(["BSpace"] * len("HOLD_DRAFT")))
            wait(lambda: "HOLD_DRAFT" not in terminal.drive("capture"), "unsent draft cleared before /compact")

            model.expect(("summary", ""), ("after-compact", ""))
            terminal.send("/compact")
            terminal.expect("Compaction will run before your next message.")
            terminal.send("ROW_A_AFTER_COMPACT: continue the same admitted scratch task.")
            terminal.expect("ROW_A_COMPACT_DONE")
            terminal.capture("compaction")
            require(model.count() == 10, "compaction did not use exactly summary and coding requests")
            terminal.close()
            journey.stop()

            # Publish while the service is down; delivery must survive restart.
            mail.publish("msg_tui_restart")
            journey.start(1)
            terminal.start(1)
            terminal.send("Continue after restart without silently recovering private custody.")
            terminal.expect("Rebind")
            terminal.capture("restart-refusal")
            require(model.count() == 10, "restart without capability reached model")
            journey.packet.write_bytes(PACKET)
            model.mail("msg_tui_restart")
            journey.api("PUT", f"/api/session/{SESSION}/stateengine/capability", {"capability": CAPABILITY})
            terminal.expect("ROW_A_DONE_msg_tui_restart")
            terminal.capture("restart-mail")
            write(journey.root / "history.json", journey.history())
            terminal.close()
            journey.stop()

            # Count only after both clients and product have joined; late
            # completions cannot escape duplicate/loss assertions.
            require(model.count() == 13 and not model.actions, "joined model sequence lost or duplicated work")
            expected = [(tool, identifier) for identifier in ("msg_tui_idle", "msg_tui_busy", "msg_tui_restart")
                        for tool in ("stateengine_msg_read", "stateengine_msg_send")]
            observed = events(journey)
            require([(row["tool"], row["message"]) for row in observed] == expected,
                    "joined MCP sequence lost, reordered or duplicated delivery")
            require(all(row["while_busy"] is False for row in observed), "MCP mail effect crossed held provider step")
            require("quiet_fixture" not in json.dumps(model.bodies), "quiet mail entered model context")
            require(Counter(mail.acks) == Counter(["quiet_fixture", "msg_tui_idle", "msg_tui_busy", "msg_tui_restart"]),
                    "joined source acknowledgement sequence differs")
            require(mail.count("close") == 2, "mail subscriptions did not close on both service exits")
            require(not provider.errors and not peer.errors, "fixture peer failed")
            result.update(passed=True, model_requests=13, initial_packet=True, idle_mail=True, busy_mail=True,
                unsent_draft_preserved=True, compaction_continuity=True, restart_delivery=True,
                duplicate_delivery=False, product_drained=True, tui_exits=2,
                prepared_launch_without_intervention=True)
            return result
        finally:
            model.busy_release.set()
            try:
                if terminal:
                    if terminal.session:
                        try:
                            terminal.capture("failure")
                        except (OSError, RuntimeError):
                            pass
                    terminal.cleanup()
            finally:
                if journey:
                    if journey.child and journey.child.poll() is None:
                        try:
                            write(journey.root / "failure-history.json", journey.history())
                        except (OSError, ValueError):
                            pass
                    journey.cleanup()
                    write(journey.root / "model-requests.json", model.bodies)
                    write(journey.root / "peers.json", {"mail_operations": mail.operations,
                        "mail_errors": peer.errors, "model_errors": provider.errors})
                    write(journey.root / "result.json", result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("cli", "product", "tui", "driver"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    args = parser.parse_args()
    for name in ("cli", "product", "tui", "driver"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    version = command([args.product, "--version"])
    require("testkit/fixture: loopback-provider-testkit" in version, "requires labelled testkit product")
    args.scratch = args.scratch.resolve()
    args.scratch.mkdir(mode=0o700)
    write(args.scratch / "binaries.json", {name: {"path": str(getattr(args, name)),
        "sha256": digest(getattr(args, name))} for name in ("cli", "product", "tui", "driver")})
    print(json.dumps(run(args)))


if __name__ == "__main__":
    main()
