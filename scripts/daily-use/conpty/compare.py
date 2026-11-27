"""Drive the native and the retained TUI through the SAME journey, and measure.

J04 asks for a comparison of the two clients on identical declared scenarios,
with raw timings and resource bounds in a reproducible environment. This is that
comparison, and its design is shaped by two things that are easy to get wrong.

## The actions are identical; the observations cannot be

Both clients receive the SAME keys, pastes and resizes, in the same order,
against the SAME running product. That is what makes the comparison fair.

What they cannot share is what to wait for. The two clients draw different
screens — the native one labels a pane `MODELS`, the retained one draws
`Select a model` — so a scenario declares its waits PER CLIENT:

    {"expect": {"native": "MODELS", "retained": "Select Model"}}

Sharing one expectation string would mean one client waited on the other's
vocabulary and either passed instantly or timed out, and neither is a
measurement. Declaring them separately keeps the actions identical and makes
each wait a real observation of the client actually under test.

## What the timings are, and are not

Each step is timed by this runner with a monotonic clock, OUTSIDE both clients.
A number reported by the thing being measured is not a measurement.

The number a step yields is therefore end-to-end: the observer's browser
keyboard, the PTY, the client, the product, and the terminal repaint. That is
the right quantity for "does this feel slower", and the wrong one for
attributing a difference to any single layer. The receipt says so rather than
letting a reader assume otherwise.

`expect` steps are bounded polls, so their resolution is the poll interval and
their reported time is an upper bound on the truth. Comparing two clients under
the same interval is still meaningful; reading a single figure as latency is
not.

## Resource bounds

Peak working set and total processor time are read from the operating system
for the client process and its descendants, after the run, through the OS's own
accounting rather than anything either client reports about itself.

Usage:

    python compare.py --scenario s.json --native <grapt-tui.exe>
                      --service-root <dir> --ts-root <oracle-root>
                      --evidence <fresh dir>
"""

from __future__ import annotations

import argparse
import json
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from journey import (  # noqa: E402  (path is set above on purpose)
    HarnessError,
    Observer,
    discover_browser,
    install,
    read_endpoint,
    screen_text,
    sha256,
    stage_harness,
)

READY_POLLS = 900
READY_SLEEP = 0.05
EXPECT_POLLS = 240
EXPECT_SLEEP = 0.25

# The isolated profile a retained-TUI run gets, mirroring the dogfood runbook's
# own isolation: every XDG root, the test home, the config directory and the
# temporary directories are inside the run, so the retained client cannot reach
# a daily-driver profile.
PROFILE_KEYS = (
    ("XDG_DATA_HOME", "data"),
    ("XDG_CONFIG_HOME", "config"),
    ("XDG_CACHE_HOME", "cache"),
    ("XDG_STATE_HOME", "state"),
    ("OPENCODE_TEST_HOME", "home"),
    ("OPENCODE_CONFIG_DIR", "config/opencode"),
    ("TEMP", "tmp"),
    ("TMP", "tmp"),
)


def credential(service_root: Path) -> tuple[str, str]:
    record = (service_root / "basic.credential").read_text(encoding="utf-8").strip()
    username, _, secret = record.partition(":")
    if not username or not secret:
        raise HarnessError(f"{service_root}/basic.credential is not one username:secret record")
    return username, secret


def native_launch(binary: Path, service_root: Path, run: Path, artifacts: Path) -> dict:
    return {
        "executable": str(binary.resolve()),
        "args": ["--service", str(service_root.resolve())],
        "cwd": str(run.resolve()),
        "env": {"NO_COLOR": "1"},
        "removeEnv": ["STATEENGINE_AGENT_ID", "STATEENGINE_PROJECT_ID"],
        "artifacts": str(artifacts.resolve()),
    }


def launch_boundary(run: Path) -> None:
    """Make the retained client's launch folder a repository of its own.

    The retained client resolves a project from the directory it is given and
    REFUSES to start without one: handed this evidence directory it exited
    before drawing a single screen, with `Git repository topology could not be
    resolved`, and the comparison recorded zero steps for that side.

    Initialising HERE is the same boundary `service.py` draws around the service
    root, and for the same reason: without it, discovery walks up out of the
    evidence tree and the client's project becomes whatever repository happens
    to contain `<evidence-root>`. That would put a real repository under a
    throwaway client run.

    The scenario enters this folder with `i` deliberately — it is the route that
    does NOT navigate the real registry — so an empty repository is the whole
    requirement, not a stand-in for one.
    """
    run.mkdir(parents=True, exist_ok=True)
    if (run / ".git").exists():
        return
    initialized = subprocess.run(
        ["git", "init", "--quiet", str(run)], capture_output=True, text=True
    )
    if initialized.returncode != 0:
        raise HarnessError(
            f"the retained client's launch boundary was refused: {initialized.stderr.strip()}"
        )


def retained_launch(
    bun: Path,
    ts_root: Path,
    endpoint: str,
    secret: str,
    run: Path,
    artifacts: Path,
    project_config: Path | None = None,
) -> dict:
    profile = run / "profile"
    launch_boundary(run)
    # THE PROJECT'S OWN CONFIGURATION, placed before the client starts.
    #
    # MCP servers are declared PER PROJECT, and this run directory IS the
    # retained client's project. A journey exercising MCP tools against a
    # service whose own root declares them therefore saw NONE of them: the
    # client legitimately resolved a project with no configuration, and the
    # model correctly reported the tool as unavailable. That looked like a
    # product defect and was a defect in the journey's setup.
    if project_config is not None:
        # COPIED TO A NAME CONFIGURATION DISCOVERY ACTUALLY READS. Preserving
        # the source filename put the file in the right directory under a name
        # nothing looks for, so the run failed exactly as it had before and
        # looked like the same product defect a second time.
        #
        # `{{RUN}}` RESOLVES TO THIS RUN'S OWN DIRECTORY, so anything the
        # config points at — an MCP server's transcript, say — lands inside
        # the evidence this run owns. A fixed path meant every run overwrote
        # the last, and a NEGATIVE witness is exactly the kind that gets
        # destroyed that way: the refusal transcript proving no `tools/call`
        # was sent is only evidence while it still exists. The harness already
        # refuses a reused evidence directory, so a per-run path is immutable
        # by construction.
        body = project_config.read_text(encoding="utf-8")
        body = body.replace("{{RUN}}", str(run.resolve()).replace("\\", "/"))
        body = body.replace("{{DAILY_USE}}", str(Path(__file__).resolve().parents[1]).replace("\\", "/"))
        (run / "opencode.jsonc").write_text(body, encoding="utf-8")
    env = {
        # The retained client authenticates to the local API from the
        # environment, never from an argument, so the secret never reaches a
        # command line or a receipt.
        "OPENCODE_SERVER_PASSWORD": secret,
        "OPENCODE_PASSWORD": secret,
        "OPENCODE_DISABLE_AUTOUPDATE": "1",
    }
    for key, name in PROFILE_KEYS:
        target = profile / name
        target.mkdir(parents=True, exist_ok=True)
        env[key] = str(target.resolve())
    return {
        # bun.exe rather than the bun.cmd shim: the shim is a batch wrapper, so
        # the process this runner measures would be cmd.exe and its accounting
        # would describe the wrapper rather than the client.
        "executable": str(bun.resolve()),
        "args": [
            "run",
            "--conditions=browser",
            str((ts_root / "packages/cli/src/index.ts").resolve()),
            "--server",
            endpoint,
            str(run.resolve()),
        ],
        "cwd": str((ts_root / "packages/cli").resolve()),
        "env": env,
        # Seat identity must not follow the retained client into a real project
        # registry: the oracle observation is explicit that only Grapt and the
        # operator's own identity may be navigated there.
        "removeEnv": [
            "STATEENGINE_AGENT_ID",
            "STATEENGINE_PROJECT_ID",
            "STATEENGINE_SEAT",
            "STATEENGINE_AGENT",
        ],
        "artifacts": str(artifacts.resolve()),
    }


def resources(pid: int) -> dict:
    """Peak working set and processor time for a process tree, from the OS.

    Read through the OS's own accounting rather than from anything either client
    reports about itself. Returns what it could observe and says when it could
    not: a process that has already exited leaves no counters, and inventing a
    zero would put a false number beside a real one.
    """
    script = (
        "$ErrorActionPreference='SilentlyContinue';"
        f"$ids=@({pid});"
        f"$kids=Get-CimInstance Win32_Process -Filter \"ParentProcessId={pid}\";"
        "$ids+=$kids.ProcessId;"
        "$out=@();"
        "foreach($i in $ids){$p=Get-Process -Id $i;"
        "if($p){$out+=[pscustomobject]@{pid=$i;peak=$p.PeakWorkingSet64;cpu=$p.TotalProcessorTime.TotalSeconds}}};"
        "$out | ConvertTo-Json -Compress"
    )
    result = subprocess.run(
        ["powershell", "-NoProfile", "-Command", script],
        capture_output=True,
        # DECODED EXPLICITLY. `text=True` decodes with the LOCALE encoding,
        # which is cp1252 on this machine — the same defect that made an
        # analysis script report a clean em dash as three mojibake characters
        # and nearly had a capture-path Unicode defect reported that did not
        # exist. These are counters today, but a reader that guesses an encoding
        # is wrong by construction.
        encoding="utf-8",
        errors="replace",
    )
    body = result.stdout.strip()
    if result.returncode != 0 or not body:
        return {"observed": False, "reason": "the process left no readable counters"}
    try:
        rows = json.loads(body)
    except json.JSONDecodeError:
        return {"observed": False, "reason": "the counters did not parse"}
    if isinstance(rows, dict):
        rows = [rows]
    # A row can carry an explicit null: `Get-Process` returns the process but
    # `TotalProcessorTime` is unreadable for it, which happens for a child this
    # session may not open. Coercing that to zero is precisely the false number
    # beside a real one that this function exists to refuse, and summing it
    # silently understates the total. So count the readable rows and SAY how
    # many were not, rather than reporting a figure that looks complete.
    peaks = [int(row["peak"]) for row in rows if row.get("peak") is not None]
    cpus = [float(row["cpu"]) for row in rows if row.get("cpu") is not None]
    sample = {
        "observed": True,
        "processes": len(rows),
        "peak_working_set_bytes": max(peaks) if peaks else None,
        "processor_seconds": round(sum(cpus), 3) if cpus else None,
        "processes_without_counters": len(rows) - min(len(peaks), len(cpus)),
    }
    if sample["processes_without_counters"]:
        sample["reason"] = (
            "at least one process in the tree exposed no readable counters; the "
            "figures above describe only the processes that did"
        )
    return sample


def cut_connections(control: Path) -> dict:
    """Tell the relay to drop every live connection, and confirm it did."""
    cut_flag = control.with_suffix(".cut")
    cut_flag.write_text("cut", encoding="utf-8")
    for _ in range(200):
        if not cut_flag.exists():
            return {"control": str(control), "state": control.read_text(encoding="utf-8")}
        time.sleep(0.05)
    raise HarnessError(f"the relay never acknowledged a cut at {control}")


def sessions_in(service_root: Path) -> set[str] | None:
    """Session aggregates in the durable store, or None if it could not be read.

    NONE IS NOT AN EMPTY SET, and conflating them fabricates evidence. An
    earlier revision returned `set()` on failure, so a BEFORE snapshot that
    failed while the AFTER snapshot succeeded made every preexisting session
    look newly created — attributing another run's sessions to this client. A
    read that did not happen has to be distinguishable from a store that held
    nothing.
    """
    database = service_root / "database.sqlite3"
    if not database.is_file():
        return None
    try:
        connection = sqlite3.connect(f"file:{database.as_posix()}?mode=ro", uri=True)
    except sqlite3.Error:
        return None
    try:
        rows = connection.execute(
            "SELECT DISTINCT aggregate_id FROM event WHERE aggregate_family = 'session'"
        ).fetchall()
    except sqlite3.Error:
        return None
    finally:
        connection.close()
    return {row[0] for row in rows if row and row[0]}


def sessions_created(
    before: set[str] | None, after: set[str] | None
) -> tuple[list[str] | None, str | None]:
    """What this client created, or NO BINDING and why.

    Both snapshots must have succeeded. A binding derived from one of them is
    not a weaker binding, it is a wrong one.
    """
    if before is None and after is None:
        return None, "the session store could not be read before or after this client"
    if before is None:
        return None, "the session store could not be read BEFORE this client, so a new session cannot be told from a preexisting one"
    if after is None:
        return None, "the session store could not be read after this client"
    return sorted(after - before), None


def run_client(
    label: str,
    launch: dict,
    steps: list[dict],
    run: Path,
    harness: Path,
    effect_root: Path,
    service_root: Path,
) -> dict:
    # Which sessions already existed. Whatever appears while THIS client runs is
    # this client's, which is a launch fact rather than an inference from how
    # its calls turned out.
    sessions_before = sessions_in(service_root)
    # Hashed BEFORE the process starts, so the digest belongs to this run.
    executable = Path(launch["executable"])
    launched_sha256 = sha256(executable) if executable.is_file() else None
    run.mkdir(parents=True, exist_ok=True)
    staging = run / "harness"
    staged = stage_harness(harness, staging)
    install(staging)
    browser = discover_browser()
    if browser is not None:
        launch["browser"] = str(browser)
    launch_path = run / "launch.json"
    launch_path.write_text(json.dumps(launch, indent=2), encoding="utf-8")

    server = subprocess.Popen(
        ["node", "server.mjs", str(launch_path.resolve())],
        cwd=staging,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    ready_path = Path(launch["artifacts"]) / "ready.json"
    ready = None
    started = time.monotonic()
    for _ in range(READY_POLLS):
        if server.poll() is not None:
            _, stderr = server.communicate()
            raise HarnessError(f"[{label}] the observer exited before ready:\n{stderr[-4000:]}")
        if ready_path.is_file():
            try:
                ready = json.loads(ready_path.read_text(encoding="utf-8"))
                break
            except json.JSONDecodeError:
                pass
        time.sleep(READY_SLEEP)
    if ready is None:
        server.kill()
        raise HarnessError(f"[{label}] no readiness receipt within the bound")

    # From spawning the observer to the client's first readable process, which
    # is the closest thing to "how long before it is there" that can be measured
    # from outside. It includes the observer's own browser start, so it is
    # comparable BETWEEN the two clients and meaningless on its own.
    startup = round(time.monotonic() - started, 3)
    observer = Observer(ready)
    timings: list[dict] = []
    failure: str | None = None
    usage = {"observed": False, "reason": "not sampled"}
    try:
        for index, step in enumerate(steps, start=1):
            at = time.monotonic()
            # Sampled BEFORE each step, while the client is certainly alive.
            # Sampling once at the end read nothing at all, because the last
            # step of every journey is the quit key: the counters describe a
            # process that no longer exists, and an unobserved resource figure
            # beside a real one is worse than no figure.
            sample = resources(int(ready["pid"]))
            if sample.get("observed"):
                usage = sample
            action_started_ms = time.time_ns() // 1_000_000
            kind, detail, shared = apply_step(observer, step, label, effect_root)
            action_completed_ms = time.time_ns() // 1_000_000
            timings.append(
                {
                    "step": index,
                    "kind": kind,
                    "detail": detail,
                    # Bounds around the external observer action, not a claimed
                    # timestamp of the frontend's internal key handler.
                    "action_started_ms": action_started_ms,
                    "action_completed_ms": action_completed_ms,
                    "submission": step.get("submission", False),
                    # Only steps both clients received identically are
                    # comparable as like for like; the rest are the navigation
                    # each client needs to reach the same place.
                    "shared": shared,
                    "seconds": round(time.monotonic() - at, 3),
                }
            )
    except HarnessError as error:
        failure = str(error)
        sample = resources(int(ready["pid"]))
        if sample.get("observed"):
            usage = sample
    finally:
        try:
            observer.screenshot()
        except (HarnessError, OSError):
            pass
        observer.stop()
        try:
            server.wait(timeout=30)
        except subprocess.TimeoutExpired:
            server.kill()

    created, binding_error = sessions_created(sessions_before, sessions_in(service_root))
    return {
        "client": label,
        # The session(s) this client created, for binding an expectation
        # without searching. More than one is reported rather than collapsed,
        # and a read that failed yields NO binding plus the reason — never a
        # binding assembled from a half-read store.
        "sessions_created": created,
        "session_binding_error": binding_error,
        # WHERE an on-disk effect was observed. Recorded so a reader can check
        # the runner looked in this client's own project rather than trusting
        # that it did — and so a mismatch is visible in the receipt itself.
        "effect_root": str(effect_root.resolve()),
        "launch": {
            "executable": launch["executable"],
            "args": launch["args"],
            # THE BYTES THAT ACTUALLY RAN, hashed AT LAUNCH.
            #
            # A path proves nothing about a mutable build output: `target/debug`
            # is rewritten by the next `cargo build`, so hashing it afterwards
            # dates the file, not the run. An earlier packet claimed a set of
            # receipts shared one binary and one of them did not — recorded here
            # so a reader never has to take that on trust again.
            "executable_sha256": launched_sha256,
        },
        "artifacts": launch["artifacts"],
        "startup_seconds": startup,
        "steps": timings,
        "resources": usage,
        "failure": failure,
    }


def resolve(value: object, label: str) -> tuple[object, bool]:
    """One step's value for this client, and whether both clients share it.

    A plain value is SHARED — both clients receive exactly the same thing, and
    only those steps are comparable as like for like. A mapping is client
    specific, which the two clients genuinely need: they draw different screens,
    and the retained client reaches a composer through a different route than
    the native one.

    The distinction is carried into the receipt rather than smoothed over. A
    comparison that presented client-specific navigation as identical input
    would be inviting a reader to compare two different journeys.
    """
    if isinstance(value, dict):
        return value.get(label), False
    return value, True


# The most a marker file may hold before this runner refuses to read it as
# evidence. These files are written by a scenario and hold one word.
EFFECT_BYTE_LIMIT = 4096


def read_effect(target: Path, limit: int) -> tuple[str, str]:
    """Read a bounded marker file, distinguishing every way it can fail.

    Returns one of `missing`, `oversized`, `unreadable`, `invalid` or `read`.
    They are kept apart because they mean different things to a reader: absent
    evidence is not the same as evidence too large to trust, and neither is the
    same as bytes that are not the text they claim to be.
    """
    try:
        with target.open("rb") as handle:
            # limit + 1 so "exactly at the limit" and "over it" are
            # distinguishable without reading the whole file.
            raw = handle.read(limit + 1)
    except FileNotFoundError:
        return "missing", ""
    except OSError:
        return "unreadable", ""
    if len(raw) > limit:
        return "oversized", ""
    try:
        return "read", raw.decode("utf-8")
    except UnicodeDecodeError:
        # NOT replaced with substitutions: a marker that is not valid UTF-8 is
        # not the marker, and quietly repairing it would let a corrupt write
        # satisfy a text assertion.
        return "invalid", ""


def apply_step(
    observer: Observer, step: dict, label: str, effect_root: Path
) -> tuple[str, object, bool]:
    if "key" in step:
        key, shared = resolve(step["key"], label)
        if key is None:
            return "key", {"skipped": "this client does not take this key"}, shared
        observer.action({"kind": "key", "key": key})
        return "key", key, shared
    if "paste" in step:
        text, shared = resolve(step["paste"], label)
        if text is None:
            return "paste", {"skipped": "this client does not take this paste"}, shared
        observer.action({"kind": "paste", "text": text})
        return "paste", len(text.encode("utf-8")), shared
    if "resize" in step:
        cols, rows = step["resize"]
        observer.action({"kind": "resize", "cols": int(cols), "rows": int(rows)})
        return "resize", [int(cols), int(rows)], True
    if "capture" in step:
        shot = observer.screenshot()
        capture = shot.get("capture", {})
        return (
            "capture",
            {
                "label": step["capture"],
                "path": shot.get("path"),
                "coherent": capture.get("coherent"),
            },
            True,
        )
    if "cut" in step:
        # The reconnect journey needs the connection to ACTUALLY break while the
        # server stays where it is. Restarting the product cannot do that — it
        # admits only `--port 0` (`IA0-1R admits only --port 0`), so it returns
        # on a different address and a client handed a new endpoint would be
        # exercising configuration rather than recovery. The clients are pointed
        # at a byte-for-byte relay instead, and this drops every socket it holds.
        detail = cut_connections(Path(step["cut"]))
        return "cut", detail, True
    if "absent" in step:
        # A bounded wait for text to GO AWAY.
        #
        # Only meaningful after the same text has been OBSERVED present, which
        # is how it is used: a bare "not on screen" proves nothing, because it
        # is satisfied by a client that never drew it, never started, or drew it
        # a moment later. Present-then-absent is a transition, and a transition
        # is an observation.
        wanted, shared = resolve(step["absent"], label)
        if wanted is None:
            return "absent", {"skipped": "this client declares no absence here"}, shared
        polls = int(step.get("polls", EXPECT_POLLS))
        for _ in range(polls):
            if screen_text(observer.state()).count(wanted) == 0:
                return "absent", {"text": wanted, "gone": True}, shared
            time.sleep(EXPECT_SLEEP)
        raise HarnessError(
            f"[{label}] `{wanted}` was still on screen after {polls} polls "
            f"({round(polls * EXPECT_SLEEP, 1)}s of bounded waiting)"
        )
    if "effect" in step:
        # An EFFECT ON DISK, observed outside the client entirely.
        #
        # Every other verb reads the screen, and a screen can be satisfied by a
        # model that TALKED about doing the work. A file either exists with the
        # expected bytes or it does not, and no amount of fluent output creates
        # one — so this is the step that separates "the tool ran" from "the
        # assistant said it ran".
        #
        # It is read from the client's OWN isolated run directory, so the two
        # clients are checked against their own projects and neither can be
        # satisfied by the other's work.
        wanted, shared = resolve(step["effect"], label)
        if wanted is None:
            return "effect", {"skipped": "this client declares no effect here"}, shared
        target = (effect_root / wanted["path"]).resolve()
        # Refuse to look outside the client's OWN project: a path that escaped
        # it would be reading another client's evidence, and with both clients
        # driven through the same journey that is exactly the mistake that
        # would let one side's work satisfy the other's assertion.
        if effect_root.resolve() not in target.parents:
            raise HarnessError(
                f"[{label}] effect path `{wanted['path']}` escapes the client's project root"
            )
        polls = int(step.get("polls", EXPECT_POLLS))
        contains = wanted.get("contains")
        absent = wanted.get("absent", False)
        # `missing` is a STRICTER absence than `absent`: it claims no file was
        # written at all, rather than that the file does not carry this marker.
        # They are different assertions and a refusal that wrote DIFFERENT bytes
        # must not satisfy "nothing was written".
        missing_only = wanted.get("missing", False)
        limit = int(wanted.get("limit", EFFECT_BYTE_LIMIT))
        for _ in range(polls):
            state, body = read_effect(target, limit)
            # REFUSE UNREADABLE EVIDENCE FIRST, before either predicate.
            #
            # This ordering is the correction: an earlier revision evaluated the
            # absence predicate first, and since an unreadable read yields an
            # empty body, `contains not in ""` was true — so corrupt, oversized
            # or non-UTF-8 evidence PASSED a marker-absence claim. Evidence this
            # runner cannot honestly read is a refusal in either direction.
            if state in ("oversized", "unreadable", "invalid"):
                raise HarnessError(
                    f"[{label}] the effect at `{wanted['path']}` is {state} "
                    f"(limit {limit} bytes) and cannot be used as evidence "
                    f"for or against a claim"
                )
            if missing_only:
                if state == "missing":
                    return "effect", {"path": str(target), "missing": True}, shared
            elif absent:
                if state == "missing" or (contains is not None and contains not in body):
                    return (
                        "effect",
                        {"path": str(target), "state": state, "absent": contains},
                        shared,
                    )
            elif state == "read" and (contains is None or contains in body):
                return (
                    "effect",
                    {
                        "path": str(target),
                        "contains": contains,
                        "bytes": target.stat().st_size,
                        "observed": True,
                    },
                    shared,
                )
            time.sleep(EXPECT_SLEEP)
        state, _ = read_effect(target, limit)
        if missing_only:
            detail = f"exists ({state}) but the claim was that nothing was written"
        elif absent:
            detail = "still carried the text it was asserted not to carry"
        else:
            detail = {
                "missing": "was never created",
                "read": "never carried the expected text",
            }.get(state, state)
        raise HarnessError(
            f"[{label}] the effect at `{wanted['path']}` {detail} after {polls} polls "
            f"({round(polls * EXPECT_SLEEP, 1)}s of bounded waiting)"
        )
    if "expect" in step:
        wanted, shared = resolve(step["expect"], label)
        if wanted is None:
            return "expect", {"skipped": "this client declares no expectation here"}, shared
        polls = int(step.get("polls", EXPECT_POLLS))
        # How many times the text must be on screen, PER CLIENT.
        #
        # A composer echoes the prompt that was typed into it, so waiting for a
        # word the prompt itself contains is satisfied before the product has
        # done anything. Where a client draws no marker that distinguishes its
        # transcript from its composer, waiting for the SECOND occurrence is
        # what separates the reply from the echo.
        #
        # That reason is a property of ONE client's screen, so the count is
        # client specific exactly as the expectation is. Sharing it measured a
        # falsehood: a client that DOES mark its transcript prints the awaited
        # text once, so a shared `times: 2` could never be satisfied no matter
        # how correctly that client behaved, and a real reply on screen was
        # reported as a timeout.
        wanted_times, times_shared = resolve(step.get("times", 1), label)
        times = 1 if wanted_times is None else int(wanted_times)
        shared = shared and times_shared
        for _ in range(polls):
            if screen_text(observer.state()).count(wanted) >= times:
                return "expect", {"text": wanted, "times": times, "observed": True}, shared
            time.sleep(EXPECT_SLEEP)
        raise HarnessError(
            f"[{label}] `{wanted}` never appeared {times} time(s) within {polls} polls "
            f"({round(polls * EXPECT_SLEEP, 1)}s of bounded waiting)"
        )
    raise HarnessError(f"[{label}] unrecognised step {step!r}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scenario", required=True, type=Path)
    parser.add_argument("--native", required=True, type=Path)
    parser.add_argument("--service-root", required=True, type=Path)
    # WHERE THE DURABLE STORE ACTUALLY IS, when that is not where the clients
    # are pointed.
    #
    # The reconnect run hands clients a service root that names a RELAY rather
    # than the product, holding only the two files a client reads. Snapshotting
    # sessions there read a directory with no `database.sqlite3` at all, so
    # both snapshots came back None and every reconnect receipt recorded NO
    # session binding -- not because the binding was unavailable, but because
    # it was being taken against the wrong directory.
    #
    # Defaults to the service root, so an ordinary run is unchanged.
    parser.add_argument("--store-root", type=Path)
    parser.add_argument("--ts-root", required=True, type=Path)
    parser.add_argument(
        "--bun",
        type=Path,
        required=True,
    )
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument(
        "--harness",
        type=Path,
        required=True,
    )
    parser.add_argument(
        "--project-config",
        type=Path,
        help="a config file copied into the retained client's launch directory "
        "before it starts. MCP servers are per-project, and that directory is "
        "the client's project.",
    )
    parser.add_argument(
        "--only",
        choices=["native", "retained"],
        help="run one side only; the receipt says which side is missing and why",
    )
    arguments = parser.parse_args()

    if arguments.evidence.exists():
        raise HarnessError(
            f"{arguments.evidence} already exists; a comparison is measured once, and reusing a "
            "directory would let one side's timings describe another run"
        )
    scenario = json.loads(arguments.scenario.read_text(encoding="utf-8"))
    steps = scenario.get("steps")
    if not isinstance(steps, list) or not steps:
        raise HarnessError(f"{arguments.scenario} declares no steps")

    store_root = arguments.store_root or arguments.service_root
    endpoint = read_endpoint(arguments.service_root)
    _, secret = credential(arguments.service_root)
    arguments.evidence.mkdir(parents=True)

    sides = []
    if arguments.only != "retained":
        run = arguments.evidence / "native"
        sides.append(
            run_client(
                "native",
                native_launch(arguments.native, arguments.service_root, run, run / "artifacts"),
                steps,
                run,
                arguments.harness,
                # The native client's session is placed in the project it
                # selects from the service catalog, and the service root is
                # itself a git boundary (`service.py` inits one), so that is
                # the project a tool writes into. It is passed EXPLICITLY and
                # recorded, rather than being inferred inside a step.
                arguments.service_root,
                store_root,
            )
        )
    if arguments.only != "native":
        run = arguments.evidence / "retained"
        sides.append(
            run_client(
                "retained",
                retained_launch(
                    arguments.bun,
                    arguments.ts_root,
                    endpoint,
                    secret,
                    run,
                    run / "artifacts",
                    arguments.project_config,
                ),
                steps,
                run,
                arguments.harness,
                # The retained client is handed this directory and resolves its
                # project from it, so its own launch boundary IS its project.
                run,
                store_root,
            )
        )

    receipt = {
        "contract": "native-tui-comparison/v1",
        "scenario": {
            "path": str(arguments.scenario.resolve()),
            "sha256": sha256(arguments.scenario),
            "name": scenario.get("name"),
        },
        "product": {"endpoint": endpoint, "service_root": str(arguments.service_root.resolve())},
        "native_binary": {
            "path": str(arguments.native.resolve()),
            "sha256": sha256(arguments.native),
        },
        "retained_source": str(arguments.ts_root.resolve()),
        "timing_note": (
            "Every step is timed by the runner with a monotonic clock outside both clients, so "
            "each figure is end to end: browser keyboard, PTY, client, product and repaint. It is "
            "the right quantity for a felt comparison and the wrong one for attributing a "
            "difference to one layer. `expect` steps are bounded polls, so their resolution is the "
            "poll interval and their figure is an upper bound."
        ),
        "sides": sides,
    }
    path = arguments.evidence / "comparison.json"
    path.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(json.dumps({"comparison": str(path), "failures": [s["failure"] for s in sides if s["failure"]]}, indent=2))
    return 1 if any(side["failure"] for side in sides) else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except HarnessError as error:
        print(f"native-tui comparison: {error}", file=sys.stderr)
        sys.exit(2)
