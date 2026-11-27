#!/usr/bin/env python3
"""Exercise real frontend interrupt/quit in a private PTY and loopback product.

Pass built grapt-server, grapt and grapt-tui binaries. Requires tmux.
The model is scripted; no paid provider or live StateEngine profile is used.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import threading
import time

import headless_acceptance as headless
from acceptance_peer import Model, Server
from sustained_provider import text


class Holding(Model):
    def __init__(self):
        super().__init__()
        self.waiting = [threading.Event() for _ in range(3)]
        self.release = [threading.Event() for _ in range(3)]

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        index = self.count() - 1
        headless.require(0 <= index < 3, "unexpected model request")
        self.waiting[index].set()
        headless.require(self.release[index].wait(30), "model release bound")
        return text("The interrupted provider response must not resume the request.")


def run(args):
    args.scratch.mkdir(mode=0o700)
    frames = args.scratch / "frames"
    frames.mkdir()
    environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                   "LANG": "C.UTF-8", "TERM": "xterm-256color",
                   "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
                   "PYTHONDONTWRITEBYTECODE": "1"}
    for variable, name in (("HOME", "home"), ("USERPROFILE", "home"),
                           ("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"),
                           ("XDG_STATE_HOME", "state"), ("XDG_CACHE_HOME", "cache"),
                           ("TMPDIR", "tmp")):
        directory = args.scratch / name
        directory.mkdir(exist_ok=True)
        environment[variable] = str(directory)
    workspace = args.scratch / "workspace"
    workspace.mkdir()
    socket = args.scratch / "tmux.sock"
    tmux = shutil.which("tmux")
    headless.require(tmux is not None, "tmux is required for the PTY witness")
    model = Holding()
    journey = None
    started = False
    original = dict(os.environ)
    result = {"real_product": True, "real_frontend": True,
              "paid_provider_calls": 0, "live_profile_touched": False,
              "passed": False, "cases": []}

    def terminal(*command):
        completed = subprocess.run([tmux, "-f", "/dev/null", "-S", str(socket), *command],
            env=environment, capture_output=True, text=True, timeout=10)
        headless.require(completed.returncode == 0, completed.stderr[:500])
        return completed.stdout

    def screen():
        return terminal("capture-pane", "-p", "-t", "audit:0.0")

    def capture(name):
        (frames / (name + ".txt")).write_text(screen())

    def key(value):
        terminal("send-keys", "-t", "audit:0.0", value)
        time.sleep(.1)

    def prompt(value):
        terminal("send-keys", "-t", "audit:0.0", "-l", value)
        key("Enter")

    def wait(predicate, label):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.05)
        capture("failed-" + label)
        raise RuntimeError("PTY condition absent: " + label)

    def alive():
        return terminal("display-message", "-p", "-t", "audit:0.0", "#{pane_dead}").strip() == "0"

    def closed():
        return terminal("display-message", "-p", "-t", "audit:0.0",
                        "#{pane_dead} #{pane_dead_status}").strip() == "1 0"

    def interrupted():
        rows = journey.history()["data"]
        return bool(rows and rows[-1].get("error", {}).get("type") == "aborted")

    try:
        os.environ.clear()
        os.environ.update(environment)
        with Server(0, model.dispatch) as provider:
            journey = headless.Journey(args, "ctrl-c-service", provider.endpoint)
            config = journey.profile / "grapt.jsonc"
            document = json.loads(config.read_text())
            document.pop("mcp", None)
            document.pop("stateengine", None)
            document["permission"] = {"*": "allow"}
            headless.write(config, document)
            journey.environment["GRAPT_TEST_CATALOG_ROOT"] = str(args.scratch)
            (Path(journey.environment["GRAPT_CONFIG_DIR"]) / "grapt.json").write_text(
                json.dumps({"permission": {"*": "allow"}}))
            journey.start(0)
            for index, mode in enumerate(("single-control-c", "double-control-c", "slash-quit")):
                session = journey.api("POST", "/api/session", {
                    "agent": "agt_live_headless1",
                    "model": {"providerID": "openai", "id": "gpt-live"},
                    "location": {"directory": str(workspace)}})["data"]["id"]
                headless.SESSION = session
                command = "exec " + shlex.join([str(args.frontend), "--service",
                    str(journey.profile), "--session", session, "--dark", "--no-notifications"])
                if not started:
                    terminal("new-session", "-d", "-s", "audit", "-x", "120", "-y", "40",
                             "-c", str(workspace), command)
                    started = True
                    terminal("set-option", "-g", "status", "off")
                    terminal("set-option", "-g", "remain-on-exit", "on")
                else:
                    headless.require(closed(), "previous frontend did not exit cleanly")
                    terminal("respawn-pane", "-t", "audit:0.0", "-c", str(workspace), command)
                wait(lambda: "Connected" in screen(), "connected-" + mode)
                prompt("Wait while I test " + mode + ".")
                headless.require(model.waiting[index].wait(10), "model request did not start")
                capture(mode + "-running")
                if index == 0:
                    key("C-b")
                    key("C-d")
                    headless.require(alive(), "Ctrl+B or Ctrl+D exited the frontend")
                    key("C-c")
                    wait(interrupted, "single-interrupted")
                    headless.require(alive(), "one Ctrl+C exited the frontend")
                    wait(lambda: "Execution interrupted" in screen(), "interrupted-frame")
                    capture("single-control-c-alive-interrupted")
                    model.release[index].set()
                    time.sleep(1.7)
                    headless.require("Press Ctrl+C again to exit" not in screen(), "quit hint did not expire")
                    terminal("send-keys", "-t", "audit:0.0", "-l", "UNSENT multiline draft")
                    key("C-c")
                    headless.require(alive() and "UNSENT multiline draft" not in screen(), "idle Ctrl+C did not clear")
                    capture("idle-draft-cleared")
                    time.sleep(1.7)
                    key("C-c")
                    headless.require(alive() and "Press Ctrl+C again to exit" in screen(), "first empty Ctrl+C")
                    capture("idle-empty-confirmation")
                    time.sleep(1.7)
                    headless.require(alive() and "Press Ctrl+C again to exit" not in screen(), "expired confirmation")
                    key("C-c")
                    key("C-c")
                elif index == 1:
                    key("C-c")
                    headless.require(alive(), "first Ctrl+C exited while running")
                    key("C-c")
                else:
                    prompt("/quit")
                wait(closed, "clean-exit-" + mode)
                wait(interrupted, "no-orphan-" + mode)
                model.release[index].set()
                history = journey.history()
                (args.scratch / (mode + "-history.json")).write_text(json.dumps(history, indent=2))
                result["cases"].append({"mode": mode, "frontend_exit": 0,
                                         "public_outcome": "aborted"})
            headless.require(model.count() == 3, "quit keys submitted an extra model request")
            headless.require(not provider.errors, "scripted provider errors: " + str(provider.errors))
            result["passed"] = True
    finally:
        for release in model.release:
            release.set()
        errors = []
        if started:
            try:
                terminal("kill-server")
            except Exception as error:
                errors.append("tmux:" + type(error).__name__)
        if journey:
            try:
                journey.stop()
            except Exception as error:
                errors.append("drain:" + type(error).__name__)
            journey.cleanup()
        os.environ.clear()
        os.environ.update(original)
        result["cleanup_errors"] = errors
        result["passed"] = result["passed"] and not errors
        (args.scratch / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    headless.require(result["passed"], "private PTY cleanup failed")
    print(json.dumps(result), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("product", "cli", "frontend", "scratch"):
        parser.add_argument("--" + option, type=lambda value: Path(value).resolve(), required=True)
    run(parser.parse_args())
