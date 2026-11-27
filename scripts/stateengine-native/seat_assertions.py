"""Validate the complete trace after every owned seat-walk process has joined."""
import json


def read_rows(path):
    raw = path.read_bytes()
    if len(raw) > 65536:
        raise ValueError("seat trace exceeds its byte bound")
    rows = [json.loads(line) for line in raw.splitlines()]
    if len(rows) > 256:
        raise ValueError("seat trace exceeds its row bound")
    return rows


def validate(evidence, held_after, mailbox):
    calls = (evidence / "mcp-calls.txt").read_text().splitlines()
    expected_calls = ["stateengine_resume", "stateengine_msg_list", "stateengine_resume",
                      "stateengine_msg_read", "stateengine_msg_read", "stateengine_resume"]
    assert calls == expected_calls, ("final MCP sequence", calls)
    events = read_rows(evidence / "mcp-calls.txt.jsonl")
    assert [row["name"] for row in events] == expected_calls, "unsettled MCP trace"
    rows = read_rows(evidence / "provider-observations.jsonl")
    attempts = read_rows(evidence / "provider-attempts.jsonl")
    assert attempts == [{"at_ns": row["at_ns"]} for row in rows], "unsettled provider attempt"
    expected = [("SEAT_LAUNCH", False), ("SEAT_LAUNCH", True),
                ("msg_seat_idle", False), ("msg_seat_idle", True), ("SEAT_BUSY", False),
                ("msg_seat_busy", False), ("msg_seat_busy", True),
                ("summary", False), ("SEAT_COMPACTED", False)]
    assert [(row["label"], row["tool_result"]) for row in rows] == expected, "final provider sequence"
    for offset, message in ((3, "msg_seat_idle"), (6, "msg_seat_busy")):
        body = {"msg_id": message, "body": "SEAT_MAIL_" + message.removeprefix("msg_seat_").upper()}
        assert rows[offset]["mail_read"] == body, "wrong full read reached the model"
        call = events[3 if message == "msg_seat_idle" else 4]
        assert call["arguments"] == {"msg_id": message}, "wrong MCP read request"
        assert call["result"] == {"message": body, "read_acknowledgement": "recorded"}, "wrong MCP read result"
        assert rows[offset - 1]["at_ns"] < call["at_ns"] < rows[offset]["at_ns"], "mail call order"
    held = int((evidence / "busy-held").read_text())
    released = int((evidence / "busy-released").read_text())
    assert held <= held_after < released, "busy barrier was not held across mail publication"
    assert not any(held_after <= row["at_ns"] <= released for row in rows), "provider entered while busy"
    assert not any(held_after <= row["at_ns"] <= released for row in events), "MCP entered while busy"
    assert rows[6]["at_ns"] > released, "busy mail was not deferred"
    assert all(row["private_absent"] for row in rows), "private binding reached model"
    assert all(row["packet"] for row in rows if row["label"] != "summary"), "verified packet missing"
    assert all(row["required_read"] for row in rows
               if row["label"] not in ("summary", "SEAT_COMPACTED")), "required read missing"
    assert mailbox.error is None, mailbox.error
    assert mailbox.acknowledged == mailbox.fetched == {"msg_seat_idle", "msg_seat_busy"}


def run_joined(journey):
    try:
        result = journey.witness()
    except Exception:
        if journey.tui:
            (journey.evidence / "failure-screen.txt").write_text(journey.driver("capture"))
        raise
    finally:
        journey.close()
    # Cleanup can itself produce calls. Final acceptance uses only joined traces.
    assert all(child.returncode == 0 for child in journey.clients), "seat client did not exit cleanly"
    validate(journey.evidence, journey.held_after, journey.peer.mailbox)
    return result
