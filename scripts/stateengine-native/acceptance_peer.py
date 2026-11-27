"""Bounded, disposable model/mail peers for the headless seat acceptance driver.

Only dummy capabilities are accepted. This is not a StateEngine implementation.
The mail shapes follow the existing product native_mail HTTP fixtures.
"""
import importlib.util
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import select
import socket
import threading

CAPABILITY = "NATIVE_FIXTURE_CAPABILITY"
PRIVATE_CURSOR = "NATIVE_FIXTURE_PRIVATE_CURSOR"
PROJECT = "native-fixture"
AGENT = "agt_native_fixture"
ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = "remote-seat-mail-transport/v2"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


transport = load("seat_acceptance_transport", ROOT / "scripts/native-tui/loopback-provider.py")


class Server(ThreadingHTTPServer):
    """At most four requests in flight, 128 total and 8 MiB per request."""
    def __init__(self, port, dispatch, with_connection=False):
        self.dispatch = dispatch
        self.with_connection = with_connection
        self.slots = threading.BoundedSemaphore(4)
        self.lock = threading.Lock()
        self.errors = []
        self.requests = 0
        super().__init__(("127.0.0.1", port), Handler)
        self.thread = threading.Thread(target=self.serve_forever, kwargs={"poll_interval": 0.05})

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            request.close()
            with self.lock:
                self.errors.append("concurrent request bound")
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            super().process_request_thread(request, address)
        finally:
            self.slots.release()

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.shutdown()
        self.server_close()
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise RuntimeError("fixture server did not join")

    @property
    def endpoint(self):
        return f"http://127.0.0.1:{self.server_port}"


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        super().setup()
        self.connection.settimeout(3)

    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.send_error(405, "streaming GET is not supported")

    def do_POST(self):
        try:
            with self.server.lock:
                self.server.requests += 1
                if self.server.requests > 128:
                    raise ValueError("request count bound")
            length = int(self.headers.get("Content-Length", "-1"))
            if not 0 < length <= 8 * 1024 * 1024:
                raise ValueError("request byte bound")
            body = self.rfile.read(length)
            if len(body) != length:
                raise ValueError("truncated request")
            arguments = [self.path, self.headers, json.loads(body)]
            if self.server.with_connection:
                arguments.append(self.connection)
            content_type, payload = self.server.dispatch(*arguments)
            self.send_response(202 if content_type is None else 200)
            if content_type is not None:
                self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            # Service shutdown cancels its retained long poll.
            pass
        except Exception as error:
            with self.server.lock:
                self.server.errors.append(str(error))
            self.send_error(400, "fixture request refused")


class Model:
    def __init__(self):
        self.bodies = []
        self.lock = threading.Lock()

    def dispatch(self, path, headers, body):
        if path != "/responses" or headers.get("Authorization") != "Bearer disposable-model-dummy":
            raise ValueError("unexpected model route or dummy credential")
        serialized = json.dumps(body)
        if CAPABILITY in serialized or PRIVATE_CURSOR in serialized:
            raise ValueError("private binding reached model")
        with self.lock:
            if len(self.bodies) >= 32:
                raise ValueError("model call bound")
            self.bodies.append(body)
        rows = body.get("input", [])
        # Read/send are real product MCP calls; the response is scripted.
        if "msg_native_long" in serialized and "FULL_BODY_END" not in serialized:
            frames = transport.tool_call_frames({"msg_id": "msg_native_long"}, 0,
                                                "stateengine_stateengine_msg_read")
        elif "FULL_BODY_END" in serialized and "msg_native_reply" not in serialized:
            frames = transport.tool_call_frames({"to": "agt_native_sender", "kind": "handoff",
                "body": "Full message read; fixture reply."}, 0, "stateengine_stateengine_msg_send")
        else:
            text = "ROW_A_MAIL_DONE" if "msg_native_reply" in serialized else "ROW_A_PACKET_DONE"
            frames = [
                ("data: " + json.dumps({"type": "response.output_text.delta", "delta": text}) + "\n\n", 0),
                ('data: {"type":"response.output_text.done"}\n\n', 0),
                ('data: {"type":"response.completed","response":{"usage":{"input_tokens":12,"output_tokens":8,"total_tokens":20}}}\n\n', 0),
            ]
        if not isinstance(rows, list):
            raise ValueError("model input shape")
        return "text/event-stream", "".join(frame for frame, _delay in frames).encode()

    def count(self):
        with self.lock:
            return len(self.bodies)


class Mail:
    def __init__(self):
        self.operations = []
        self.acks = []
        self.armed = False
        self.changed = threading.Event()
        self.lock = threading.Lock()
        self.limits = json.loads((ROOT / "fixtures/stateengine-native/stateengine-mail-profile.json").read_text())["limits"]

    def signal(self):
        self.armed = True
        self.changed.set()

    def count(self, operation):
        with self.lock:
            return self.operations.count(operation)

    def row(self, message, attention):
        return {"protocol": "remote-seat-mail-transport/v1", "source_binding": "bind-native",
            "projects": {"source": PROJECT, "target": "secode"}, "source_message": message,
            "source_cursor": "cursor-" + message, "sender": "agt_native_sender", "recipient": AGENT,
            "kind": "handoff", "body": "Read the full fixture message through MCP.", "refs": [],
            "supersession": {"supersedes": None, "superseded_by": None},
            "archive": {"disposition": "active", "reason": None},
            "source_created": "2026-10-25T00:00:00.000Z", "attention": attention}

    def dispatch(self, path, headers, body, connection=None):
        if path != "/" + PROTOCOL or headers.get("Authorization") != "Bearer " + CAPABILITY:
            raise ValueError("mail route or dummy binding")
        operation, args = body["operation"], body["args"]
        with self.lock:
            self.operations.append(operation)
        if operation == "open":
            request = args["request"]
            if (request["requested_protocol"] != PROTOCOL or request["expected_recipient"] != AGENT
                    or request["expected_local_project"] != "secode"):
                raise ValueError("mail protocol or expected principal")
            value = {"binding_id": "bind-native", "protocol": PROTOCOL,
                "adapter_version": "stateengine-native-mail/2", "projects": {"source": PROJECT, "target": "secode"},
                "verified_principal": AGENT, "local_recipient": AGENT, "mapping_version": "exact-seat-id/v1",
                "takeover": {"generation": 1, "displaced": None}, "limits": self.limits,
                "capabilities": {"atomic_subscribe_and_snapshot": True, "durable_delivery_ack": True,
                    "bulk_dismiss": True, "explicit_project_pair": True, "opaque_authenticated_cursors": True,
                    "advisory_payload_bytes": 0, "coalesced_pending_signals": 1}}
        elif operation == "subscribe_and_snapshot":
            value = {"subscription_id": "sub-native", "generation": 1, "snapshot": {
                "snapshot_id": "snap-native", "barrier": "barrier-native",
                "start_after": args["resume"]["from_checkpoint"], "retained_origin": "retained-origin/v1"}}
        elif operation == "fetch_snapshot_page":
            messages = [self.row("quiet_fixture", "quiet"), self.row("msg_native_long", "wake")] if self.armed and args["after"] is None else []
            value = {"messages": messages, "next_after": None, "refused_rows": [],
                     "last_cursor": "cursor-msg_native_long" if messages else None}
        elif operation == "acknowledge_delivery":
            ack = args["ack"]
            if ack["recipient"] != AGENT or ack["source_message"] not in ("quiet_fixture", "msg_native_long"):
                raise ValueError("foreign mail acknowledgement")
            with self.lock:
                self.acks.append(ack["source_message"])
            value = {"tag": "Recorded", "receipt": {"ack_id": ack["ack_id"],
                "source_message": ack["source_message"], "projects": ack["projects"]},
                "primary_delivery": {"witnessed": True}, "floor_projection": {"state": "unarmed",
                    "outcome": "refused", "disposition": None, "reason": "secondary-unarmed",
                    "outcome_record": {"state": "recorded"}, "retry_required": False, "retry": None}}
        elif operation == "next_change":
            if connection is None:
                self.changed.wait(timeout=1)
            else:
                # Rebinding cancels a retained poll. Release its bounded slot
                # on EOF, as the native-TUI peer already does.
                for _ in range(20):
                    if self.changed.is_set():
                        break
                    if select.select([connection], [], [], 0.05)[0]:
                        if connection.recv(1, socket.MSG_PEEK) == b"":
                            raise ConnectionResetError("mail poll cancelled")
                        raise ValueError("unexpected bytes after mail request")
            self.changed.clear()
            value = {"generation": 1, "token": None}
        elif operation == "close":
            value = {"joined_tasks": 0, "ticks_used": 1}
        else:
            raise ValueError("unexpected mail operation")
        return "application/json", json.dumps({"ok": True, "value": value}).encode()
