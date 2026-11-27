#!/usr/bin/env python3
"""Execution health for one seat, from its service logs and session database.

Measurement only: the session database is opened strictly read-only and the
rotated service logs are scanned; nothing is written anywhere. The unit of
the verdict is the execution: the session.execution.* events carry an
executionID, so no pairing guess is needed.

The verdict is decided per execution by exec_verdict(), which has a single
HEALTHY path: a recognised success terminal (a user's own interrupt is
ruled not a failure and does not block health). A failure terminal, a
lost-work interrupt, or any unrecognised terminal kind or version is
FAILED. A started execution with no terminal is UNKNOWN while it may still
be running (started within the last IN_PROGRESS_MINUTES of the window, or
with service activity after the start) and FAILED as stranded otherwise -
which is how process loss shows up, since recovery writes no execution
terminal. An input is consumed only when it was promoted and its promotion
went into an execution: the execution running in the input's session at
the promotion, or one starting after it (a steer joins a live execution).
An admitted input without such a promotion, or a promoted input with no
execution, is accepted-never-run, and older than the in-progress threshold
it is FAILED as stranded. Step-level failures are detail, never counted as
executions, so no mirror deduplication is needed. The window is HEALTHY
only when every execution and every admitted input is healthy and no
unrecognised execution terminal exists anywhere in the window, even beside
a success.

The execution family is read in full and parsed strictly: a type is
recognised only when it spells session.execution.<kind>.<version> with a
known kind and version exactly; anything else - a missing version, a
leading zero, an unknown kind or version - is an unrecognised execution
terminal that fails the window, while unrecognised session.step.* types
are shown as detail only, and all of their rows are read within the same
bounds as the recognised ones. An execution-level event without a usable
executionID, or an input event without a usable inputID, is unproven
evidence: it is never counted as healthy, so the window is at best
UNKNOWN.

Event queries are two-stage: stage 1 projects the id and the payload
size in bytes only, checking the row and payload budgets before any
payload bytes are allocated; stage 2 fetches data only for rows that
fit the budget.

Exit codes:
  0  healthy: telemetry is live and every execution and admitted input is
      healthy
  1  failures: an execution failed, was lost or stranded, an admitted
      input never ran, an unrecognised execution terminal was seen, or
      the service is not running
  2  unknown: telemetry liveness could not be established, an execution or
      input has no recognised terminal yet, or the evidence was truncated
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

DEFAULT_WINDOW_HOURS = 6.0
DEFAULT_TOP_CLASSES = 10
DEFAULT_MIN_REPEATS = 3
SERVER_NAMES = ("grapt-server", "secode-server")
MANIFEST_NAME = "release-manifest.json"
MANIFEST_CAP_BYTES = 1024 * 1024
EXECUTABLE_CAP_BYTES = 1024 * 1024 * 1024
MAX_LOG_BYTES = 64 * 1024 * 1024
LOG_CHUNK_BYTES = 1024 * 1024
MAX_EVENT_ROWS = 10_000
MAX_PAYLOAD_BYTES = 16 * 1024 * 1024
PROGRESS_INSTRUCTIONS = 5000
MAX_DB_INSTRUCTIONS = 40_000_000
MAX_DB_SECONDS = 30.0
IN_PROGRESS_MINUTES = 5.0

TURN_HEALTHY = "HEALTHY"
TURN_FAILED = "FAILED"
TURN_UNKNOWN = "UNKNOWN"

EPOCH_MS = re.compile(r"[A-Za-z_]*ms=(\d{13})")
STEP_FAILED = re.compile(
    r"^step-failed session=(\S+) step=\d+ reason=(.+?) detail=(.*)$")
REASON_DEBUG = re.compile(r"^(\w+) \{ (\w+): (.+) \}$")
WRAPPED = re.compile(r'^\w+\("(.*)"\)$')

DB_TYPES = {
    "admitted": "session.input.admitted.1",
    "step_failed": "session.step.failed.3",
    "step_lost": "session.step.failed.2",
}

# The execution family is read in full and each type parsed strictly:
# a recognised kind with a recognised version, nothing else.
EXECUTION_EVENT = re.compile(
    r"^session\.execution\.([a-z_]+)\.([1-9][0-9]*)\Z")
RECOGNIZED_EXECUTIONS = {
    "started": (1,),
    "succeeded": (1,),
    "failed": (1,),
    "interrupted": (1,),
}

# Step-level kinds and versions (after the session.step. prefix), shown
# as detail only.
STEP_EVENT = re.compile(r"^session\.step\.([a-z_]+)\.([1-9][0-9]*)\Z")
RECOGNIZED_STEPS = {
    "failed": (2, 3),
}

# Detail phrases that name a distinct brick class inside a broader refusal.
DETAIL_KEYS = (
    ("read capacity is busy", "read-capacity-busy"),
    ("item or tool count bound", "item-tool-count-bound"),
)

INTERRUPTED_ALIASES = {"shutdown": "managed-shutdown"}

# Interrupt reasons whose work is lost and nothing resumes today; they
# count in the failures verdict as their own interrupted-by-restart class.
LOST_WORK_REASONS = ("managed-shutdown", "process-loss")


def kebab(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "-", name).lower()


def slug(text: str, limit: int = 48) -> str:
    words = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
    return words[:limit].rstrip("-") or "unspecified"


def classify_reason(reason_debug: str, detail: str):
    """Normalize one step-failed reason into (class, interrupted)."""
    match = REASON_DEBUG.match(reason_debug.strip())
    if not match:
        return "unparsed-reason/" + slug(reason_debug), False
    kind, _field, value = match.groups()
    kind = kebab(kind)
    inner = WRAPPED.match(value).group(1) if WRAPPED.match(value) else kebab(value)
    interrupted = kind == "interrupted"
    if kind == "provider-failure" and inner == "local-refusal":
        for needle, key in DETAIL_KEYS:
            if needle in detail:
                return f"{kind}/{inner}/{key}", interrupted
        return f"{kind}/{inner}/" + slug(detail), interrupted
    return f"{kind}/{inner}", interrupted


def classify_db_reason(data: str):
    """Normalize a durable step-failed reason into (class, interrupted)."""
    try:
        reason = json.loads(data).get("reason") or {}
    except ValueError:
        return None, False
    kind = reason.get("kind")
    effect = reason.get("effect")
    inner = (reason.get("class") or reason.get("failure")
             or reason.get("policy") or reason.get("by")
             or reason.get("reason")
             or (effect.get("kind") if isinstance(effect, dict) else None))
    if not kind:
        return None, False
    if not inner:
        inner = "unknown"
    kind = kebab(str(kind))
    return f"{kind}/{kebab(str(inner))}", kind == "interrupted"


def error_slug(data: str) -> str:
    """A short class for an execution-level failure, from its error."""
    try:
        error = json.loads(data).get("error")
    except ValueError:
        return "unspecified"
    if not isinstance(error, dict):
        return "unspecified"
    text = str(error.get("message") or error.get("type") or "").strip()
    return slug(text) if text else "unspecified"


def ending_bucket(cls):
    """Route a normalized terminal class to failures/lost/user/unrec."""
    if cls.startswith("interrupted/"):
        inner = cls.split("/", 1)[1]
        if inner == "user":
            return "user"
        if inner in LOST_WORK_REASONS:
            return "lost"
        return "unrec"
    if cls.startswith("process-loss/"):
        return "lost"
    return "failures"


def ending_key(cls, bucket):
    """The display key for a terminal class in its bucket."""
    if bucket != "lost":
        return cls
    inner = cls.split("/", 1)[1] if "/" in cls else cls
    if inner in LOST_WORK_REASONS:
        return "interrupted-by-restart/" + inner
    return "interrupted-by-restart/process-loss"


BUCKETS = {
    "failures": "failures",
    "lost": "lost",
    "user": "user_interrupts",
    "unrec": "unknown_endings",
}


def record_ending(state, cls, session, epoch):
    """Count one terminal ending in the source state and per-session sums."""
    bucket = ending_bucket(cls)
    key = ending_key(cls, bucket)
    state[BUCKETS[bucket]][key] = state[BUCKETS[bucket]].get(key, 0) + 1
    if bucket == "failures" and session is not None:
        entry = state["per_session"].setdefault(session, [0, 0])
        entry[0] += 1
        entry[1] = max(entry[1], epoch)


def event_execution_id(data):
    """The executionID carried by an execution-level event, if any."""
    try:
        value = json.loads(data).get("executionID")
    except (ValueError, AttributeError):
        return None
    return value if isinstance(value, str) and value else None


def interrupted_terminal(reason):
    """Classify one execution-level interrupt reason.

    Returns (kind, class): lost work (managed-shutdown, process-loss)
    fails the execution under its interrupted-by-restart class; the
    user's own interrupt is not a failure; every other reason is
    unrecognised evidence and fails the execution conservatively rather
    than claiming a healthy ending.
    """
    reason = INTERRUPTED_ALIASES.get(reason, kebab(reason))
    if reason in LOST_WORK_REASONS:
        return "lost", "interrupted-by-restart/" + reason
    if reason == "user":
        return "user", "interrupted/user"
    return "unrec", "interrupted/" + reason


def pick_terminal(terminals):
    """The execution's deciding terminal, or None without one.

    Any unrecognised terminal decides, conservatively. Otherwise the worst
    recognised one decides: lost work and failure beat a success or a user
    interrupt, and a success beats a user interrupt (the work completed).
    """
    for kind in ("unrec", "lost", "failed", "success", "user"):
        for terminal in terminals:
            if terminal["kind"] == kind:
                return terminal
    return None


def exec_verdict(execution, recent_floor, log_last):
    """Decide one execution.

    The only HEALTHY paths are a recognised success terminal and the
    user's own interrupt (ruled not a failure on 2026-11-18). Every other
    path falls through to the end: FAILED for a recognised failure or
    lost-work terminal, for an unrecognised terminal, and for a started
    execution with no terminal older than IN_PROGRESS_MINUTES and no
    service activity since the start (stranded - how process loss shows
    up, since recovery writes no execution terminal); UNKNOWN while the
    execution may still be running.
    """
    terminal = pick_terminal(execution["terminals"])
    if terminal is not None:
        if terminal["kind"] in ("success", "user"):
            return TURN_HEALTHY
        return TURN_FAILED
    start = execution["start"]
    if start is None:
        return TURN_UNKNOWN
    if (start >= recent_floor
            or (log_last is not None and log_last >= start)):
        return TURN_UNKNOWN
    return TURN_FAILED


def scan_logs(paths, lo_ms, hi_ms):
    """Count window lines and step failures; log epochs are attributed.

    Log lines carry no timestamp of their own, so each line inherits the
    last 13-digit millisecond epoch seen on or before it (the provider
    dispatch lines stamp the stream). The attribution is approximate by
    construction and is labelled as such in the report. At most
    MAX_LOG_BYTES of log are scanned; past the cap the evidence is
    incomplete and the verdict cannot be healthy.
    """
    state = {
        "lines_total": 0,
        "lines_in_window": 0,
        "unattributed": 0,
        "first": None,
        "last": None,
        "failures": {},
        "lost": {},
        "user_interrupts": {},
        "unknown_endings": {},
        "per_session": {},
        "sessions": {},
        "bytes_scanned": 0,
        "bytes_capped": False,
    }
    for path in paths:
        try:
            handle = open(path, "rb")
        except OSError:
            state.setdefault("unreadable_files", []).append(Path(path).name)
            continue
        current = None
        with handle:
            def process_line(line_text):
                nonlocal current
                state["lines_total"] += 1
                found = EPOCH_MS.search(line_text)
                if found:
                    current = int(found.group(1))
                if current is None:
                    state["unattributed"] += 1
                    return
                if state["first"] is None or current < state["first"]:
                    state["first"] = current
                if state["last"] is None or current > state["last"]:
                    state["last"] = current
                if not lo_ms <= current <= hi_ms:
                    return
                state["lines_in_window"] += 1
                match = STEP_FAILED.match(line_text)
                if not match:
                    return
                session, reason, detail = match.groups()
                cls, is_interrupted = classify_reason(reason, detail)
                if is_interrupted:
                    cls = "interrupted/" + cls.split("/", 1)[1]
                record_ending(state, cls, session, current)

            tail = b""
            while not state["bytes_capped"]:
                chunk = handle.read(LOG_CHUNK_BYTES)
                if not chunk:
                    if tail:
                        process_line(tail.decode("utf-8", "replace"))
                    break
                state["bytes_scanned"] += len(chunk)
                data = tail + chunk
                tail = b""
                if state["bytes_scanned"] >= MAX_LOG_BYTES:
                    state["bytes_capped"] = True
                cut = data.rfind(b"\n")
                if cut != -1:
                    tail = data[cut + 1:]
                    data = data[:cut]
                for raw in data.split(b"\n"):
                    process_line(raw.decode("utf-8", "replace"))
    return state


IN_CHUNK = 1000


def bounded_event_rows(con, from_where, params, need_payload, flag, budget):
    """Yield (aggregate_id, created, data, type) event rows in bounds.

    Stage 1 projects id, aggregate_id, created, type and the payload size
    in bytes (LENGTH of the BLOB form, so multibyte text is charged in
    bytes, not characters), so the row cap and the payload byte budget
    are checked before any payload bytes are allocated. Stage 2 fetches
    data only for rows whose length fits the remaining budget. Truncation
    is reported through the caller's flag dict; the payload budget is a
    dict {"left": bytes} charged per row in stage 1.
    """
    stage1 = []
    for row in con.execute(
            "SELECT id, aggregate_id, created, type, "
            "LENGTH(CAST(data AS BLOB)) " + from_where + " LIMIT ?",
            params + (MAX_EVENT_ROWS + 1,)):
        if len(stage1) >= MAX_EVENT_ROWS:
            flag["truncated"] = True
            return
        stage1.append(row)
    if not need_payload:
        for _event_id, aggregate_id, created, _type, _length in stage1:
            yield aggregate_id, created, None, _type
        return
    wanted = []
    for event_id, aggregate_id, created, type_name, length in stage1:
        length = length or 0
        if budget["left"] < length:
            flag["truncated"] = True
            return
        budget["left"] -= length
        wanted.append((event_id, aggregate_id, created, type_name))
    data = {}
    ids = [event_id for event_id, _aggregate_id, _created, _type in wanted]
    for offset in range(0, len(ids), IN_CHUNK):
        chunk = ids[offset:offset + IN_CHUNK]
        marks = ", ".join("?" for _ in chunk)
        for event_id, payload in con.execute(
                "SELECT id, data FROM event WHERE id IN ({})".format(marks),
                chunk):
            data[event_id] = payload
    for event_id, aggregate_id, created, type_name in wanted:
        yield aggregate_id, created, data.get(event_id, ""), type_name


def read_db(db_path, lo_ms, hi_ms):
    """Read the session database read-only; report failure as a fact.

    The path is percent-encoded into the file URI, so reserved characters
    cannot redirect or create a file. The unit of the verdict is the
    execution: the execution-level events carry their executionID, so no
    pairing guess is needed. Query work is bounded by an instruction and
    wall-clock budget, and every event read is two-stage: stage 1 projects
    the id and the payload size in bytes only, checking the row and
    payload budgets before any payload bytes are allocated; stage 2
    fetches data only for the rows that fit.
    """
    uri = Path(db_path).resolve().as_uri() + "?mode=ro"
    try:
        con = sqlite3.connect(uri, uri=True, timeout=2.0)
    except sqlite3.Error as error:
        return {"ok": False, "error": str(error)}
    budget = {"used": 0, "start": time.monotonic()}

    def progress():
        budget["used"] += PROGRESS_INSTRUCTIONS
        if (budget["used"] > MAX_DB_INSTRUCTIONS
                or time.monotonic() - budget["start"] > MAX_DB_SECONDS):
            return 1
        return 0

    flag = {"truncated": False}
    payload = {"left": MAX_PAYLOAD_BYTES}
    in_window = "AND created BETWEEN ? AND ?"

    def event_rows(where, params, need_payload):
        return bounded_event_rows(con, "FROM event " + where, params,
                                  need_payload, flag, payload)

    try:
        con.execute("PRAGMA query_only = ON")
        con.execute("PRAGMA busy_timeout = 2000")
        con.set_progress_handler(progress, PROGRESS_INSTRUCTIONS)
        executions = {}
        step_failures = {}
        unrec_exec = {}
        unrec_step = {}
        user_interrupts = {}
        admitted_total = 0
        malformed = 0
        unmatched_admitted = 0

        def execution(eid):
            entry = executions.get(eid)
            if entry is None:
                entry = executions[eid] = {"start": None, "session": None,
                                           "terminals": []}
            return entry

        def note_terminal(eid, kind, cls, epoch, session=None):
            entry = execution(eid)
            entry["terminals"].append(
                {"kind": kind, "cls": cls, "epoch": epoch})
            # A terminal carries its session, so an execution that starts
            # before the window is still named by its boundary terminal.
            if entry["session"] is None and session is not None:
                entry["session"] = session

        def payload_field(data, field):
            """A non-empty string field carried by an event payload."""
            try:
                value = json.loads(data).get(field)
            except (ValueError, AttributeError):
                return None
            return value if isinstance(value, str) and value else None

        def input_id(data):
            return payload_field(data, "inputID")

        def event_session(aggregate_id, data):
            """The session carrying the event: aggregate id, sessionID."""
            if aggregate_id is not None:
                return aggregate_id
            return payload_field(data, "sessionID")

        # The whole execution family is read at once and each type parsed
        # strictly: a recognised kind with a recognised version, nothing
        # else. A missing version, a leading zero, an unknown kind or an
        # unknown version is an unrecognised terminal that fails the
        # window, even beside a success.
        for aggregate_id, created, data, type_name in event_rows(
                "WHERE (type = ? OR type LIKE ?) " + in_window,
                ("session.execution", "session.execution.%", lo_ms, hi_ms),
                True):
            session = event_session(aggregate_id, data)
            match = EXECUTION_EVENT.match(type_name)
            kind = None
            if match:
                versions = RECOGNIZED_EXECUTIONS.get(match.group(1))
                if (versions is not None
                        and int(match.group(2)) in versions):
                    kind = match.group(1)
            eid = event_execution_id(data)
            if kind is None:
                key = "unrecognized/" + type_name
                note_terminal(eid or "unattributed", "unrec", key, created,
                              session)
                unrec_exec[key] = unrec_exec.get(key, 0) + 1
                continue
            # A recognised execution event without an executionID is
            # unproven evidence: it cannot be paired with any terminal,
            # so it is counted as malformed, not merged under a shared
            # identity.
            if eid is None:
                malformed += 1
                continue
            if kind == "started":
                entry = execution(eid)
                if entry["start"] is None or created < entry["start"]:
                    entry["start"] = created
                if entry["session"] is None:
                    entry["session"] = session
            elif kind == "succeeded":
                note_terminal(eid, "success", None, created, session)
            elif kind == "failed":
                cls = "execution-failed/" + error_slug(data or "")
                note_terminal(eid, "failed", cls, created, session)
            else:
                try:
                    reason = str(json.loads(data).get("reason")
                                 or "unspecified")
                except ValueError:
                    reason = "unspecified"
                kind, cls = interrupted_terminal(reason)
                note_terminal(eid, kind, cls, created, session)
                if kind == "user":
                    user_interrupts[cls] = user_interrupts.get(cls, 0) + 1

        promoted = []
        for aggregate_id, created, data, _type in event_rows(
                "WHERE type LIKE ? " + in_window,
                ("session.input.promoted.%", lo_ms, hi_ms), True):
            iid = input_id(data)
            if iid is None:
                # An input event without a usable inputID is unproven.
                unmatched_admitted += 1
                continue
            promoted.append((event_session(aggregate_id, data), created,
                             iid))
        promoted_ids = {input_id_ for _sid, _ts, input_id_ in promoted}

        never_run = []
        for aggregate_id, created, data, _type in event_rows(
                "WHERE type = ? " + in_window,
                (DB_TYPES["admitted"], lo_ms, hi_ms), True):
            admitted_total += 1
            iid = input_id(data)
            if iid is None:
                # An input event without a usable inputID is unproven.
                unmatched_admitted += 1
                continue
            if iid not in promoted_ids:
                # No promotion: the input never reached an execution.
                never_run.append(created)

        def promoted_reached(session, at_ms):
            """The execution the input went into: the one running in its
            session at the promotion, or starting after it. A promotion
            is not completion: it commits before the join, so a crash in
            between leaves a promoted input with no execution.
            """
            for entry in executions.values():
                if entry["session"] != session:
                    continue
                start = entry["start"]
                if start is not None and start >= at_ms:
                    return True
                if start is None or start <= at_ms:
                    if not entry["terminals"]:
                        return True
                    if any(terminal["epoch"] >= at_ms
                           for terminal in entry["terminals"]):
                        return True
            return False

        for session, at_ms, _iid in promoted:
            if not promoted_reached(session, at_ms):
                # Promoted with no execution started: accepted-never-run,
                # on the same clock as an unpromoted admission.
                never_run.append(at_ms)
        never_run.sort()

        for _aggregate_id, _created, data, _type in event_rows(
                "WHERE type IN (?, ?) " + in_window,
                (DB_TYPES["step_failed"], DB_TYPES["step_lost"], lo_ms,
                 hi_ms), True):
            cls, _is_interrupted = classify_db_reason(data or "")
            if cls is None:
                cls = "unparsed-reason/db"
            cls = ending_key(cls, ending_bucket(cls))
            step_failures[cls] = step_failures.get(cls, 0) + 1

        # Step-level types are discovered for the detail section; the
        # execution family was already read and parsed in full above.
        types = [row[0] for row in con.execute(
            "SELECT DISTINCT type FROM event WHERE created BETWEEN ? AND ?",
            (lo_ms, hi_ms)).fetchall()]
        for type_name in types:
            if not type_name.startswith("session.step."):
                continue
            match = STEP_EVENT.match(type_name)
            versions = (RECOGNIZED_STEPS.get(match.group(1))
                        if match else None)
            if (match is not None and versions is not None
                    and int(match.group(2)) in versions):
                continue
            key = "unrecognized/" + type_name
            for _aggregate_id, _created, _data, _type in event_rows(
                    "WHERE type = ? " + in_window,
                    (type_name, lo_ms, hi_ms), False):
                unrec_step[key] = unrec_step.get(key, 0) + 1

        exec_classes = {}
        brick = {}
        lost_sessions = {}
        for entry in executions.values():
            terminal = pick_terminal(entry["terminals"])
            if terminal is None:
                continue
            if terminal["kind"] in ("failed", "lost", "unrec"):
                cls = terminal["cls"]
                exec_classes[cls] = exec_classes.get(cls, 0) + 1
            if terminal["kind"] in ("failed", "unrec"):
                sid = entry["session"] or "unattributed"
                count, last = brick.get(sid, (0, 0))
                brick[sid] = [count + 1, max(last, terminal["epoch"])]
            elif terminal["kind"] == "lost":
                sid = entry["session"] or "unattributed"
                count, last = lost_sessions.get(sid, (0, 0))
                lost_sessions[sid] = [count + 1, max(last, terminal["epoch"])]

        return {
            "ok": True,
            "truncated": flag["truncated"],
            "counts": {
                "admitted": admitted_total,
                "started": sum(1 for entry in executions.values()
                               if entry["start"] is not None),
            },
            "executions": executions,
            "never_run": never_run,
            "step_failures": step_failures,
            "unrec_exec": unrec_exec,
            "unrec_step": unrec_step,
            "user_interrupts": user_interrupts,
            "exec_classes": exec_classes,
            "brick": brick,
            "lost_sessions": lost_sessions,
            "malformed": malformed,
            "unmatched_admitted": unmatched_admitted,
        }
    except sqlite3.Error as error:
        message = str(error)
        if (budget["used"] > MAX_DB_INSTRUCTIONS
                or time.monotonic() - budget["start"] > MAX_DB_SECONDS
                or "callback requested a stop" in message
                or message == "interrupted"):
            return {"ok": False, "error": "the query budget was exceeded "
                    "before the window could be read"}
        return {"ok": False, "error": message}
    finally:
        con.close()


def find_service(seat):
    """The lowest pid whose binary is the product server for this seat."""
    if not Path("/proc").is_dir():
        return None, None, "the process table is not available on this host"
    seat = str(Path(seat).resolve())
    candidate = None
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            exe = os.readlink("/proc/{}/exe".format(name))
            raw = Path("/proc/{}/cmdline".format(name)).read_bytes()
        except OSError:
            continue
        cmd = raw.replace(b"\0", b" ").decode("utf-8", "replace")
        if Path(exe).name in SERVER_NAMES and seat in cmd:
            if candidate is None or int(name) < candidate[0]:
                candidate = (int(name), exe)
    if candidate is None:
        return None, None, "no running product server names this seat"
    return candidate[0], candidate[1], None


def git(repo, args, timeout=30):
    try:
        return subprocess.run(
            ["git", "-C", str(repo), *args],
            capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired):
        return None


def resolve_main(repo):
    """A fresh main from a reachable remote, else the stale tracking refs."""
    for remote in ("github", "origin"):
        result = git(repo, ["ls-remote", remote, "refs/heads/main"], timeout=15)
        if result is not None and result.returncode == 0:
            head = result.stdout.strip().splitlines()
            if head and head[0].strip():
                return head[0].split()[0], remote + "/main", None
    for ref in ("refs/remotes/github/main", "refs/remotes/origin/main"):
        result = git(repo,
                     ["rev-parse", "--verify", "--quiet", ref + "^{commit}"],
                     timeout=10)
        if result is not None and result.returncode == 0:
            return result.stdout.strip(), ref.split("/")[-2] + "/main", \
                "main read from the stale tracking ref {} " \
                "(no remote reachable)".format(ref)
    return None, None, None


def attest_binary(exe, manifest):
    """The package identity contract: the manifest must attest these bytes.

    Returns (commit, problem). The manifest must carry a binaries entry
    for this exact executable whose sha256 matches the running bytes and
    whose source_commit equals engine_version; anything missing or
    incomplete is an unattested build, with no engine_version fallback.
    """
    binaries = manifest.get("binaries")
    entry = binaries.get(Path(exe).name) if isinstance(binaries, dict) else None
    if not isinstance(entry, dict):
        return None, "the release manifest does not attest this binary"
    source_commit = entry.get("source_commit")
    engine_version = manifest.get("engine_version")
    if not (isinstance(source_commit, str) and len(source_commit) == 40
            and isinstance(engine_version, str)
            and engine_version == source_commit):
        return None, "the manifest engine_version does not match this " \
                     "binary's source commit"
    try:
        if Path(exe).stat().st_size > EXECUTABLE_CAP_BYTES:
            return None, "the running executable exceeds the hash cap"
        digest = hashlib.sha256()
        with open(exe, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
    except OSError:
        return None, "the running executable cannot be read"
    expected = entry.get("sha256")
    if not isinstance(expected, str) or \
            digest.hexdigest().lower() != expected.lower():
        return None, "the running binary differs from the one " \
                     "release-manifest.json attests"
    return source_commit, None


def build_check(exe, repo):
    lines = []
    attested = None
    manifest = Path(exe).parent / MANIFEST_NAME
    if manifest.is_file():
        try:
            if manifest.stat().st_size > MANIFEST_CAP_BYTES:
                lines.append("  attested build: none (release manifest "
                             "exceeds the read cap)")
                return lines
            raw = manifest.read_bytes()
            manifest_obj = json.loads(raw)
        except (OSError, ValueError):
            manifest_obj = None
            lines.append("  attested build: release manifest unreadable")
        if manifest_obj is not None:
            attested, problem = attest_binary(exe, manifest_obj)
            if attested is None:
                lines.append("  attested build: none ({})".format(problem))
    if attested is None and not lines:
        lines.append("  attested build: none (unattested binary)")
    if attested is None:
        return lines
    lines.append("  attested build: {}".format(attested))
    if repo is None:
        lines.append("  main comparison: skipped (no --repo given)")
        return lines
    main_sha, ref, ref_note = resolve_main(repo)
    if main_sha is None:
        lines.append("  main comparison: no main ref reachable in the given "
                     "repo")
        return lines
    if ref_note:
        lines.append("  " + ref_note)
    present = git(repo, ["rev-parse", "--verify", "--quiet",
                         attested + "^{commit}"], timeout=10)
    if present is None or present.returncode != 0:
        lines.append("  {}: attested commit is not in this clone "
                     "(rewritten or stale history)".format(ref))
        return lines
    if attested == main_sha:
        lines.append("  {}: up to date with main".format(ref))
        return lines
    ancestor = git(repo, ["merge-base", "--is-ancestor", attested, main_sha])
    if ancestor is not None and ancestor.returncode == 0:
        count = git(repo, ["rev-list", "--count",
                           "{}..{}".format(attested, main_sha)])
        behind = count.stdout.strip() if count else "?"
        lines.append("  {}: {} commit(s) behind main ({})".format(
            ref, behind, main_sha[:12]))
    else:
        lines.append("  {}: not an ancestor of main ({}): an older build, a "
                     "side branch, or rewritten history".format(
                         ref, main_sha[:12]))
    return lines


def parse_when(text):
    if text is None:
        return time.time()
    if text.isdigit():
        value = int(text)
        return value / 1000.0 if value > 10_000_000_000 else float(value)
    return datetime.fromisoformat(text.replace("Z", "+00:00")).timestamp()


def iso(ms):
    return datetime.fromtimestamp(ms / 1000.0, tz=timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ")


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="Report turn health for one seat (read-only).",
        epilog="The seat directory may also come from the TURN_HEALTH_SEAT "
               "environment variable.")
    parser.add_argument("--seat", default=os.environ.get("TURN_HEALTH_SEAT"),
                        help="seat directory holding logs/ and database.sqlite3")
    parser.add_argument("--window-hours", type=float,
                        default=DEFAULT_WINDOW_HOURS,
                        help="window length in hours (default: %(default)s)")
    parser.add_argument("--until", default=None,
                        help="window end, ISO 8601 UTC or epoch s/ms (default: now)")
    parser.add_argument("--repo", default=None,
                        help="git checkout used to compare the attested build with main")
    parser.add_argument("--top", type=int, default=DEFAULT_TOP_CLASSES,
                        help="how many failure classes to list (default: %(default)s)")
    parser.add_argument("--min-repeats", type=int, default=DEFAULT_MIN_REPEATS,
                        help="failures in one session that mark it a suspected brick "
                             "(default: %(default)s)")
    parser.add_argument("--log-glob", default="server.log*",
                        help="log file pattern under logs/ (default: %(default)s)")
    args = parser.parse_args(argv)
    if not args.seat:
        parser.error("--seat (or TURN_HEALTH_SEAT) is required")
    if args.window_hours <= 0:
        parser.error("--window-hours must be positive")
    return args


def main(argv=None):
    nice = getattr(os, "nice", None)
    if nice is not None:
        try:
            nice(10)
        except (OSError, PermissionError):
            pass
    args = parse_args(argv)
    seat = Path(args.seat)
    if not seat.is_dir():
        print("turn-health: seat directory not found: {}".format(seat))
        print("verdict  UNKNOWN  (exit 2)")
        return 2

    until = parse_when(args.until)
    hi_ms = int(until * 1000)
    lo_ms = int((until - args.window_hours * 3600.0) * 1000)

    log_paths = sorted((seat / "logs").glob(args.log_glob))
    logs = scan_logs(log_paths, lo_ms, hi_ms)
    db_file = seat / "database.sqlite3"
    db = read_db(db_file, lo_ms, hi_ms) if db_file.is_file() else {
        "ok": False, "error": "absent"}
    pid, exe, service_error = find_service(seat)

    db_ok = db["ok"]
    counts = db["counts"] if db_ok else None
    state_rank = {TURN_HEALTHY: 0, TURN_UNKNOWN: 1, TURN_FAILED: 2}
    state = TURN_HEALTHY

    def escalate(candidate):
        nonlocal state
        if state_rank[candidate] > state_rank[state]:
            state = candidate

    success_exec = failed_exec = lost_exec = user_exec = 0
    stranded_exec = in_progress_exec = 0
    stranded_input = in_progress_input = 0
    unrec_exec_total = 0
    unproven = 0
    recent_floor = hi_ms - int(IN_PROGRESS_MINUTES * 60_000)
    if db_ok:
        for entry in db["executions"].values():
            verdict = exec_verdict(entry, recent_floor, logs["last"])
            escalate(verdict)
            terminal = pick_terminal(entry["terminals"])
            if terminal is None:
                if verdict == TURN_FAILED:
                    stranded_exec += 1
                else:
                    in_progress_exec += 1
            elif terminal["kind"] in ("failed", "unrec"):
                failed_exec += 1
            elif terminal["kind"] == "lost":
                lost_exec += 1
            elif terminal["kind"] == "user":
                user_exec += 1
            else:
                success_exec += 1
        for created in db["never_run"]:
            if created >= recent_floor:
                in_progress_input += 1
                escalate(TURN_UNKNOWN)
            else:
                stranded_input += 1
                escalate(TURN_FAILED)
        unrec_exec_total = sum(db["unrec_exec"].values())
        if unrec_exec_total:
            escalate(TURN_FAILED)
        unproven = db["malformed"] + db["unmatched_admitted"]
        if unproven:
            escalate(TURN_UNKNOWN)

    print("turn-health {}".format(seat.name))
    print("window  {} .. {} UTC ({} h)".format(
        iso(lo_ms), iso(hi_ms), args.window_hours))
    print("telemetry")
    names = ", ".join(p.name for p in log_paths) or "none found"
    print("  log files: {} ({})".format(len(log_paths), names))
    if logs.get("unreadable_files"):
        print("  unreadable log files: {}".format(
            ", ".join(logs["unreadable_files"])))
    last_attr = iso(logs["last"]) if logs["last"] is not None else "none"
    print("  lines in window: {} (last attributed line ~{}, unattributed: {})".format(
        logs["lines_in_window"], last_attr, logs["unattributed"]))
    if logs["bytes_capped"]:
        print("  log bytes capped at {:,} (evidence may be incomplete)".format(
            MAX_LOG_BYTES))
    if db_ok:
        if db["truncated"]:
            print("  database: {} (read-only, truncated after {} classified "
                  "rows)".format(db_file.name, MAX_EVENT_ROWS))
        else:
            print("  database: {} (read-only)".format(db_file.name))
    else:
        print("  database: {} ({})".format(db_file.name, db["error"]))
    print("executions")
    if db_ok:
        print("  started {}   succeeded {}   failed {}   lost {}   "
              "interrupted {}".format(
                  counts["started"], success_exec, failed_exec, lost_exec,
                  user_exec))
        print("  admitted {}   never ran {}   in progress {}   stranded "
              "{}".format(
                  counts["admitted"], len(db["never_run"]),
                  in_progress_exec + in_progress_input,
                  stranded_exec + stranded_input))
        if unproven:
            print("  unattributed {} (missing executionID or inputID; "
                  "evidence unproven)".format(unproven))
        if counts["started"]:
            print("  failure rate (failed or lost, /started): {:.1f}%".format(
                100.0 * (failed_exec + lost_exec) / counts["started"]))
    else:
        print("  unknown (database unreadable)")
    print("executions by class (log | db); step-level rows are detail")
    combined = {}
    if db_ok:
        for cls, n in db["exec_classes"].items():
            combined.setdefault(cls, [0, 0])[1] = n
    ranked = sorted(combined.items(), key=lambda kv: (-sum(kv[1]), kv[0]))
    if not ranked:
        print("  none")
    for cls, (log_n, db_n) in ranked[: args.top]:
        print("  {}  {} | {}".format(cls, log_n, db_n))
    if len(ranked) > args.top:
        print("  ... {} more class(es)".format(len(ranked) - args.top))
    print("step-failure detail (never counted as executions; log | db)")
    combined = {}
    for source in (logs["failures"], logs["lost"], logs["unknown_endings"]):
        for cls, n in source.items():
            combined.setdefault(cls, [0, 0])[0] += n
    if db_ok:
        for source in (db["step_failures"], db["unrec_step"]):
            for cls, n in source.items():
                combined.setdefault(cls, [0, 0])[1] += n
    ranked = sorted(combined.items(), key=lambda kv: (-sum(kv[1]), kv[0]))
    if not ranked:
        print("  none")
    for cls, (log_n, db_n) in ranked[: args.top]:
        print("  {}  {} | {}".format(cls, log_n, db_n))
    if len(ranked) > args.top:
        print("  ... {} more class(es)".format(len(ranked) - args.top))
    print("interrupted (not counted as failures)")
    combined = {}
    for side, source in ((0, logs["user_interrupts"]),
                         (1, db["user_interrupts"] if db_ok else {})):
        for cls, n in source.items():
            combined.setdefault(cls, [0, 0])[side] += n
    ranked = sorted(combined.items(), key=lambda kv: (-sum(kv[1]), kv[0]))
    if not ranked:
        print("  none")
    for cls, (log_n, db_n) in ranked:
        print("  {}  {} | {}".format(cls, log_n, db_n))
    if db_ok and db["unrec_exec"]:
        print("unrecognized execution terminals (the verdict cannot be "
              "healthy)")
        ranked = sorted(db["unrec_exec"].items(),
                        key=lambda kv: (-kv[1], kv[0]))
        for cls, n in ranked:
            print("  {}  {} | {}".format(cls, 0, n))
    print("repeated failures in window (>={}, suspected brick)".format(
        args.min_repeats))
    if db_ok:
        repeated = [(sid, n, last) for sid, (n, last) in
                    db["brick"].items()
                    if n >= args.min_repeats]
    else:
        repeated = []
    repeated.sort(key=lambda item: (-item[1], item[0]))
    if not repeated:
        print("  none")
    for sid, n, last in repeated:
        print("  {}  {}  last {}".format(sid, n, iso(last)))
    print("lost to restart (session, count, last; not counted as bricks)")
    if db_ok:
        lost = [(sid, n, last) for sid, (n, last) in
                db["lost_sessions"].items()]
    else:
        lost = []
    lost.sort(key=lambda item: (-item[1], item[0]))
    if not lost:
        print("  none")
    for sid, n, last in lost:
        print("  {}  {}  last {}".format(sid, n, iso(last)))
    print("build")
    if exe is not None:
        print("  running: {} (pid {})".format(exe, pid))
        lines = build_check(exe, Path(args.repo) if args.repo else None)
    elif service_error and "process table" in service_error:
        lines = ["  running: not checked ({})".format(service_error)]
    else:
        lines = ["  running: none ({})".format(service_error)]
    for line in lines:
        print(line)

    if state == TURN_FAILED:
        parts = []
        if failed_exec:
            parts.append("{} failed execution(s)".format(failed_exec))
        if lost_exec:
            parts.append("{} lost to restart or process loss".format(
                lost_exec))
        if stranded_exec:
            parts.append("{} stranded execution(s)".format(stranded_exec))
        if stranded_input:
            parts.append("{} accepted input(s) never ran".format(
                stranded_input))
        if unrec_exec_total:
            parts.append("{} unrecognized terminal event(s)".format(
                unrec_exec_total))
        print("verdict  FAILURES  (exit 1): " +
              (", ".join(parts) or "the window contains failures"))
        return 1
    if service_error and "process table" not in service_error:
        print("verdict  FAILURES  (exit 1): " + service_error)
        return 1
    unknown = []
    if logs["lines_in_window"] == 0:
        unknown.append("no log lines fall in the window")
    if not db_ok:
        unknown.append("the session database is {}; executions unknown".format(
            db["error"]))
    elif counts["started"] == 0 and counts["admitted"] == 0:
        unknown.append("no executions started and no inputs admitted in "
                       "the window")
    if state == TURN_UNKNOWN:
        if unproven:
            unknown.append("{} event(s) lack an executionID or inputID, "
                           "evidence unproven".format(unproven))
        if in_progress_exec:
            unknown.append("{} execution(s) started without a recognised "
                           "terminal, possibly still running".format(
                               in_progress_exec))
        if in_progress_input:
            unknown.append("{} admitted input(s) without an execution "
                           "start, possibly still queued".format(
                               in_progress_input))
    if db_ok and db["truncated"]:
        unknown.append("only the first {} classified database rows were read".format(
            MAX_EVENT_ROWS))
    if logs["bytes_capped"]:
        unknown.append("only the first {:,} bytes of the service logs were "
                       "scanned".format(MAX_LOG_BYTES))
    if unknown:
        print("verdict  UNKNOWN  (exit 2): " + "; ".join(unknown))
        return 2
    print("verdict  HEALTHY  (exit 0)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
