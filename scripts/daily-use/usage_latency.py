"""Recompute the usage and latency tables from a Session database.

COMMITTED SO THE FIGURES CAN BE RECHECKED rather than trusted. Point it at a
COPY of a service's `database.sqlite3`; it only reads, but a live service owns
its file and this should never be the thing that touches it.

    python scripts/daily-use/usage_latency.py <database.sqlite3>

TWO THINGS IT IS CAREFUL ABOUT, both of which produced wrong numbers first:

  * Latency uses `completedAt` from a step terminal's PAYLOAD, never the
    event's `created`. `created` on `session.step.ended.3` is a RESERVED
    coordinate allocated when the step began, so it precedes events the step
    waited on and yields a near-zero interval that looks plausible.

  * The interval names are STEP boundaries, not provider dispatch-to-first-
    output. Submission, admission and dispatch all sit inside them.
"""

import json
import sqlite3
import sys
from pathlib import Path


def events(connection, session):
    return [
        (row[0], row[1], row[2], json.loads(row[3]))
        for row in connection.execute(
            "select seq, type, created, data from event where aggregate_id = ? order by seq",
            (session,),
        )
    ]


def usage(connection):
    print("USAGE, per attempt")
    for aggregate, data in connection.execute(
        "select aggregate_id, data from event where type = 'session.usage.observed.2' order by rowid"
    ):
        observed = json.loads(data)
        counts = observed["usage"]
        fields = {
            name: value
            for name, value in counts.items()
            if isinstance(value, dict) and "count" in value
        }
        print(f"  session {aggregate[-8:]} attempt {observed['attempt'][-10:]}")
        for name, value in sorted(fields.items()):
            print(f"    {name:<26}{value['count']:>7}  {value['provenance']}")
        names = ("inputTotalTokens", "outputTokens", "totalTokens")
        if all(name in fields for name in names):
            balance = fields[names[0]]["count"] + fields[names[1]]["count"]
            print(f"    input+output == total: {balance == fields[names[2]]['count']}")
        else:
            print("    input+output == total: unavailable (provider fields absent)")


def latency(connection):
    sessions = [
        row[0]
        for row in connection.execute(
            "select distinct aggregate_id from event where type = 'session.execution.started.1'"
        )
    ]
    print()
    print("LATENCY (ms), settlement times only")
    print(f"{'session':<10}{'step1->1st out':>16}{'approve':>10}{'tool run':>10}{'step2->out':>12}{'total':>8}")
    for session in sessions:
        recorded = events(connection, session)
        executions = sum(name == "session.execution.started.1" for _, name, _, _ in recorded)
        if executions != 1:
            print(f"{session[-8:]}: unavailable ({executions} executions; this table requires one)")
            continue

        def at(kind, index=0, field=None):
            hits = [(created, data) for _, name, created, data in recorded if name == kind]
            if len(hits) <= index:
                return None
            created, data = hits[index]
            return data[field] if field else created

        started = at("session.execution.started.1")
        opened = [data for _, name, _, data in recorded
                  if name == "session.assistant-message.opened.2"]
        second_output = None
        if len(opened) > 1:
            message = opened[1]["message"]
            second_output = next((created for _, name, created, data in recorded
                                  if name == "session.content.appended.1"
                                  and data.get("message") == message), None)
        gap = lambda a, b: (b - a) if a is not None and b is not None else None
        print(
            f"{session[-8:]:<10}"
            f"{str(gap(started, at('session.content.appended.1'))):>16}"
            f"{str(gap(at('permission.asked.2'), at('permission.replied.2'))):>10}"
            f"{str(gap(at('permission.replied.2'), at('session.tool.terminal.1'))):>10}"
            f"{str(gap(at('session.step.ended.3', 0, 'completedAt'), second_output)):>12}"
            f"{str(gap(started, at('session.execution.succeeded.1'))):>8}"
        )


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    connection = sqlite3.connect(Path(sys.argv[1]).resolve().as_uri() + "?mode=ro", uri=True)
    usage(connection)
    latency(connection)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
