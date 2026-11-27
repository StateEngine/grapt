"""Explicit model actions and cursor-aware mail for the disposable TUI journey."""
from collections import deque
import json
import select
import socket
import threading

from acceptance_peer import AGENT, CAPABILITY, PROTOCOL, Mail, Model, transport

SUMMARY = "## Objective\nContinue the admitted scratch task.\n## Progress\nInitial packet verified; idle and busy mail read and replied.\n## Next\nFinish the restart delivery check.\n"


def text_frames(text):
    return "text/event-stream", ("data: " + json.dumps({"type": "response.output_text.delta", "delta": text})
        + '\n\ndata: {"type":"response.output_text.done"}\n\n'
        + 'data: {"type":"response.completed","response":{"usage":{"input_tokens":12,"output_tokens":8,"total_tokens":20}}}\n\n').encode()


class TuiModel(Model):
    def __init__(self):
        super().__init__()
        self.actions = deque()
        self.completed = []
        self.busy_entered = threading.Event()
        self.busy_release = threading.Event()

    def expect(self, *actions):
        with self.lock:
            self.actions.extend(actions)

    def mail(self, identifier):
        self.expect(("read", identifier), ("reply", identifier), ("complete", identifier))

    def dispatch(self, path, headers, body):
        # Reuse the bounded request recording and private-binding checks.
        super().dispatch(path, headers, body)
        with self.lock:
            if not self.actions:
                raise ValueError("unexpected extra model execution")
            action, identifier = self.actions.popleft()
            self.completed.append((action, identifier))
        serialized = json.dumps(body)
        if action != "summary" and "SEAT_PACKET_TASK" not in serialized:
            raise ValueError("model execution lost verified packet context")
        if action == "read":
            if identifier not in serialized:
                raise ValueError("mail input missing its delivery identity")
            name, arguments = "stateengine_stateengine_msg_read", {"msg_id": identifier}
        elif action == "reply":
            if "FULL_BODY_END_" + identifier not in serialized:
                raise ValueError("full message read did not reach the model")
            name, arguments = "stateengine_stateengine_msg_send", {
                "to": "agt_native_sender", "kind": "handoff", "body": "Read " + identifier}
        elif action == "complete":
            if "reply_" + identifier not in serialized:
                raise ValueError("mail reply receipt did not reach the model")
            return text_frames("ROW_A_DONE_" + identifier)
        elif action == "summary":
            if "summarizer" not in serialized and "Summarize the supplied conversation" not in serialized:
                raise ValueError("compaction did not issue a summary request")
            return text_frames(SUMMARY)
        elif action == "after-compact":
            summarized = any(isinstance(row.get("content"), str) and SUMMARY in row["content"]
                             for row in body.get("input", []))
            if "Model-written continuation summary" not in serialized or not summarized:
                raise ValueError("compaction summary did not reach the resumed coding request")
            return text_frames("ROW_A_COMPACT_DONE")
        elif action == "busy":
            self.busy_entered.set()
            if not self.busy_release.wait(timeout=20):
                raise ValueError("busy provider release timed out")
            return text_frames("ROW_A_BUSY_DONE")
        elif action == "initial":
            return text_frames("ROW_A_INITIAL_DONE")
        else:
            raise ValueError("unknown TUI model action")
        frames = transport.tool_call_frames(arguments, 0, name)
        return "text/event-stream", "".join(frame for frame, _delay in frames).encode()


class TuiMail(Mail):
    def __init__(self):
        super().__init__()
        self.rows = []

    def publish(self, identifier):
        if identifier not in ("msg_tui_idle", "msg_tui_busy", "msg_tui_restart"):
            raise ValueError("unknown synthetic message")
        with self.lock:
            if any(row["source_message"] == identifier for row in self.rows):
                raise ValueError("duplicate synthetic publication")
            if not self.rows:
                self.rows.append(self.row("quiet_fixture", "quiet"))
            self.rows.append(self.row(identifier, "wake"))
        self.changed.set()

    def dispatch(self, path, headers, body, connection=None):
        if path != "/" + PROTOCOL or headers.get("Authorization") != "Bearer " + CAPABILITY:
            raise ValueError("mail route or dummy binding")
        operation, args = body["operation"], body["args"]
        if operation == "next_change" and connection is not None:
            with self.lock:
                self.operations.append(operation)
            # A cancelled retained poll must release its fixture thread before
            # reconnect/shutdown. Waiting only on the advisory event leaves
            # cancelled clients occupying the bounded server's request slots.
            for _ in range(100):
                if self.changed.is_set():
                    self.changed.clear()
                    break
                if select.select([connection], [], [], 0.05)[0]:
                    if connection.recv(1, socket.MSG_PEEK) == b"":
                        raise ConnectionResetError("mail poll cancelled")
                    raise ValueError("unexpected bytes after mail request")
            return "application/json", json.dumps({"ok": True, "value": {"generation": 1, "token": None}}).encode()
        if operation not in ("fetch_snapshot_page", "acknowledge_delivery"):
            return super().dispatch(path, headers, body)
        with self.lock:
            self.operations.append(operation)
            if operation == "fetch_snapshot_page":
                start = 0
                if args["after"] is not None:
                    matches = [index for index, row in enumerate(self.rows) if row["source_cursor"] == args["after"]]
                    if len(matches) != 1:
                        raise ValueError("foreign mail cursor")
                    start = matches[0] + 1
                rows = self.rows[start:]
                value = {"messages": rows, "next_after": None, "refused_rows": [],
                    "last_cursor": rows[-1]["source_cursor"] if rows else None}
            else:
                ack = args["ack"]
                if ack["recipient"] != AGENT or ack["source_message"] not in [row["source_message"] for row in self.rows]:
                    raise ValueError("foreign mail acknowledgement")
                self.acks.append(ack["source_message"])
                value = {"tag": "Recorded", "receipt": {"ack_id": ack["ack_id"],
                    "source_message": ack["source_message"], "projects": ack["projects"]},
                    "primary_delivery": {"witnessed": True}, "floor_projection": {"state": "unarmed",
                        "outcome": "refused", "disposition": None, "reason": "secondary-unarmed",
                        "outcome_record": {"state": "recorded"}, "retry_required": False, "retry": None}}
        return "application/json", json.dumps({"ok": True, "value": value}).encode()
