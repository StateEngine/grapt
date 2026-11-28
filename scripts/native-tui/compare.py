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
import base64
import json
import sqlite3
import urllib.request
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

# How much of a child's own output a diagnostic carries back. Bounded because it
# goes into an exception message, and kept from BOTH ENDS -- see `bounded`.
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

    `communicate()` cannot supply this: these children write to FILES so that
    nothing blocks on an undrained pipe, and `communicate()` on a file-backed
    child returns `(None, None)`.

    ABSENT, UNREADABLE AND EMPTY ARE REPORTED AS THEMSELVES. A diagnostic that
    could not be read must not arrive as an empty string, because the caller
    raises it as the reason a run stopped — and "the observer said nothing" and
    "nobody could read what the observer said" are different facts that lead to
    different next steps.
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


EXPECT_POLLS = 240
# How many times a capture is retaken while the screen is still moving.
CAPTURE_SETTLE_ATTEMPTS = 12
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
    bun: Path, ts_root: Path, endpoint: str, secret: str, run: Path, artifacts: Path
) -> dict:
    profile = run / "profile"
    launch_boundary(run)
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



def incoherent_captures(steps: list[dict]) -> list[str]:
    """Captures the observer itself said had not settled.

    Separate so it can be witnessed. The observer reports `coherent` and
    `quiescent` per screenshot and the pass condition ignored both, so a frame
    the harness KNEW was mid-render was captioned as a settled answer and the
    run reported success.
    """
    return [
        step["detail"].get("label")
        for step in steps
        if step.get("kind") == "capture"
        and isinstance(step.get("detail"), dict)
        and step["detail"].get("coherent") is False
    ]


def execution_failures(service_root: Path, sessions: list[str] | None) -> dict:
    """What the PRODUCT recorded about the sessions this client created.

    A comparison that reads only the screen can pass while every session it
    made ended in a failure the product itself wrote down — B3,
    measured: four sessions, four `session.execution.failed` rows, and
    `failures: [null, null]`. The screen is not the whole record, so this asks
    the record.

    It reads the durable log directly, and it distinguishes "could not read"
    from "nothing there": a store that cannot be opened yields no verdict
    rather than a clean one.
    """
    if sessions is None:
        return {"observed": False, "reason": "this client's sessions could not be bound"}
    database = service_root / "database.sqlite3"
    if not database.is_file():
        return {"observed": False, "reason": f"no durable store at {database}"}
    try:
        connection = sqlite3.connect(f"file:{database.as_posix()}?mode=ro", uri=True)
    except sqlite3.Error as error:
        return {"observed": False, "reason": f"the durable store could not be opened: {error}"}
    try:
        failed = {}
        for session in sessions:
            rows = connection.execute(
                "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session' "
                "AND aggregate_id = ?1 AND type = 'session.execution.failed.1'",
                (session,),
            ).fetchone()
            if rows and rows[0]:
                failed[session] = rows[0]
    except sqlite3.Error as error:
        return {"observed": False, "reason": f"the durable store could not be read: {error}"}
    finally:
        connection.close()
    return {"observed": True, "sessions": len(sessions), "failed": failed}


DEFAULT_T5_STREAMING_MIN_TOK_PER_S = 200.0


def measure_idle_cpu_delta(
    pid: int,
    window_seconds: float,
    sampler=None,
) -> float | None:
    """Measure CPU seconds consumed across a quiet idle window while the live app
    is running. Fails closed (returns None) if either sample is unobserved or if
    the process-tree counter decreased (never clamps negative deltas to zero)."""
    if window_seconds <= 0.0:
        return None
    probe = resources if sampler is None else sampler
    before = probe(pid)
    if (
        not before.get("observed")
        or before.get("processor_seconds") is None
        or int(before.get("processes_without_counters", 0) or 0) != 0
    ):
        return None
    time.sleep(window_seconds)
    after = probe(pid)
    if (
        not after.get("observed")
        or after.get("processor_seconds") is None
        or int(after.get("processes_without_counters", 0) or 0) != 0
    ):
        return None
    delta = float(after["processor_seconds"]) - float(before["processor_seconds"])
    if delta < 0.0:
        return None
    return round(delta, 6)


def is_quit_step(step: dict, label: str) -> bool:
    """True when `step` terminates the client so idle CPU is sampled BEFORE quit.
    Does not treat Control+c as quit because scenarios use Control+c to interrupt
    an active turn before Control+q exits."""
    if step.get("quit") is True:
        return True
    if "key" in step:
        key, _ = resolve(step["key"], label)
        if isinstance(key, str) and key.lower() in ("control+q", "ctrl+q"):
            return True
    return False


def validate_t5_scenario(scenario: dict, steps: list[dict]) -> None:
    """Refuse --enforce-t5-budgets on non-T5 scenarios that declare no streaming-echo probes."""
    has_echo = any(step.get("t5_echo") is True or "echo" in step for step in steps)
    rate = scenario.get("t5_streaming_tok_per_s")
    if rate is None:
        rates = [
            float(step["streaming_tok_per_s"])
            for step in steps
            if step.get("streaming_tok_per_s") is not None
        ]
        rate = min(rates) if rates else None
    if not has_echo or rate is None or float(rate) < DEFAULT_T5_STREAMING_MIN_TOK_PER_S:
        raise HarnessError(
            "--enforce-t5-budgets requires a T5 streaming-echo scenario declaring at least one "
            "t5_echo/echo step and streaming rate >= 200 tok/s"
        )


def run_client(
    label: str,
    launch: dict,
    steps: list[dict],
    run: Path,
    harness: Path,
    effect_root: Path,
    service_root: Path,
    idle_window_seconds: float = 0.0,
    stream_metrics_path: Path | None = None,
    stream_scope: str | None = None,
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

    # THE OBSERVER'S OUTPUT GOES TO FILES, NEVER TO A PIPE NOBODY READS.
    #
    # These were both `subprocess.PIPE`, and nothing drains them for the whole
    # life of the journey: the observer serves until the run ends. A pipe with
    # no reader fills — 4 KB on Windows — and the next write BLOCKS inside the
    # observer, which would stall the run at whatever step it happened to be on
    # and look like the client hanging. It is the same defect measured
    # as B3 in `service.py`, in three more places; the product one was found
    # because it corrupted evidence, these were found by reading for the class.
    #
    # Files also keep the observer's own diagnostics beside the run, which a
    # pipe discarded on every successful journey.
    out = (run / "observer-stdout.log").open("w", encoding="utf-8")
    err = (run / "observer-stderr.log").open("w", encoding="utf-8")
    server = subprocess.Popen(
        ["node", "server.mjs", str(launch_path.resolve())],
        cwd=staging,
        stdout=out,
        stderr=err,
        text=True,
    )
    # The parent's copies are closed at once: the child holds its own, and on
    # Windows a lingering handle makes the file undeletable.
    out.close()
    err.close()
    ready_path = Path(launch["artifacts"]) / "ready.json"
    ready = None
    started = time.monotonic()
    for _ in range(READY_POLLS):
        if server.poll() is not None:
            # Read from the files, not from `communicate()`, which returns
            # (None, None) for a file-backed child. `communicate()` is still
            # called to reap it.
            server.communicate()
            raise HarnessError(
                f"[{label}] the observer exited before ready:\n"
                + child_diagnostic(run, "observer")
            )
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
    idle_cpu_seconds = None
    try:
        for index, step in enumerate(steps, start=1):
            # Sample idle CPU while the live app sits idle BEFORE executing any quit step.
            if idle_cpu_seconds is None and idle_window_seconds > 0.0 and is_quit_step(step, label):
                idle_cpu_seconds = measure_idle_cpu_delta(int(ready["pid"]), idle_window_seconds)
            # Sampled BEFORE starting the step timer so PowerShell/OS resource
            # probe overhead never inflates step or echo latency.
            sample = resources(int(ready["pid"]))
            if sample.get("observed"):
                usage = sample
            at = time.monotonic()
            kind, detail, shared = apply_step(
                observer,
                step,
                label,
                effect_root,
                service_root,
                sessions_before,
                stream_metrics_path=stream_metrics_path,
                stream_scope=step.get("stream_scope") or stream_scope,
            )
            elapsed_s = time.monotonic() - at
            timing_row = {
                "step": index,
                "kind": kind,
                "detail": detail,
                # Only steps both clients received identically are
                # comparable as like for like; the rest are the navigation
                # each client needs to reach the same place.
                "shared": shared,
                "seconds": round(elapsed_s, 3),
                "elapsed_ms": round(elapsed_s * 1000.0, 3),
            }
            if step.get("t5_echo") is True or kind == "echo":
                timing_row["t5_echo"] = True
                if isinstance(detail, dict) and detail.get("skipped") is not None:
                    timing_row["skipped"] = True
                    timing_row["elapsed_ms"] = None
                else:
                    if isinstance(detail, dict) and detail.get("echo_ms") is not None:
                        timing_row["elapsed_ms"] = float(detail["echo_ms"])
                    if isinstance(detail, dict) and detail.get("streaming_observed") is True:
                        timing_row["streaming_observed"] = True
                        if detail.get("observed_tok_per_s") is not None:
                            timing_row["observed_tok_per_s"] = float(detail["observed_tok_per_s"])
                if step.get("streaming_tok_per_s") is not None:
                    timing_row["streaming_tok_per_s"] = float(step["streaming_tok_per_s"])
            timings.append(timing_row)
        if idle_cpu_seconds is None and idle_window_seconds > 0.0:
            idle_cpu_seconds = measure_idle_cpu_delta(int(ready["pid"]), idle_window_seconds)
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
    # THE DURABLE END STATE, not just the screen. A green comparison must be
    # able to say what the product recorded about the sessions it created.
    recorded = execution_failures(service_root, created)
    if failure is None and recorded.get("failed"):
        failure = (
            f"[{label}] the product recorded an execution failure on "
            f"{len(recorded['failed'])} of this client's {recorded['sessions']} session(s): "
            f"{recorded['failed']}"
        )
    # A CAPTURE THE OBSERVER CALLED INCOHERENT IS RECORDED AS ONE, ALWAYS.
    #
    # The observer reports `coherent`/`quiescent` per screenshot and the pass
    # condition ignored both, so a frame the harness KNEW was mid-render was
    # captioned as a settled answer and the run reported success. Every such
    # capture now appears in the receipt, per side, so a green run cannot hide
    # one.
    #
    # It fails the run only where the scenario DECLARED the frame is the
    # evidence (`"settled": true`). The retained client repaints continuously
    # in a live session — measured: twelve retakes over three seconds, never
    # quiescent — so an unconditional failure would make this comparison
    # unrunnable against it and would say nothing true about either client.
    # What carries a claim here is the WAIT that precedes the capture: `expect`,
    # `absent`, `effect` and the durable record. Captures illustrate.
    incoherent = incoherent_captures(timings)
    demanded = [
        step["detail"].get("label")
        for step in timings
        if step.get("kind") == "capture"
        and isinstance(step.get("detail"), dict)
        and step["detail"].get("settled_required")
        and step["detail"].get("coherent") is False
    ]
    if failure is None and demanded:
        failure = (
            f"[{label}] {len(demanded)} capture(s) declared `settled` never settled, so the "
            f"frames they photographed are not evidence of what they are captioned: {demanded}"
        )
    return {
        "client": label,
        "recorded_executions": recorded,
        "incoherent_captures": incoherent,
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
        "idle_cpu_seconds": idle_cpu_seconds,
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


def form_producer(service_root: Path, before: set[str] | None, declared: dict) -> dict:
    """Raise a real form on the session this client just opened.

    THE PRODUCER IS EXTERNAL TO BOTH CLIENTS, and deliberately so. A form comes
    from something that needs an answer — an MCP server's elicitation, a tool
    that cannot proceed without a choice — never from the client that displays
    it, and neither client here can mint one: `create` is the one form route the
    native client does not implement, because a client settles questions and
    does not ask them. So the question is posted through the product's own
    `POST /api/session/{id}/form` route, exactly as a producer would, and both
    clients meet it the way they would meet any other.

    The session is the one that appeared while this client was running. Naming
    it that way rather than creating one here matters: a form on a session the
    client is not looking at would be answered by nobody and would prove only
    that the route works.
    """
    endpoint = read_endpoint(service_root)
    username, secret = credential(service_root)
    token = base64.b64encode(f"{username}:{secret}".encode("utf-8")).decode("ascii")
    for _ in range(EXPECT_POLLS):
        after = sessions_in(service_root)
        fresh = sorted(after - before) if (after is not None and before is not None) else []
        if fresh:
            session = fresh[-1]
            body = json.dumps(declared).encode("utf-8")
            request = urllib.request.Request(
                f"{endpoint}/api/session/{session}/form",
                data=body,
                method="POST",
                headers={
                    "Authorization": f"Basic {token}",
                    "Content-Type": "application/json",
                },
            )
            with urllib.request.urlopen(request, timeout=30) as response:
                raised = json.loads(response.read().decode("utf-8"))
            return {
                "session": session,
                "form": raised["data"]["id"],
                "title": raised["data"]["title"],
                "producer": "external, through the product's own form route",
            }
        time.sleep(EXPECT_SLEEP)
    raise HarnessError(
        f"no session appeared for this client within "
        f"{round(EXPECT_POLLS * EXPECT_SLEEP, 1)}s, so a form had nowhere to be raised"
    )


def _read_stream_tokens(
    state: object,
    stream_metrics_path: Path | None,
    stream_scope: str | None = None,
) -> float | None:
    """Read genuine assistant stream token count from the observer /state payload
    or the staged loopback provider's scoped stream-metrics counter file. Never
    reads a shared global counter across unrelated sockets."""
    if isinstance(state, dict) and isinstance(state.get("stream_tokens"), (int, float)):
        return float(state["stream_tokens"])
    if stream_metrics_path is not None and stream_metrics_path.is_file() and stream_scope:
        try:
            payload = json.loads(stream_metrics_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return None
        if isinstance(payload, dict) and isinstance(payload.get("scopes"), dict):
            scoped = payload["scopes"].get(stream_scope, 0)
            if isinstance(scoped, (int, float)):
                return float(scoped)
    return None


def apply_step(
    observer: Observer,
    step: dict,
    label: str,
    effect_root: Path,
    service_root: Path,
    sessions_before: set[str] | None,
    stream_metrics_path: Path | None = None,
    stream_scope: str | None = None,
) -> tuple[str, object, bool]:
    if "echo" in step:
        text, shared = resolve(step["echo"], label)
        if text is None:
            return "echo", {"skipped": "this client does not take this echo probe"}, shared
        expect_text = step.get("expect", text)
        before_state = observer.state()
        before_screen = screen_text(before_state)
        if expect_text in before_screen:
            raise HarnessError(
                f"[{label}] composer echo marker `{expect_text}` was already present on screen before input"
            )
        scope = step.get("stream_scope") or stream_scope
        before_tokens = _read_stream_tokens(before_state, stream_metrics_path, scope)
        echo_start = time.monotonic()
        observer.action({"kind": "paste", "text": text})
        polls = int(step.get("polls", EXPECT_POLLS))
        for _ in range(polls):
            after_state = observer.state()
            after_screen = screen_text(after_state)
            if expect_text in after_screen:
                elapsed_s = max(time.monotonic() - echo_start, 1e-6)
                echo_ms = round(elapsed_s * 1000.0, 3)
                after_tokens = _read_stream_tokens(after_state, stream_metrics_path, scope)
                # Populate concurrent streaming ONLY from genuine assistant stream
                # token counters; never infer streaming from whole-screen text length,
                # which would miscount static status repaints or pasted composer text.
                if before_tokens is not None and after_tokens is not None:
                    delta_tokens = max(0.0, after_tokens - before_tokens)
                else:
                    delta_tokens = 0.0
                streaming_observed = delta_tokens > 0.0
                observed_tok_per_s = (
                    round(delta_tokens / elapsed_s, 3) if streaming_observed else 0.0
                )
                return "echo", {
                    "text": text,
                    "observed": True,
                    "absent_before": True,
                    "echo_ms": echo_ms,
                    "streaming_observed": streaming_observed,
                    "observed_tok_per_s": observed_tok_per_s,
                    "stream_delta_tokens": round(delta_tokens, 3),
                }, shared
            time.sleep(0.002)
        raise HarnessError(
            f"[{label}] composer echo `{expect_text}` never rendered within {polls} polls"
        )
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
        # A CAPTURE IS RETAKEN UNTIL THE SCREEN SETTLES.
        #
        # The observer reports whether the frame it grabbed had stopped
        # changing, and a single shot taken the instant an expectation matched
        # routinely catches a client mid-repaint — a streamed answer still
        # arriving, or a status line that animates. Retaking is what a person
        # does, and it turns "the screen was busy" into either a settled frame
        # or an honest failure, rather than into a photograph captioned as
        # something it does not show.
        attempts = int(step.get("settle_attempts", CAPTURE_SETTLE_ATTEMPTS))
        shot = observer.screenshot()
        taken = 1
        while taken < attempts and shot.get("capture", {}).get("coherent") is False:
            time.sleep(EXPECT_SLEEP)
            shot = observer.screenshot()
            taken += 1
        capture = shot.get("capture", {})
        return (
            "capture",
            {
                "label": step["capture"],
                "path": shot.get("path"),
                "coherent": capture.get("coherent"),
                "quiescent": capture.get("quiescent"),
                "attempts": taken,
                # PER CLIENT, like every other declaration in a step: the
                # native frame is the evidence for its own claim, while the
                # retained client repaints continuously and its frame is an
                # illustration beside a wait that carries the proof.
                "settled_required": bool(resolve(step.get("settled", False), label)[0]),
            },
            True,
        )
    if "form" in step:
        # NOT resolved per client. Every other step's mapping selects what THIS
        # client receives; a form payload is a mapping because a form has a
        # title and fields, and running it through that convention read
        # `{"title": ...}` as a client map with no entry for either side and
        # skipped the step on both. The same question is raised whichever
        # client is running, which is exactly what makes the two sides
        # comparable here, so this value is shared by construction.
        declared = step["form"]
        if not isinstance(declared, dict):
            raise HarnessError(f"[{label}] a form step declares an object, not {type(declared)}")
        return "form", form_producer(service_root, sessions_before, declared), True
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


DEFAULT_T5_ECHO_P95_MAX_MS = 16.0
DEFAULT_T5_IDLE_CPU_MAX_SECONDS = 0.5


def _percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, min(len(ordered) - 1, int((len(ordered) * fraction) + 0.999999999) - 1))
    return ordered[index]


def _witnessed_streaming_rate(native: dict, steps: list[dict], t5_metrics: dict) -> float | None:
    """Return observed concurrent streaming rate (tok/s) only from witnessed measurements.
    Never treats a static declared scenario `streaming_tok_per_s` as witnessed streaming."""
    witness = native.get("streaming_witness")
    if not isinstance(witness, dict):
        witness = t5_metrics.get("streaming_witness")
    if isinstance(witness, dict) and witness.get("observed") is True and witness.get("tok_per_s") is not None:
        return float(witness["tok_per_s"])
    observed_rates: list[float] = []
    for step in steps:
        if not (step.get("t5_echo") is True or step.get("kind") == "echo"):
            continue
        detail = step.get("detail") if isinstance(step.get("detail"), dict) else {}
        if step.get("skipped") is True or detail.get("skipped") is not None:
            continue
        if step.get("streaming_observed") is True and step.get("observed_tok_per_s") is not None:
            observed_rates.append(float(step["observed_tok_per_s"]))
        elif detail.get("streaming_observed") is True and detail.get("observed_tok_per_s") is not None:
            observed_rates.append(float(detail["observed_tok_per_s"]))
    return min(observed_rates) if observed_rates else None


def evaluate_t5_budgets(
    sides: list[dict],
    echo_p95_max_ms: float = DEFAULT_T5_ECHO_P95_MAX_MS,
    idle_cpu_max_seconds: float = DEFAULT_T5_IDLE_CPU_MAX_SECONDS,
    streaming_min_tok_per_s: float = DEFAULT_T5_STREAMING_MIN_TOK_PER_S,
) -> dict:
    """Evaluate T5 smoothness budgets (streaming echo p95 <= 16 ms, witnessed concurrent
    streaming >= 200 tok/s, true idle CPU ceiling) against explicit T5 measurements on the
    native client. Fails closed when any tagged echo step was skipped, never substitutes
    cumulative journey CPU for idle CPU, and never reports pass when any metric is unmeasured."""
    native = next((side for side in sides if side.get("client") == "native"), None)
    if native is None:
        return {"status": "not-measured", "reason": "native-side-absent"}
    steps = native.get("steps") or []
    tagged_echo_steps = [
        step for step in steps if step.get("t5_echo") is True or step.get("kind") == "echo"
    ]
    skipped_echo_steps = [
        step
        for step in tagged_echo_steps
        if step.get("skipped") is True
        or (isinstance(step.get("detail"), dict) and step["detail"].get("skipped") is not None)
    ]
    echo_samples = [
        float(step["elapsed_ms"])
        for step in tagged_echo_steps
        if step.get("elapsed_ms") is not None
        and step.get("skipped") is not True
        and not (isinstance(step.get("detail"), dict) and step["detail"].get("skipped") is not None)
    ]
    echo_p95 = _percentile(echo_samples, 0.95)
    if skipped_echo_steps:
        echo_status = "fail"
        echo_reason = "t5-echo-step-skipped"
    elif echo_p95 is None:
        echo_status = "not-measured"
        echo_reason = None
    else:
        echo_status = "pass" if echo_p95 <= echo_p95_max_ms else "fail"
        echo_reason = None
    # Only accept an explicit idle-window CPU measurement; cumulative journey
    # `resources.processor_seconds` is active work, never idle CPU.
    t5_metrics = native.get("t5_metrics") if isinstance(native.get("t5_metrics"), dict) else {}
    idle_cpu_seconds = native.get("idle_cpu_seconds")
    if idle_cpu_seconds is None:
        idle_cpu_seconds = t5_metrics.get("idle_cpu_seconds")
    idle_status = (
        "not-measured"
        if idle_cpu_seconds is None
        else ("pass" if float(idle_cpu_seconds) <= idle_cpu_max_seconds else "fail")
    )
    witnessed_tok_per_s = _witnessed_streaming_rate(native, steps, t5_metrics)
    streaming_status = (
        "not-measured"
        if witnessed_tok_per_s is None
        else ("pass" if witnessed_tok_per_s >= streaming_min_tok_per_s else "fail")
    )
    if echo_status == "fail" or idle_status == "fail" or streaming_status == "fail":
        overall = "fail"
    elif echo_status == "pass" and idle_status == "pass" and streaming_status == "pass":
        overall = "pass"
    else:
        overall = "not-measured"
    echo_report = {
        "status": echo_status,
        "observed": None if echo_p95 is None else round(echo_p95, 3),
        "budget": echo_p95_max_ms,
        "samples": len(echo_samples),
        "skipped_samples": len(skipped_echo_steps),
    }
    if echo_reason is not None:
        echo_report["reason"] = echo_reason
    return {
        "status": overall,
        "echo_p95_ms": echo_report,
        "concurrent_streaming_tok_per_s": {
            "status": streaming_status,
            "observed": None if witnessed_tok_per_s is None else round(witnessed_tok_per_s, 3),
            "budget": streaming_min_tok_per_s,
        },
        "idle_cpu_seconds": {
            "status": idle_status,
            "observed": None if idle_cpu_seconds is None else round(float(idle_cpu_seconds), 3),
            "budget": idle_cpu_max_seconds,
        },
    }


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
        "--only",
        choices=["native", "retained"],
        help="run one side only; the receipt says which side is missing and why",
    )
    parser.add_argument(
        "--echo-p95-max-ms",
        type=float,
        default=DEFAULT_T5_ECHO_P95_MAX_MS,
        help="T5 streaming/echo p95 latency budget in milliseconds (default: 16.0)",
    )
    parser.add_argument(
        "--idle-cpu-max-seconds",
        type=float,
        default=DEFAULT_T5_IDLE_CPU_MAX_SECONDS,
        help="T5 idle CPU processor-seconds ceiling (default: 0.5)",
    )
    parser.add_argument(
        "--t5-idle-window-seconds",
        type=float,
        default=None,
        help="quiet idle window (seconds) sampled after steps to measure idle_cpu_seconds",
    )
    parser.add_argument(
        "--stream-metrics",
        type=Path,
        default=None,
        help="path to the live loopback-provider scoped stream-tokens JSON counter for T5 streaming witness",
    )
    parser.add_argument(
        "--stream-scope",
        type=str,
        default=None,
        help="session/turn scope key attributing loopback-provider stream tokens to the measured terminal",
    )
    parser.add_argument(
        "--enforce-t5-budgets",
        action="store_true",
        help="exit nonzero if T5 smoothness budgets fail or are unmeasured",
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
    if arguments.enforce_t5_budgets:
        validate_t5_scenario(scenario, steps)

    store_root = arguments.store_root or arguments.service_root
    endpoint = read_endpoint(arguments.service_root)
    _, secret = credential(arguments.service_root)
    arguments.evidence.mkdir(parents=True)

    idle_window = (
        arguments.t5_idle_window_seconds
        if arguments.t5_idle_window_seconds is not None
        else (1.0 if arguments.enforce_t5_budgets else 0.0)
    )
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
                idle_window_seconds=idle_window,
                stream_metrics_path=arguments.stream_metrics,
                stream_scope=arguments.stream_scope,
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
        # DECLARED ASYMMETRY, carried rather than inferred. A dimension that
        # exercises one client is not a comparison, and a receipt that prints
        # two symmetric rows for it invites a reader to believe both clients
        # did the journey.
        "comparison": scenario.get("comparison", "both-clients"),
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
        "t5_budgets": evaluate_t5_budgets(
            sides,
            echo_p95_max_ms=arguments.echo_p95_max_ms,
            idle_cpu_max_seconds=arguments.idle_cpu_max_seconds,
        ),
    }
    path = arguments.evidence / "comparison.json"
    path.write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(
        json.dumps(
            {
                "comparison": str(path),
                "failures": [s["failure"] for s in sides],
                "t5_budgets": receipt["t5_budgets"]["status"],
            },
            indent=2,
        )
    )
    t5_failed = arguments.enforce_t5_budgets and receipt["t5_budgets"]["status"] != "pass"
    return 1 if (any(side["failure"] for side in sides) or t5_failed) else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except HarnessError as error:
        print(f"native-tui comparison: {error}", file=sys.stderr)
        sys.exit(2)
