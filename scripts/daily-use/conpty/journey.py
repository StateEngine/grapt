"""Operate and capture the actual native Rust TUI, from a declared scenario.

This is the R5 harness: it launches the SHIPPED `grapt-tui` binary under a real
Windows ConPTY (or Unix PTY), drives it with real keyboard and paste events
through a browser terminal, and captures settled screenshots and the raw
terminal byte stream. Nothing about the product is simulated -- no test hook, no
injected state, no scripted terminal. The client under observation is the same
binary a user runs.

## Why it reuses the retained observer rather than growing a second one

`scripts/terminal-harness` in the frozen TypeScript tree is a landed, canonical
observer with exactly the properties this campaign needs: a real PTY, xterm.js
5.5.0, authenticated loopback control, bounded requests and geometry, and --
the property that makes its screenshots evidence rather than decoration -- a
bounded settle step that marks every capture `quiescent`/`coherent` instead of
guessing that the screen had finished changing.

Writing a second observer here would be a duplicate implementation of proven
machinery, and a worse one. So this runner STAGES that observer, byte for byte,
into the run directory and records the SHA-256 of every file it copied.

Staging rather than referencing is deliberate, for two reasons. The TypeScript
root is FROZEN, and the observer needs `node_modules` installed beside it, so
driving it in place would write into a frozen root. And the receipt has to be
reproducible from canonical artifacts after landing, which a path into a
working tree would not be.

## What a scenario is

A JSON file naming an ordered list of steps. Every step is one of:

    {"key": "Control+p"}                 one key press through the browser
    {"paste": "line one\n\tline two"}    one paste through xterm's paste path
    {"resize": [100, 30]}                one PTY + renderer resize
    {"expect": "MODELS", "polls": 40}    wait, bounded, for text on the screen
    {"capture": "the model picker"}      one settled screenshot, labelled

There is no sleep step and no ambient clock anywhere in this file. `expect` is a
bounded count of fixed polls and says how long it actually waited when it gives
up; a scenario that depended on a sleep would be a scenario whose result
depended on the machine it ran on.

## What it refuses

An existing evidence directory, because overwriting evidence is how a receipt
comes to describe a run that did not happen. A missing binary or service root,
by name. And a scenario step it does not recognise, rather than skipping it --
a silently skipped step is a journey that reports more than it drove.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# The canonical observer, and the exact files this runner stages from it.
# Named individually rather than copied wholesale so that a new file appearing
# beside them is a deliberate adoption rather than something that arrives in an
# evidence run unnoticed.
HARNESS_FILES = (
    "server.mjs",
    "control.mjs",
    "index.html",
    "package.json",
    "package-lock.json",
    "README.md",
)

# Bounds. Every wait in this file is a bounded count of fixed polls.
READY_POLLS = 600
READY_SLEEP = 0.05
EXPECT_POLLS = 120
EXPECT_SLEEP = 0.25
REQUEST_TIMEOUT = 60.0


class HarnessError(RuntimeError):
    """A refusal this runner can name."""


def discover_browser() -> Path | None:
    """An already-installed Chromium the observer can drive.

    Playwright pins one build per version and refuses to launch when that exact
    build is absent, which is an ordinary state on a machine that has installed
    several. The observer accepts an explicit executable for exactly this, so
    rather than downloading another copy this picks the newest INSTALLATION
    that actually completed, and the receipt records which one ran.

    Returns `None` when nothing is installed, so the observer falls back to
    Playwright's own resolution and reports its own refusal by name.
    """
    root = Path.home() / "AppData" / "Local" / "ms-playwright"
    if not root.is_dir():
        return None
    candidates = []
    for entry in root.glob("chromium-*"):
        # The marker file is Playwright's own: a directory without it is a
        # partial or superseded download, which is exactly the state that
        # produced "executable doesn't exist" here.
        if not (entry / "INSTALLATION_COMPLETE").is_file():
            continue
        executable = entry / "chrome-win64" / "chrome.exe"
        if not executable.is_file():
            executable = entry / "chrome-linux" / "chrome"
        if executable.is_file():
            suffix = entry.name.rsplit("-", 1)[-1]
            candidates.append((int(suffix) if suffix.isdigit() else 0, executable))
    if not candidates:
        return None
    return max(candidates)[1]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def stage_harness(source: Path, into: Path) -> dict[str, str]:
    """Copy the canonical observer and record what was copied."""
    if not source.is_dir():
        raise HarnessError(f"the canonical terminal harness is absent: {source}")
    into.mkdir(parents=True, exist_ok=True)
    staged: dict[str, str] = {}
    for name in HARNESS_FILES:
        origin = source / name
        if not origin.is_file():
            raise HarnessError(f"the canonical harness is missing {name}: {origin}")
        shutil.copy2(origin, into / name)
        staged[name] = sha256(origin)
    return staged


def install(into: Path) -> None:
    """Install the observer's locked dependencies beside the staged copy."""
    if (into / "node_modules").is_dir():
        return
    result = subprocess.run(
        ["npm", "ci", "--ignore-scripts", "--workspaces=false"],
        cwd=into,
        capture_output=True,
        text=True,
        shell=os.name == "nt",
    )
    if result.returncode != 0:
        raise HarnessError(
            f"installing the observer's locked dependencies failed:\n{result.stderr[-4000:]}"
        )


def read_endpoint(service_root: Path) -> str:
    """The endpoint a live product published while serving."""
    receipt = service_root / "serve-ready.json"
    if not receipt.is_file():
        raise HarnessError(
            f"no live product at {service_root}: {receipt.name} is absent. "
            "Start the product against this root first."
        )
    value = json.loads(receipt.read_text(encoding="utf-8"))
    endpoint = value.get("observed_endpoint")
    if not isinstance(endpoint, str) or not endpoint:
        raise HarnessError(f"{receipt} carries no observed_endpoint")
    return endpoint


class Observer:
    """The authenticated control surface of one running observer."""

    def __init__(self, ready: dict[str, object]) -> None:
        self.url = str(ready["url"])
        self.token = str(ready["token"])
        self.artifacts = Path(str(ready["artifacts"]))

    def _call(self, method: str, path: str, payload: object | None = None) -> dict:
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            f"{self.url}{path}",
            data=data,
            method=method,
            headers={
                "Authorization": f"Bearer {self.token}",
                "Content-Type": "application/json",
                "Host": self.url.removeprefix("http://"),
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT) as response:
                return json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as error:
            body = error.read().decode("utf-8", "replace")
            raise HarnessError(f"{method} {path} answered {error.code}: {body}") from error

    def state(self) -> dict:
        return self._call("GET", "/state")

    def action(self, action: dict) -> dict:
        return self._call("POST", "/action", action)

    def screenshot(self) -> dict:
        return self._call("POST", "/screenshot")

    def stop(self) -> None:
        try:
            self._call("POST", "/stop")
        except (HarnessError, OSError):
            # The observer shuts down as it answers, so a dropped connection
            # here is the ordinary case and not a failure to report.
            pass


def screen_text(state: dict) -> str:
    """The terminal's visible text, however the observer spells it."""
    for key in ("text", "screen", "lines"):
        value = state.get(key)
        if isinstance(value, str):
            return value
        if isinstance(value, list):
            return "\n".join(str(line) for line in value)
    return json.dumps(state)


def run_scenario(observer: Observer, steps: list[dict], log: list[dict]) -> None:
    for index, step in enumerate(steps, start=1):
        if "key" in step:
            observer.action({"kind": "key", "key": step["key"]})
            log.append({"step": index, "key": step["key"]})
        elif "paste" in step:
            observer.action({"kind": "paste", "text": step["paste"]})
            log.append({"step": index, "paste_bytes": len(step["paste"].encode("utf-8"))})
        elif "resize" in step:
            cols, rows = step["resize"]
            observer.action({"kind": "resize", "cols": int(cols), "rows": int(rows)})
            log.append({"step": index, "resize": [int(cols), int(rows)]})
        elif "expect" in step:
            wanted = str(step["expect"])
            polls = int(step.get("polls", EXPECT_POLLS))
            seen = False
            for _ in range(polls):
                if wanted in screen_text(observer.state()):
                    seen = True
                    break
                time.sleep(EXPECT_SLEEP)
            waited = round(polls * EXPECT_SLEEP, 2)
            log.append({"step": index, "expect": wanted, "observed": seen, "polls": polls})
            if not seen:
                # Named, and fatal. A journey that carried on past an unmet
                # expectation would capture screens of a state it never reached
                # and file them as evidence that it did.
                raise HarnessError(
                    f"step {index}: `{wanted}` never appeared within {polls} polls "
                    f"({waited}s of bounded waiting)"
                )
        elif "capture" in step:
            shot = observer.screenshot()
            capture = shot.get("capture", {})
            log.append(
                {
                    "step": index,
                    "capture": step["capture"],
                    "path": shot.get("path"),
                    "quiescent": capture.get("quiescent"),
                    "coherent": capture.get("coherent"),
                }
            )
        else:
            raise HarnessError(f"step {index}: unrecognised step {step!r}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scenario", required=True, type=Path)
    parser.add_argument(
        "--binary", required=True, type=Path, help="the shipped grapt-tui executable"
    )
    parser.add_argument(
        "--service-root",
        type=Path,
        help="a live product's service root; omitted runs against the contract double",
    )
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument(
        "--browser",
        type=Path,
        help="an installed Chromium executable; discovered when omitted",
    )
    parser.add_argument(
        "--harness",
        type=Path,
        required=True,
        help="the canonical terminal observer to stage",
    )
    arguments = parser.parse_args()

    if arguments.evidence.exists():
        raise HarnessError(
            f"{arguments.evidence} already exists; every run needs a fresh evidence "
            "directory so a receipt cannot describe a mixture of two runs"
        )
    if not arguments.binary.is_file():
        raise HarnessError(f"the native client binary is absent: {arguments.binary}")

    scenario = json.loads(arguments.scenario.read_text(encoding="utf-8"))
    steps = scenario.get("steps")
    if not isinstance(steps, list) or not steps:
        raise HarnessError(f"{arguments.scenario} declares no steps")

    arguments.evidence.mkdir(parents=True)
    staging = arguments.evidence / "harness"
    staged = stage_harness(arguments.harness, staging)
    install(staging)

    args: list[str] = []
    target = "the in-process contract double"
    if arguments.service_root is not None:
        endpoint = read_endpoint(arguments.service_root)
        args = ["--service", str(arguments.service_root)]
        target = endpoint

    artifacts = arguments.evidence / "artifacts"
    launch = {
        "executable": str(arguments.binary.resolve()),
        "args": args,
        "cwd": str(arguments.evidence.resolve()),
        # An EXPLICIT environment. The observer merges this over its own, and
        # the campaign's isolation depends on the client inheriting nothing that
        # would point it at a daily-driver root.
        "env": {"NO_COLOR": "1"},
        "removeEnv": ["STATEENGINE_AGENT_ID", "STATEENGINE_PROJECT_ID"],
        "artifacts": str(artifacts.resolve()),
    }
    browser = arguments.browser or discover_browser()
    if browser is not None:
        launch["browser"] = str(browser)
    launch_path = arguments.evidence / "launch.json"
    launch_path.write_text(json.dumps(launch, indent=2), encoding="utf-8")

    server = subprocess.Popen(
        ["node", "server.mjs", str(launch_path.resolve())],
        cwd=staging,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    ready_path = artifacts / "ready.json"
    ready: dict[str, object] | None = None
    for _ in range(READY_POLLS):
        if server.poll() is not None:
            _, stderr = server.communicate()
            raise HarnessError(f"the observer exited before it was ready:\n{stderr[-4000:]}")
        if ready_path.is_file():
            try:
                ready = json.loads(ready_path.read_text(encoding="utf-8"))
                break
            except json.JSONDecodeError:
                pass  # still being written
        time.sleep(READY_SLEEP)
    if ready is None:
        server.kill()
        raise HarnessError(
            f"the observer published no ready receipt within "
            f"{round(READY_POLLS * READY_SLEEP, 1)}s"
        )

    observer = Observer(ready)
    log: list[dict] = []
    failure: str | None = None
    try:
        run_scenario(observer, steps, log)
    except HarnessError as error:
        failure = str(error)
    finally:
        # A final capture even on failure: the screen the journey stopped on is
        # the most useful single artifact a failed run can leave.
        try:
            observer.screenshot()
        except (HarnessError, OSError):
            pass
        observer.stop()
        try:
            server.wait(timeout=30)
        except subprocess.TimeoutExpired:
            server.kill()

    receipt = {
        "contract": "native-tui-journey/v1",
        "scenario": {
            "path": str(arguments.scenario.resolve()),
            "sha256": sha256(arguments.scenario),
            "name": scenario.get("name"),
        },
        "client": {
            "binary": str(arguments.binary.resolve()),
            "sha256": sha256(arguments.binary),
        },
        "target": target,
        "service_root": str(arguments.service_root) if arguments.service_root else None,
        "harness": {
            "source": str(arguments.harness),
            "files": staged,
            "browser": str(browser) if browser else "playwright default",
        },
        "artifacts": str(artifacts.resolve()),
        "steps": log,
        "failure": failure,
    }
    receipt_path = arguments.evidence / "receipt.json"
    receipt_path.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(json.dumps({"receipt": str(receipt_path), "failure": failure}, indent=2))
    return 1 if failure else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except HarnessError as error:
        print(f"native-tui journey: {error}", file=sys.stderr)
        sys.exit(2)
