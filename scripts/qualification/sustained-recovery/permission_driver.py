"""One fresh permission decision per snapshot; reconcile only exact durable conflicts."""

from contextlib import closing
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import urllib.error
import urllib.parse
import urllib.request


PAGE_ROWS = 256
ERROR_BYTES = 64 * 1024
LIFECYCLE_ROWS = 4096
LIFECYCLE_BYTES = 8 * 1024 * 1024


class EvidenceBoundExceeded(ValueError):
    def __init__(self, reason, rows, size):
        super().__init__(reason)
        self.reason = reason
        self.rows = rows
        self.size = size


def bounded_identity(value, label, maximum=256):
    if not isinstance(value, str) or not value or len(value) > maximum:
        raise ValueError(f"{label} is absent or oversized")
    if not all(character.isalnum() or character in "._:-" for character in value):
        raise ValueError(f"{label} contains unsafe characters")
    return value


def decide_one(api, prefix, permitted, terminal_reply, record):
    page = api(prefix + "/permission")
    rows = page["data"]
    if not isinstance(rows, list) or len(rows) > PAGE_ROWS:
        raise ValueError("invalid or oversized permission page")
    if not rows:
        return False
    permission = rows[0]
    session = prefix.rsplit("/", 1)[-1]
    request = bounded_identity(permission.get("id"), "permission id")
    if permission["sessionID"] != session or permission["status"] != "pending":
        raise ValueError("permission is foreign or not pending")
    choice = "once" if permitted(permission) else "reject"
    safe_permission = {
        "id": request,
        "sessionID": bounded_identity(permission.get("sessionID"), "session id"),
        "status": "pending",
    }
    action = permission.get("action")
    if action is not None:
        safe_permission["action"] = bounded_identity(action, "permission action")
    record({"phase": "attempt", "permission": safe_permission, "reply": choice})
    try:
        api(prefix + "/permission/" + request + "/reply", {"reply": choice})
    except urllib.error.HTTPError as error:
        raw = error.read(ERROR_BYTES + 1)
        error.close()
        if len(raw) > ERROR_BYTES:
            record(
                {
                    "phase": "error",
                    "requestID": request,
                    "status": error.code,
                    "body_state": "oversized",
                }
            )
            raise ValueError("oversized permission error") from error
        record(
            {
                "phase": "error",
                "requestID": request,
                "status": error.code,
                "body_bytes": len(raw),
                "body_sha256": hashlib.sha256(raw).hexdigest(),
            }
        )
        try:
            value = json.loads(raw)
        except (ValueError, UnicodeError):
            raise error
        if (
            error.code != 409
            or not isinstance(value, dict)
            or value.get("_tag") != "PermissionNotLiveError"
            or value.get("requestID") != request
            or value.get("message") != "Permission request already settled: " + request
        ):
            raise error
        settled = terminal_reply(session, request)
        if settled not in ("once", "session", "always", "reject"):
            raise ValueError("terminal permission evidence missing") from error
        record(
            {"phase": "settled-elsewhere", "requestID": request, "reply": settled}
        )
    else:
        record({"phase": "applied", "requestID": request, "reply": choice})
    return True


def read_terminal_reply(database, session, request):
    rows = database.execute(
        "SELECT data FROM event WHERE aggregate_family='session' AND aggregate_id=? "
        "AND type='permission.replied.2' AND json_extract(data,'$.requestID')=? LIMIT 2",
        (session, request),
    ).fetchall()
    if len(rows) != 1:
        raise ValueError("missing or ambiguous permission settlement")
    row = json.loads(rows[0][0])
    if row.get("sessionID") != session or row.get("requestID") != request:
        raise ValueError("foreign permission settlement")
    return row.get("reply")


def preserve_lifecycle(database_path, session, destination):
    """Best-effort bounded public evidence after teardown; never hides failure."""
    try:
        uri = database_path.resolve().as_uri() + "?mode=ro"
        with closing(sqlite3.connect(uri, uri=True)) as database:
            where = (
                "aggregate_family='session' AND aggregate_id=? AND "
                "(type LIKE 'permission.%' OR type LIKE 'session.execution.%' "
                "OR type LIKE 'session.compaction.%' OR type LIKE 'session.usage.%' "
                "OR type='session.tool.terminal.1')"
            )
            count, size = database.execute(
                "SELECT COUNT(*),COALESCE(SUM(length(CAST(data AS BLOB))),0) "
                "FROM event WHERE " + where,
                (session,),
            ).fetchone()
            if count > LIFECYCLE_ROWS:
                raise EvidenceBoundExceeded("row-bound", count, size)
            if size > LIFECYCLE_BYTES:
                raise EvidenceBoundExceeded("byte-bound", count, size)
            # SQLite projects only short allowlisted scalars. Private-capable
            # resources, metadata, source, error detail and full event JSON are
            # never materialized in the driver process.
            rows = database.execute(
                "SELECT seq,type,json_valid(data),"
                "substr(json_extract(data,'$.sessionID'),1,257),"
                "substr(json_extract(data,'$.requestID'),1,257),"
                "substr(json_extract(data,'$.action'),1,257),"
                "substr(json_extract(data,'$.reply'),1,17) "
                "FROM event WHERE " + where + " ORDER BY seq",
                (session,),
            )
            public = []
            for seq, kind, valid, held_session, request, action, reply in rows:
                if valid != 1:
                    raise ValueError("lifecycle event is not JSON")
                data = {}
                if kind.startswith("permission."):
                    if held_session is not None:
                        data["sessionID"] = bounded_identity(held_session, "event session")
                    if request is not None:
                        data["requestID"] = bounded_identity(request, "event request")
                    if action is not None:
                        data["action"] = bounded_identity(action, "event action")
                    if reply is not None:
                        if reply not in ("once", "session", "always", "reject"):
                            raise ValueError("permission event reply is invalid")
                        data["reply"] = reply
                public.append({"seq": seq, "type": kind, "data": data})
        destination.write_text(json.dumps(public, indent=2) + "\n", encoding="utf-8")
        return {"state": "captured", "rows": len(public)}
    except EvidenceBoundExceeded as error:
        return {
            "state": "refused",
            "reason": error.reason,
            "observed_rows": error.rows,
            "observed_bytes": error.size,
        }
    except Exception as error:
        return {"state": "failed", "error_type": type(error).__name__}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("decide",))
    parser.add_argument("--prefix", required=True)
    parser.add_argument("--database", type=Path, required=True)
    parser.add_argument("--record", type=Path, required=True)
    parser.add_argument("--reply", choices=("once", "reject"), required=True)
    arguments = parser.parse_args()
    parsed = urllib.parse.urlparse(arguments.prefix)
    if parsed.scheme != "http" or parsed.hostname not in ("127.0.0.1", "localhost"):
        parser.error("the deterministic driver requires a loopback HTTP product")
    authorization = os.environ.get("GRAPT_DRIVER_AUTHORIZATION")
    if not authorization:
        parser.error("GRAPT_DRIVER_AUTHORIZATION is required")

    def api(path, body=None):
        encoded = None if body is None else json.dumps(body).encode("utf-8")
        request = urllib.request.Request(path, data=encoded, method="GET" if body is None else "POST")
        request.add_header("Authorization", authorization)
        request.add_header("Content-Type", "application/json")
        with urllib.request.urlopen(request, timeout=3) as response:
            raw = response.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            raise ValueError("product response exceeded its bound")
        return {} if not raw else json.loads(raw)

    def terminal(session, request):
        uri = arguments.database.resolve().as_uri() + "?mode=ro"
        with closing(sqlite3.connect(uri, uri=True)) as database:
            return read_terminal_reply(database, session, request)

    def record(value):
        encoded = json.dumps(value, sort_keys=True)
        if len(encoded.encode("utf-8")) > 4096:
            raise ValueError("permission diagnostic exceeded its bound")
        with arguments.record.open("a", encoding="utf-8") as stream:
            stream.write(encoded + "\n")

    decided = decide_one(
        api,
        arguments.prefix,
        lambda _permission: arguments.reply == "once",
        terminal,
        record,
    )
    print(json.dumps({"decided": decided, "reply": arguments.reply}))


if __name__ == "__main__":
    main()
