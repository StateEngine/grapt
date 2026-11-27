"""Correlate an observed run's durable tool and permission record.

A receipt says what was on SCREEN. It cannot say whether a call reached a
terminal, because a rendered verb is not a terminal discriminator — the retained
client shows `Ran` for any parsed command, streaming included. The durable
record can say it, and this reads it.

## What makes this a correlation rather than two lists

The load-bearing edge is `permission.asked.2 -> permission.replied.2 -> the tool
call the permission was FOR`. Emitting calls and permissions as separate arrays
leaves a reader to pair them by eye, which is the mistake this file exists to
avoid: two clients drive one product in a comparison run, so the pairing is
exactly where a wrong answer hides.

## What it refuses

Evidence that is corrupt, ambiguous or incomplete is REFUSED, not skipped. A
silently dropped row makes an empty result indistinguishable from a clean run,
and "no defects found" would then be produced by a parser bug as readily as by a
correct product. Every refusal names the row id, type and session.

Identity is `(session, call id)`. A call id alone is not unique across sessions,
and in a comparison run the two clients' sessions are precisely what must not be
merged. Payload session is checked against the event's own `aggregate_id`, so a
record that disagrees with its own aggregate is refused rather than believed.

Ordering is `aggregate_id, seq` — the durable sequence — not event id.

The store is opened READ-ONLY (`mode=ro`). Evidence a reader might mutate while
inspecting it is not evidence.

Usage:
    python correlate.py --service-root <root> [--out <file.json>]
    python correlate.py --selftest
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import tempfile
from pathlib import Path

TOOL_STARTED = "session.tool.input.started.1"
TOOL_TERMINAL = "session.tool.terminal.1"
PERMISSION_ASKED = "permission.asked.2"
PERMISSION_REPLIED = "permission.replied.2"

WANTED = (TOOL_STARTED, TOOL_TERMINAL, PERMISSION_ASKED, PERMISSION_REPLIED)


class CorrelationError(RuntimeError):
    """Evidence this extractor will not draw a conclusion from."""


def _connect(database: Path) -> sqlite3.Connection:
    """Open an evidence store the ONLY way this tool is allowed to open one.

    Read-only, and factored out so it can be witnessed. While it was inline in
    `_rows`, the selftest exercised `correlate` on synthetic rows and never
    opened a store at all — so replacing `mode=ro` with `mode=rw` left the
    whole selftest green. A reader that can mutate the evidence is not a
    reader, and that property has to be measured somewhere.
    """
    return sqlite3.connect(f"file:{database.as_posix()}?mode=ro", uri=True)


def _rows(database: Path) -> list[dict]:
    connection = _connect(database)
    try:
        columns = [c[1] for c in connection.execute("PRAGMA table_info(event)")]
        placeholders = ",".join("?" for _ in WANTED)
        # DURABLE ORDER: aggregate then seq. Event id is an insertion detail and
        # can interleave aggregates.
        found = connection.execute(
            f"SELECT * FROM event WHERE type IN ({placeholders}) "
            "ORDER BY aggregate_id, seq",
            WANTED,
        ).fetchall()
    finally:
        connection.close()
    return [dict(zip(columns, row)) for row in found]


def _payload(row: dict) -> dict:
    try:
        value = json.loads(row["data"])
    except (TypeError, ValueError) as error:
        raise CorrelationError(
            f"row id={row.get('id')} type={row.get('type')} "
            f"aggregate={row.get('aggregate_id')} seq={row.get('seq')}: "
            f"payload is not readable JSON ({error})"
        ) from error
    if not isinstance(value, dict):
        raise CorrelationError(
            f"row id={row.get('id')} type={row.get('type')}: payload is not an object"
        )
    return value


def _session_of(row: dict, payload: dict) -> str:
    """The session this row belongs to, checked against its own aggregate."""
    stated = payload.get("sessionID")
    aggregate = row.get("aggregate_id")
    if stated is not None and aggregate is not None and stated != aggregate:
        raise CorrelationError(
            f"row id={row.get('id')} type={row.get('type')} seq={row.get('seq')}: "
            f"payload session {stated!r} disagrees with aggregate {aggregate!r}"
        )
    session = stated or aggregate
    if not session:
        raise CorrelationError(
            f"row id={row.get('id')} type={row.get('type')}: no session to attribute it to"
        )
    return session


def _require(row: dict, payload: dict, field: str) -> str:
    value = payload.get(field)
    if not value:
        raise CorrelationError(
            f"row id={row.get('id')} type={row.get('type')} "
            f"aggregate={row.get('aggregate_id')} seq={row.get('seq')}: "
            f"missing `{field}`, so it cannot be correlated"
        )
    return value


def correlate(rows: list[dict]) -> dict:
    calls: dict[tuple[str, str], dict] = {}
    permissions: dict[tuple[str, str], dict] = {}

    for row in rows:
        payload = _payload(row)
        kind = row["type"]
        session = _session_of(row, payload)

        if kind == TOOL_STARTED:
            key = (session, _require(row, payload, "callID"))
            if key in calls and "started_seq" in calls[key]:
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: call {key[1]} "
                    f"starts twice in session {key[0]}; ambiguous"
                )
            entry = calls.setdefault(key, {"session": key[0], "call": key[1]})
            entry["tool"] = _require(row, payload, "name")
            entry["started_seq"] = row["seq"]
        elif kind == TOOL_TERMINAL:
            key = (session, _require(row, payload, "call"))
            entry = calls.get(key)
            if entry is None:
                # AN ORPHAN TERMINAL. `setdefault` here invented a call that
                # never started, and the extract then presented it as a settled
                # invocation. A terminal is evidence ABOUT a call, not evidence
                # that one happened.
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: terminal for "
                    f"call {key[1]} in session {key[0]} has no start; orphan"
                )
            if "terminal" in entry:
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: call {key[1]} "
                    f"reaches two terminals in session {key[0]}; ambiguous"
                )
            outcome = payload.get("outcome")
            if not isinstance(outcome, dict) or not outcome.get("kind"):
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: terminal for "
                    f"call {key[1]} carries no outcome kind; unusable as settlement"
                )
            entry["terminal"] = outcome
            entry["terminal_seq"] = row["seq"]
        elif kind == PERMISSION_ASKED:
            key = (session, _require(row, payload, "id"))
            if key in permissions:
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: permission "
                    f"{key[1]} asked twice in session {key[0]}; ambiguous"
                )
            source = payload.get("source")
            source = source if isinstance(source, dict) else {}
            call = source.get("callID")
            resources = payload.get("resources")
            # A TOOL ask that names no call and no target cannot support an
            # approval claim. It is KEPT, because a non-tool permission is
            # legitimate evidence of its own, but it is marked so validation
            # cannot mistake it for one.
            joinable = bool(call) and isinstance(resources, list) and bool(resources)
            permissions[key] = {
                "session": key[0],
                "request": key[1],
                "action": payload.get("action"),
                # EXACT, never summarised: this is what the user authorised.
                "resources": resources,
                # THE EDGE that makes this a correlation.
                "call": call,
                "joinable": joinable,
                "asked_seq": row["seq"],
            }
        elif kind == PERMISSION_REPLIED:
            key = (session, _require(row, payload, "requestID"))
            entry = permissions.get(key)
            if entry is None:
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: reply for "
                    f"permission {key[1]} in session {key[0]} has no matching ask"
                )
            if "reply" in entry:
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: permission "
                    f"{key[1]} answered twice in session {key[0]}; ambiguous"
                )
            reply = payload.get("reply")
            if not isinstance(reply, str) or not reply:
                raise CorrelationError(
                    f"row id={row.get('id')} seq={row.get('seq')}: reply to "
                    f"permission {key[1]} carries no decision; unusable"
                )
            entry["reply"] = reply
            entry["replied_seq"] = row["seq"]

    # JOIN, in the extract rather than in a reader's head.
    for entry in permissions.values():
        call = entry.get("call")
        if call is None:
            continue
        key = (entry["session"], call)
        if key not in calls:
            raise CorrelationError(
                f"permission {entry['request']} names call {call} in session "
                f"{entry['session']}, which has no tool call; unjoinable"
            )
        calls[key].setdefault("permissions", []).append(entry["request"])

    sessions: dict[str, dict] = {}
    for (session, _), entry in sorted(calls.items()):
        sessions.setdefault(session, {"calls": [], "permissions": []})["calls"].append(entry)
    for (session, _), entry in sorted(permissions.items()):
        sessions.setdefault(session, {"calls": [], "permissions": []})["permissions"].append(entry)

    return {
        "contract": "native-tui-durable-correlation/v2",
        "event_types": list(WANTED),
        "sessions": sessions,
        "note": (
            "Read-only extract of the durable record, keyed by (session, call id). A "
            "tool call's TERMINAL is the settlement claim; a client's rendered verb "
            "is not. Corrupt, ambiguous or unjoinable evidence is refused rather "
            "than skipped, so an empty result cannot be produced by a parser fault."
        ),
    }


def verify(extract: dict, expected: dict) -> list[dict]:
    """Check the exact journeys a scenario claims, in the sessions it names.

    TWO THINGS THIS REFUSES TO DO, both of which it used to.

    It does not SEARCH for a journey. Expectations name the client session ids,
    taken from the run's own launch mapping, and each journey is looked for in
    ONE named session. Searching every session and counting distinct hits let
    two sessions from the SAME client satisfy "two clients", and let a repeat
    observation of the same fixed command stand in for the intended one — the
    command being fixed is exactly what makes tool+target globally ambiguous.

    It does not treat an absent expectation as a pass. An empty journey list, a
    missing client map and a nonpositive requirement are all refusals, because
    the one thing worse than a failing check is a check that cannot fail.
    """
    clients = expected.get("clients")
    if not isinstance(clients, dict) or not clients:
        raise CorrelationError(
            "expected-facts names no client sessions; a journey cannot be bound "
            "to a client without them, and searching for one is what this refuses"
        )
    for role, session in clients.items():
        if not isinstance(session, str) or not session:
            raise CorrelationError(f"client {role!r} has no session id")
        if session not in extract["sessions"]:
            raise CorrelationError(
                f"client {role!r} names session {session}, which is absent from "
                f"this store; the expectation does not describe this run"
            )

    journeys = expected.get("journeys")
    if not isinstance(journeys, list) or not journeys:
        raise CorrelationError(
            "expected-facts declares no journeys, so it would pass without "
            "checking anything"
        )

    resolved: list[dict] = []
    for want in journeys:
        for field in ("client", "tool", "target", "reply", "outcome"):
            if not want.get(field):
                raise CorrelationError(f"a journey is missing `{field}`")
        role = want["client"]
        if role not in clients:
            raise CorrelationError(
                f"journey names client {role!r}, which has no session binding"
            )
        session_id = clients[role]
        session = extract["sessions"][session_id]
        tool, target = want["tool"], want["target"]

        asks = {
            permission["request"]: permission
            for permission in session["permissions"]
            # A permission naming no call and no target cannot resolve a tool
            # approval, whatever else it is evidence of.
            if permission.get("joinable")
            and permission.get("resources")
            and target in permission["resources"]
        }
        candidates = []
        for call in session["calls"]:
            if call.get("tool") != tool:
                continue
            joined = [asks[r] for r in call.get("permissions", []) if r in asks]
            if len(joined) > 1:
                raise CorrelationError(
                    f"call {call['call']} joins {len(joined)} permissions for "
                    f"target {target!r}; ambiguous"
                )
            if joined:
                candidates.append((call, joined[0]))

        if not candidates:
            raise CorrelationError(
                f"{role} session {session_id} has no {tool} on {target!r}; an "
                f"absent journey is a failure however many other calls settled"
            )
        if len(candidates) > 1:
            raise CorrelationError(
                f"{role} session {session_id} has {len(candidates)} {tool} calls "
                f"on {target!r}; ambiguous, so none of them is THE journey"
            )

        call, permission = candidates[0]
        if permission.get("reply") != want["reply"]:
            raise CorrelationError(
                f"{role} call {call['call']}: permission {permission['request']} "
                f"was answered {permission.get('reply')!r}, expected {want['reply']!r}"
            )
        terminal = call.get("terminal")
        if not isinstance(terminal, dict) or terminal.get("kind") != want["outcome"]:
            raise CorrelationError(
                f"{role} call {call['call']}: terminal is {terminal!r}, "
                f"expected kind {want['outcome']!r}"
            )
        resolved.append(
            {
                "client": role,
                "session": session_id,
                "call": call["call"],
                "tool": tool,
                "target": target,
                "reply": want["reply"],
                "outcome": want["outcome"],
            }
        )
    return resolved


# ---------------------------------------------------------------------------
# The evidence boundary, proven rather than asserted.
# ---------------------------------------------------------------------------


def _row(id_, type_, aggregate, seq, data):
    return {
        "id": id_,
        "type": type_,
        "aggregate_id": aggregate,
        "seq": seq,
        "data": json.dumps(data) if isinstance(data, (dict, list)) else data,
    }


def selftest() -> int:
    failures: list[str] = []

    def refuses(name, rows, needle=""):
        try:
            correlate(rows)
        except CorrelationError as error:
            if needle and needle not in str(error):
                failures.append(f"{name}: refused for the wrong reason: {error}")
            else:
                print(f"  ok   {name}")
            return
        failures.append(f"{name}: ACCEPTED evidence it must refuse")
        print(f"  FAIL {name}")

    print("the correlation boundary refuses what it cannot trust:")

    # A REUSED CALL ID ACROSS TWO SESSIONS must not merge.
    rows = [
        _row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_a", "callID": "call_x", "name": "shell"}),
        _row(2, TOOL_TERMINAL, "ses_a", 2, {"sessionID": "ses_a", "call": "call_x", "outcome": {"kind": "completed"}}),
        _row(3, TOOL_STARTED, "ses_b", 1, {"sessionID": "ses_b", "callID": "call_x", "name": "shell"}),
        _row(4, TOOL_TERMINAL, "ses_b", 2, {"sessionID": "ses_b", "call": "call_x", "outcome": {"kind": "failed"}}),
    ]
    extract = correlate(rows)
    same_id_kept_apart = (
        len(extract["sessions"]) == 2
        and extract["sessions"]["ses_a"]["calls"][0]["terminal"]["kind"] == "completed"
        and extract["sessions"]["ses_b"]["calls"][0]["terminal"]["kind"] == "failed"
    )
    print(f"  {'ok  ' if same_id_kept_apart else 'FAIL'} one call id in two sessions stays two calls")
    if not same_id_kept_apart:
        failures.append("a reused call id merged across sessions")

    refuses(
        "a payload session disagreeing with its aggregate",
        [_row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_b", "callID": "call_x", "name": "shell"})],
        "disagrees with aggregate",
    )
    refuses(
        "a permission naming a call that does not exist",
        [
            _row(1, PERMISSION_ASKED, "ses_a", 1, {
                "sessionID": "ses_a", "id": "per_1", "action": "shell",
                "resources": ["echo hi"], "source": {"callID": "call_missing"},
            }),
        ],
        "unjoinable",
    )
    refuses(
        "corrupt json",
        [_row(1, TOOL_STARTED, "ses_a", 1, "{not json")],
        "not readable JSON",
    )
    refuses(
        "a reply with no matching ask",
        [_row(1, PERMISSION_REPLIED, "ses_a", 1, {"sessionID": "ses_a", "requestID": "per_ghost", "reply": "once"})],
        "no matching ask",
    )
    refuses(
        "a call that starts twice",
        [
            _row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_a", "callID": "call_x", "name": "shell"}),
            _row(2, TOOL_STARTED, "ses_a", 2, {"sessionID": "ses_a", "callID": "call_x", "name": "shell"}),
        ],
        "starts twice",
    )
    refuses(
        "a call with two terminals",
        [
            _row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_a", "callID": "call_x", "name": "shell"}),
            _row(2, TOOL_TERMINAL, "ses_a", 2, {"sessionID": "ses_a", "call": "call_x", "outcome": {"kind": "completed"}}),
            _row(3, TOOL_TERMINAL, "ses_a", 3, {"sessionID": "ses_a", "call": "call_x", "outcome": {"kind": "failed"}}),
        ],
        "two terminals",
    )
    refuses(
        "a terminal with no start (case A)",
        [_row(1, TOOL_TERMINAL, "ses_a", 1, {"sessionID": "ses_a", "call": "c", "outcome": {}})],
        "orphan",
    )
    refuses(
        "a terminal whose outcome has no kind",
        [
            _row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_a", "callID": "c", "name": "shell"}),
            _row(2, TOOL_TERMINAL, "ses_a", 2, {"sessionID": "ses_a", "call": "c", "outcome": {}}),
        ],
        "no outcome kind",
    )
    refuses(
        "a reply carrying no decision (case B)",
        [
            _row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_a", "callID": "c", "name": "shell"}),
            _row(2, PERMISSION_ASKED, "ses_a", 2, {
                "sessionID": "ses_a", "id": "p", "action": "shell", "source": {"type": "tool"},
            }),
            _row(3, PERMISSION_REPLIED, "ses_a", 3, {"sessionID": "ses_a", "requestID": "p"}),
        ],
        "no decision",
    )
    refuses(
        "a tool start with no name",
        [_row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_a", "callID": "c"})],
        "missing `name`",
    )
    refuses(
        "a row with no id to correlate",
        [_row(1, TOOL_STARTED, "ses_a", 1, {"sessionID": "ses_a", "name": "shell"})],
        "missing `callID`",
    )

    print("named facts, bound to named sessions:")

    def rejects(name, extract, expected, needle=""):
        try:
            verify(extract, expected)
        except CorrelationError as error:
            if needle and needle not in str(error):
                failures.append(f"{name}: rejected for the wrong reason: {error}")
            else:
                print(f"  ok   {name}")
            return
        failures.append(f"{name}: ACCEPTED a claim it must reject")
        print(f"  FAIL {name}")

    def chain(session, call, request, tool, target, reply, kind, seq=1):
        return [
            _row(seq, TOOL_STARTED, session, seq, {"sessionID": session, "callID": call, "name": tool}),
            _row(seq + 1, PERMISSION_ASKED, session, seq + 1, {
                "sessionID": session, "id": request, "action": tool,
                "resources": [target], "source": {"callID": call},
            }),
            _row(seq + 2, PERMISSION_REPLIED, session, seq + 2, {
                "sessionID": session, "requestID": request, "reply": reply,
            }),
            _row(seq + 3, TOOL_TERMINAL, session, seq + 3, {
                "sessionID": session, "call": call, "outcome": {"kind": kind},
            }),
        ]

    CMD = "echo a> j03.txt"
    two_clients = {"native": "ses_native", "retained": "ses_retained"}
    journey = lambda client, kind="completed", reply="once": {
        "client": client, "tool": "shell", "target": CMD, "reply": reply, "outcome": kind
    }

    # THE FOREIGN-SESSION CASE. A third session ran the SAME fixed command and
    # settled; the retained client's own session did not. Searching would have
    # found two successes and called it two clients.
    foreign = correlate(
        chain("ses_native", "call_1", "per_1", "shell", CMD, "once", "completed")
        + chain("ses_other", "call_2", "per_2", "shell", CMD, "once", "completed")
    )
    rejects(
        "a foreign session running the same command is not the client's journey",
        foreign,
        {"clients": two_clients, "journeys": [journey("native"), journey("retained")]},
        "absent from this store",
    )

    # Same shape, but the named retained session EXISTS and simply lacks the call.
    present_but_empty = correlate(
        chain("ses_native", "call_1", "per_1", "shell", CMD, "once", "completed")
        + chain("ses_retained", "call_9", "per_9", "read", "other.txt", "once", "completed")
        + chain("ses_other", "call_2", "per_2", "shell", CMD, "once", "completed")
    )
    rejects(
        "another session's success cannot fill a named session's gap",
        present_but_empty,
        {"clients": two_clients, "journeys": [journey("native"), journey("retained")]},
        "has no shell",
    )

    rejects(
        "empty expected-facts is not a pass",
        foreign,
        {},
        "names no client sessions",
    )
    rejects(
        "no journeys is not a pass",
        foreign,
        {"clients": {"native": "ses_native"}, "journeys": []},
        "declares no journeys",
    )
    rejects(
        "a journey naming an unbound client",
        foreign,
        {"clients": {"native": "ses_native"}, "journeys": [journey("retained")]},
        "no session binding",
    )

    duplicated = correlate(
        chain("ses_native", "call_1", "per_1", "shell", CMD, "once", "completed")
        + chain("ses_native", "call_2", "per_2", "shell", CMD, "once", "completed", seq=10)
    )
    rejects(
        "two identical calls in one session are ambiguous, not a pass",
        duplicated,
        {"clients": {"native": "ses_native"}, "journeys": [journey("native")]},
        "ambiguous",
    )

    failed = correlate(chain("ses_native", "call_1", "per_1", "shell", CMD, "once", "failed"))
    rejects(
        "a failed journey cannot satisfy a completed claim",
        failed,
        {"clients": {"native": "ses_native"}, "journeys": [journey("native")]},
        "expected kind",
    )
    refused_reply = correlate(chain("ses_native", "call_1", "per_1", "shell", CMD, "reject", "failed"))
    rejects(
        "a rejected permission cannot satisfy an approved claim",
        refused_reply,
        {"clients": {"native": "ses_native"}, "journeys": [journey("native", "failed")]},
        "expected 'once'",
    )

    both = correlate(
        chain("ses_native", "call_1", "per_1", "shell", CMD, "once", "completed")
        + chain("ses_retained", "call_2", "per_2", "shell", CMD, "once", "completed")
        + chain("ses_other", "call_3", "per_3", "shell", CMD, "once", "completed", seq=10)
    )
    matched = verify(both, {"clients": two_clients,
                            "journeys": [journey("native"), journey("retained")]})
    ok = (
        len(matched) == 2
        and {m["session"] for m in matched} == {"ses_native", "ses_retained"}
        and {m["client"] for m in matched} == {"native", "retained"}
    )
    print(f"  {'ok  ' if ok else 'FAIL'} the genuine two-client journey resolves, ignoring the third session")
    if not ok:
        failures.append("the genuine two-client journey did not resolve")

    # THE STORE IS OPENED READ-ONLY, and that is measured against a real file.
    #
    # Every case above runs on synthetic rows, so none of them opens a store —
    # which left the read-only mode unwitnessed. Evidence a reader can write to
    # is evidence a reader can spoil.
    with tempfile.TemporaryDirectory() as directory:
        database = Path(directory) / "evidence.db"
        seed = sqlite3.connect(database)
        seed.execute(
            "CREATE TABLE event (id INTEGER, type TEXT, aggregate_id TEXT, "
            "seq INTEGER, data TEXT)"
        )
        seed.commit()
        seed.close()

        readable = False
        try:
            _rows(database)
            readable = True
        except sqlite3.Error as error:
            failures.append(f"the read-only store could not be read: {error}")
        print(f"  {'ok  ' if readable else 'FAIL'} a real store is readable through the reader")

        refused = False
        connection = _connect(database)
        try:
            connection.execute("INSERT INTO event VALUES (1, 'x', 'a', 1, '{}')")
            connection.commit()
        except sqlite3.OperationalError:
            refused = True
        finally:
            connection.close()
        if not refused:
            failures.append("the evidence store accepted a WRITE through the reader")
        print(f"  {'ok  ' if refused else 'FAIL'} and a write through it is refused")

    print()
    if failures:
        print(f"{len(failures)} failure(s)")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("the correlation boundary holds")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service-root", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--selftest", action="store_true")
    parser.add_argument(
        "--expect",
        type=Path,
        help="expected-facts file naming each journey: tool, exact target, reply, "
        "outcome and how many distinct sessions must show it",
    )
    arguments = parser.parse_args()

    if arguments.selftest:
        return selftest()
    if not arguments.service_root:
        print("correlate: --service-root or --selftest is required", file=sys.stderr)
        return 2

    database = arguments.service_root / "database.sqlite3"
    if not database.is_file():
        print(f"correlate: no store at {database}", file=sys.stderr)
        return 2

    try:
        extract = correlate(_rows(database))
        if arguments.expect:
            expected = json.loads(arguments.expect.read_text(encoding="utf-8"))
            extract["verified"] = {
                "expectation": str(arguments.expect.resolve()),
                "journeys": verify(extract, expected),
            }
    except CorrelationError as error:
        print(f"correlate: {error}", file=sys.stderr)
        return 3

    extract["database"] = str(database.resolve())
    rendered = json.dumps(extract, indent=2)
    if arguments.out:
        arguments.out.write_text(rendered + "\n", encoding="utf-8")
        print(f"wrote {arguments.out}")
    else:
        print(rendered)
    return 0


if __name__ == "__main__":
    sys.exit(main())
