"""Wait for completed setup executions in the owned synthetic store, read-only."""
import json
import sqlite3
import sys
import time
from pathlib import Path


def wait(database, session, output):
    # Missing/unreadable/schema-invalid evidence is a failure, never a skip.
    with sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True, timeout=0.1) as connection:
        for _ in range(400):
            rows = connection.execute(
                "SELECT id, seq FROM event WHERE aggregate_family='session' AND aggregate_id=? AND type=? ORDER BY seq LIMIT 2",
                (session, "session.execution.succeeded.1")).fetchall()
            if len(rows) == 2:
                output.write_text(json.dumps({"session": session, "terminals": rows}) + "\n", encoding="utf-8")
                return
            time.sleep(0.025)
        observed = connection.execute("SELECT seq, type FROM event WHERE aggregate_family='session' AND aggregate_id=? ORDER BY seq LIMIT 32", (session,)).fetchall()
        output.write_text(json.dumps({"session": session, "observed": observed}) + "\n", encoding="utf-8")
    raise RuntimeError("setup never reached two real execution terminals within ten seconds")


if __name__ == "__main__":
    wait(Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3]))
