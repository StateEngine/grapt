"""Watch a real product's `/api/event` stream while one prompt runs.

The minimal reproduction for the live-lifecycle question: does the product EMIT
execution lifecycle events on the subscription the native client opens, and do
they arrive? This speaks HTTP directly, so it answers that question without the
client in the way — if the events are not on this stream, no client change can
project them, and if they are, the fault is downstream of the socket.

It deliberately does NOT use the native client's transport: a probe that shared
the code under suspicion could not exonerate or convict it.

REDACTION: the service root's credential is read to authenticate and is never
logged, echoed or written to the receipt. Event bodies are recorded as received
because they are this run's own fixture content, but the receipt carries the
event TYPES and sizes up front so a reader can work from those alone.

Usage:
    python event-probe.py --service-root <root> --evidence <fresh dir>
                          [--prompt "..."]
"""

from __future__ import annotations

import argparse
import base64
import http.client
import json
import socket
import sys
import threading
import time
from pathlib import Path


class ProbeError(RuntimeError):
    """A refusal this script can name."""


def endpoint(service_root: Path) -> tuple[str, int]:
    receipt = json.loads((service_root / "serve-ready.json").read_text(encoding="utf-8"))
    raw = receipt["observed_endpoint"]
    rest = raw.split("://", 1)[1]
    host, _, port = rest.partition(":")
    return host, int(port)


def authorization(service_root: Path) -> str:
    record = (service_root / "basic.credential").read_text(encoding="utf-8").strip()
    return "Basic " + base64.b64encode(record.encode("utf-8")).decode("ascii")


def call(host: str, port: int, auth: str, method: str, path: str, body: object | None) -> dict:
    connection = http.client.HTTPConnection(host, port, timeout=30)
    payload = None if body is None else json.dumps(body)
    headers = {"Authorization": auth}
    if payload is not None:
        headers["Content-Type"] = "application/json"
    connection.request(method, path, payload, headers)
    response = connection.getresponse()
    raw = response.read()
    connection.close()
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        parsed = None
    return {"status": response.status, "body": parsed, "raw_bytes": len(raw)}


class Subscription(threading.Thread):
    """Hold `/api/event` open and record every frame, with arrival times."""

    def __init__(self, host: str, port: int, auth: str) -> None:
        super().__init__(daemon=True)
        self.host, self.port, self.auth = host, port, auth
        self.frames: list[dict] = []
        self.status: int | None = None
        self.error: str | None = None
        self.started_at = time.monotonic()
        self.stop = threading.Event()

    def run(self) -> None:
        try:
            sock = socket.create_connection((self.host, self.port), timeout=30)
            sock.settimeout(1.0)
            request = (
                f"GET /api/event HTTP/1.1\r\nHost: {self.host}:{self.port}\r\n"
                f"Authorization: {self.auth}\r\nAccept: text/event-stream\r\n"
                "Connection: keep-alive\r\n\r\n"
            )
            sock.sendall(request.encode("ascii"))
            buffer = b""
            # Read the head first so the status is known before any frame.
            while b"\r\n\r\n" not in buffer:
                chunk = sock.recv(4096)
                if not chunk:
                    raise ProbeError("the subscription closed before answering")
                buffer += chunk
            head, _, rest = buffer.partition(b"\r\n\r\n")
            self.status = int(head.split(b" ")[1])
            buffer = rest
            while not self.stop.is_set():
                # An SSE event ends at a blank line.
                while b"\n\n" in buffer:
                    raw, _, buffer = buffer.partition(b"\n\n")
                    self.record(raw.decode("utf-8", "replace"))
                try:
                    chunk = sock.recv(8192)
                except socket.timeout:
                    continue
                if not chunk:
                    break
                buffer += chunk
            sock.close()
        except Exception as error:  # noqa: BLE001 - a probe reports, never raises
            self.error = f"{type(error).__name__}: {error}"

    def record(self, raw: str) -> None:
        at = round(time.monotonic() - self.started_at, 3)
        data = "".join(
            line[len("data:") :].strip()
            for line in raw.splitlines()
            if line.startswith("data:")
        )
        if not data:
            return
        try:
            parsed = json.loads(data)
        except json.JSONDecodeError:
            self.frames.append({"at": at, "type": "<unparsed>", "bytes": len(data)})
            return
        kind = parsed.get("type") if isinstance(parsed, dict) else None
        self.frames.append(
            {"at": at, "type": kind, "bytes": len(data), "event": parsed}
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service-root", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument(
        "--prompt",
        default="Reply with the word STARTING and then count slowly from one to fifty, one number per line.",
    )
    parser.add_argument("--settle-seconds", type=float, default=25.0)
    parser.add_argument(
        "--interrupt-after-deltas",
        type=int,
        default=0,
        help="interrupt once this many text deltas have arrived; 0 never interrupts",
    )
    arguments = parser.parse_args()

    if arguments.evidence.exists():
        raise ProbeError(f"{arguments.evidence} already exists; a probe is recorded once")
    host, port = endpoint(arguments.service_root)
    auth = authorization(arguments.service_root)

    subscription = Subscription(host, port, auth)
    subscription.start()
    # The subscription must be ESTABLISHED before the prompt, or an absence of
    # events proves only that nobody was listening yet.
    time.sleep(2.0)
    if subscription.status is None:
        raise ProbeError(f"the subscription never answered: {subscription.error}")

    # The selection the product itself offers, read rather than assumed, so the
    # session is created exactly as the native client creates one.
    # These routes do not share one envelope: `/api/project` answers a bare
    # array whose rows carry `worktree`, while `/api/agent` and `/api/model`
    # answer `{location, data}`. Read both shapes rather than assume one.
    def answer(path: str) -> dict:
        got = call(host, port, auth, "GET", path, None)
        if got["status"] != 200:
            raise ProbeError(f"GET {path} answered {got['status']}")
        return got

    def rows(path: str) -> tuple[list, dict]:
        body = answer(path)["body"]
        if isinstance(body, list):
            return body, {}
        if isinstance(body, dict):
            data = body.get("data")
            return (data if isinstance(data, list) else []), (body.get("location") or {})
        raise ProbeError(f"GET {path} answered neither an array nor an object")

    projects, _ = rows("/api/project")
    if not projects:
        raise ProbeError("the product offers no project")
    agents, agent_location = rows("/api/agent")
    model_rows, model_location = rows("/api/model")
    models = [m for m in model_rows if m.get("enabled") is not False]
    # The directory the PRODUCT itself reports for the selection, not a path
    # this probe composed: a session created against a directory the product
    # spells differently is a different session.
    directory = (
        agent_location.get("directory")
        or model_location.get("directory")
        or projects[0].get("worktree")
        or projects[0].get("directory")
    )
    if not agents or not models or not directory:
        raise ProbeError("the product offers no agent, no model, or no directory")
    agent_id = agents[0].get("id")
    model_id = models[0].get("id")
    provider_id = models[0].get("providerID") or models[0].get("provider_id")

    session = call(
        host,
        port,
        auth,
        "POST",
        "/api/session",
        {
            "agent": agent_id,
            "model": {"id": model_id, "providerID": provider_id},
            "location": {"directory": directory},
        },
    )
    session_id = None
    if isinstance(session.get("body"), dict):
        data = session["body"].get("data")
        if isinstance(data, dict):
            session_id = data.get("id")
    if session_id is None:
        raise ProbeError(
            f"no session id: status {session['status']} body {json.dumps(session.get('body'))[:400]}"
        )

    before = len(subscription.frames)
    # The frozen prompt route and body, exactly as `wire::encode_prompt` builds
    # them: POST /api/session/{id}/prompt with {"text": ...}. An earlier
    # revision posted to /message and got 405, which proved only that the probe
    # was wrong.
    prompt = call(
        host,
        port,
        auth,
        "POST",
        f"/api/session/{session_id}/prompt",
        {"text": arguments.prompt},
    )
    # Optionally interrupt while the answer is genuinely streaming, to learn
    # which TERMINAL the product publishes for an interrupted execution. Waiting
    # for a text delta first means the interrupt lands on a live execution
    # rather than on one that has already settled.
    interrupt = None
    if arguments.interrupt_after_deltas > 0:
        for _ in range(600):
            deltas = sum(
                1 for f in subscription.frames if f.get("type") == "session.text.delta"
            )
            if deltas >= arguments.interrupt_after_deltas:
                break
            time.sleep(0.05)
        interrupt = call(
            host, port, auth, "POST", f"/api/session/{session_id}/interrupt", {}
        )
    time.sleep(arguments.settle_seconds)
    subscription.stop.set()
    subscription.join(timeout=10)

    frames = subscription.frames
    kinds: dict[str, int] = {}
    for frame in frames:
        kinds[str(frame.get("type"))] = kinds.get(str(frame.get("type")), 0) + 1

    arguments.evidence.mkdir(parents=True)
    receipt = {
        "contract": "native-tui-event-probe/v1",
        "product": {"host": host, "port": port, "service_root": str(arguments.service_root.resolve())},
        "session_id": session_id,
        "selection": {"agent": agent_id, "model": model_id, "provider": provider_id},
        "subscription": {
            "status": subscription.status,
            "error": subscription.error,
            "frames_total": len(frames),
            "frames_before_prompt": before,
            "frames_after_prompt": len(frames) - before,
        },
        "prompt_call": {"status": prompt["status"], "bytes": prompt["raw_bytes"]},
        "interrupt_call": None if interrupt is None else {"status": interrupt["status"]},
        "event_types": kinds,
        "note": (
            "The subscription is opened and confirmed BEFORE the prompt is sent, so an "
            "absence of lifecycle events cannot be explained by nobody listening yet. "
            "The credential is never logged."
        ),
    }
    (arguments.evidence / "receipt.json").write_text(
        json.dumps(receipt, indent=2), encoding="utf-8"
    )
    (arguments.evidence / "frames.json").write_text(
        json.dumps(frames, indent=2), encoding="utf-8"
    )
    print(json.dumps(receipt, indent=2))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ProbeError as error:
        print(f"native-tui event probe: {error}", file=sys.stderr)
        sys.exit(2)
