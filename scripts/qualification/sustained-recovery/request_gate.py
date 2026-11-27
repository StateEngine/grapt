"""Versioned experiment relay with bounded, secret-free transport evidence.

The relay makes exactly one upstream request for each admitted downstream
request. A transport failure ends that physical call but does not close the
global experiment budget; only the call cap, deadline, or controller teardown
does that. The product remains the sole owner of retry and visible-effect
policy.
"""

from dataclasses import dataclass
import argparse
import hashlib
import http.client
import http.server
import io
import json
import socket
import ssl
import threading
import time


REQUEST_BYTES = 4 * 1024 * 1024
RESPONSE_BYTES = 16 * 1024 * 1024
CHUNK_BYTES = 16 * 1024


@dataclass(frozen=True)
class Admission:
    identity: str
    ordinal: int
    elapsed_ms: int


@dataclass(frozen=True)
class Refusal:
    reason: str
    elapsed_ms: int


class GlobalDeadlineExceeded(TimeoutError):
    """The one experiment deadline, distinct from an operation timeout."""


class CooperativeSocketIO(io.RawIOBase):
    """Binary file view that keeps http.client on the interruptible socket path."""

    def __init__(self, held, mode):
        super().__init__()
        self.held = held
        self.mode = mode
        self.released = False

    def readable(self):
        return self.mode == "r"

    def writable(self):
        return self.mode == "w"

    def readinto(self, buffer):
        if not self.readable():
            raise io.UnsupportedOperation("not readable")
        return self.held.recv_into(buffer)

    def write(self, buffer):
        if not self.writable():
            raise io.UnsupportedOperation("not writable")
        self.held.sendall(buffer)
        return len(buffer)

    def fileno(self):
        return self.held.fileno()

    def close(self):
        try:
            super().close()
        finally:
            if not self.released:
                self.released = True
                self.held.release_file()


class CooperativeSocket:
    """A socket whose I/O observes controller interruption within 50 ms."""

    POLL_SECONDS = 0.05

    def __init__(self, gate, held):
        self.gate = gate
        self.held = held
        self.interrupted = threading.Event()
        self.read_lock = threading.Lock()
        self.read_changed = threading.Condition(self.read_lock)
        self.read_count = 0
        self.read_phase = None
        self.read_inflight = False
        self.lifetime_lock = threading.Lock()
        self.owner_open = True
        self.file_count = 0
        self.transport_closed = False

    def set_read_phase(self, phase):
        with self.read_changed:
            self.read_phase = phase
            self.read_changed.notify_all()

    def _deadline(self):
        return time.monotonic() + self.gate.operation_timeout()

    def _timeout(self, deadline):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise socket.timeout("timed out")
        return min(self.POLL_SECONDS, remaining)

    def recv_into(self, buffer):
        with self.read_changed:
            self.read_count += 1
            self.read_inflight = True
            self.read_changed.notify_all()
        deadline = self._deadline()
        try:
            while not self.interrupted.is_set():
                self.held.settimeout(self._timeout(deadline))
                try:
                    return self.held.recv_into(buffer)
                except socket.timeout:
                    continue
            raise OSError("socket interrupted by controller teardown")
        finally:
            with self.read_changed:
                self.read_inflight = False
                self.read_changed.notify_all()

    def sendall(self, data):
        deadline = self._deadline()
        view = memoryview(data)
        sent = 0
        while sent < len(view) and not self.interrupted.is_set():
            self.held.settimeout(self._timeout(deadline))
            try:
                written = self.held.send(view[sent:])
            except socket.timeout:
                continue
            if written <= 0:
                raise OSError("socket send made no progress")
            sent += written
        if sent < len(view):
            raise OSError("socket interrupted by controller teardown")

    def makefile(self, mode, buffering=None, *_args, **_kwargs):
        if mode not in ("rb", "wb"):
            raise ValueError("relay sockets expose binary files only")
        with self.lifetime_lock:
            if not self.owner_open or self.transport_closed:
                raise OSError("socket is closed")
            self.file_count += 1
        try:
            raw = CooperativeSocketIO(self, mode[0])
            if buffering == 0:
                return raw
            if mode == "rb":
                return io.BufferedReader(raw)
            return io.BufferedWriter(raw)
        except BaseException:
            self.release_file()
            raise

    def release_file(self):
        close_transport = False
        with self.lifetime_lock:
            if self.file_count <= 0:
                raise RuntimeError("socket file lifetime underflow")
            self.file_count -= 1
            if self.file_count == 0 and not self.owner_open and not self.transport_closed:
                self.transport_closed = True
                close_transport = True
        if close_transport:
            self.held.close()

    def _close_transport(self):
        close_transport = False
        with self.lifetime_lock:
            if not self.transport_closed:
                self.transport_closed = True
                close_transport = True
        if close_transport:
            return self.held.close()
        return None

    def shutdown(self, how):
        self.interrupted.set()
        try:
            return self.held.shutdown(how)
        finally:
            self._close_transport()

    def close(self):
        close_transport = False
        with self.lifetime_lock:
            if self.owner_open:
                self.owner_open = False
                if self.file_count == 0 and not self.transport_closed:
                    self.transport_closed = True
                    close_transport = True
        if close_transport:
            return self.held.close()
        return None

    def fileno(self):
        return self.held.fileno()

    def __getattr__(self, name):
        return getattr(self.held, name)


class Budget:
    """One armed, lock-serialized total-call and elapsed-time budget."""

    def __init__(self, maximum=64, seconds=900, clock=time.monotonic):
        if maximum < 1 or seconds <= 0:
            raise ValueError("budget bounds must be positive")
        self.maximum = maximum
        self.seconds = seconds
        self.clock = clock
        self.started = None
        self.calls = 0
        self.closed_reason = None
        self.lock = threading.Lock()

    def arm(self):
        with self.lock:
            if self.started is not None or self.closed_reason is not None:
                raise RuntimeError("budget cannot be rearmed")
            self.started = self.clock()

    def acquire(self):
        with self.lock:
            if self.started is None:
                return Refusal("not-armed", 0)
            elapsed = max(0.0, self.clock() - self.started)
            elapsed_ms = int(elapsed * 1000)
            if self.closed_reason is not None:
                return Refusal(self.closed_reason, elapsed_ms)
            if elapsed >= self.seconds:
                self.closed_reason = "deadline"
                return Refusal("deadline", elapsed_ms)
            if self.calls >= self.maximum:
                self.closed_reason = "call-cap"
                return Refusal("call-cap", elapsed_ms)
            self.calls += 1
            return Admission(f"relay-call-{self.calls:02d}", self.calls, elapsed_ms)

    def close(self, reason="controller-teardown"):
        with self.lock:
            if self.closed_reason is None:
                self.closed_reason = reason

    def remaining(self):
        """Return absolute remaining seconds and close truthfully at expiry."""
        with self.lock:
            if self.started is None:
                return 0.0
            elapsed = max(0.0, self.clock() - self.started)
            if self.closed_reason is None and elapsed >= self.seconds:
                self.closed_reason = "deadline"
            if self.closed_reason is not None:
                return 0.0
            return max(0.0, self.seconds - elapsed)


class Gate(http.server.HTTPServer):
    """Serial relay: at most one downstream and one upstream call are live."""

    allow_reuse_address = False

    def __init__(
        self,
        address,
        budget,
        log,
        upstream,
        *,
        certificate=None,
        key=None,
        upstream_tls=False,
        upstream_context=None,
        transport_timeout=60,
        clock=time.monotonic,
        model="gpt-5.6-sol",
    ):
        self.budget = budget
        self.log = log
        self.upstream = upstream
        self.upstream_tls = upstream_tls
        self.upstream_context = upstream_context
        self.transport_timeout = transport_timeout
        self.clock = clock
        self.model = model
        self.record_lock = threading.Lock()
        self.active_lock = threading.Lock()
        self.active = set()
        super().__init__(address, Handler)
        if (certificate is None) != (key is None):
            self.server_close()
            raise ValueError("certificate and key must be supplied together")
        if certificate is not None:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.load_cert_chain(certificate, key)
            self.socket = context.wrap_socket(self.socket, server_side=True)

    def record(self, record):
        encoded = json.dumps({"schema": "sustained-recovery-relay/v1", **record}, sort_keys=True)
        if len(encoded) > 4096:
            raise ValueError("relay diagnostic exceeded its bound")
        with self.record_lock:
            self.log.write(encoded + "\n")
            self.log.flush()

    def operation_timeout(self):
        remaining = self.budget.remaining()
        if remaining <= 0:
            raise GlobalDeadlineExceeded("experiment deadline elapsed")
        return min(self.transport_timeout, remaining)

    def register(self, held):
        with self.active_lock:
            self.active.add(held)

    def unregister(self, held):
        with self.active_lock:
            self.active.discard(held)

    def interrupt_active(self):
        with self.active_lock:
            active = list(self.active)
        for held in active:
            try:
                held.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                held.close()
            except OSError:
                pass

    def shutdown(self):
        self.budget.close()
        self.interrupt_active()
        super().shutdown()

    def serve_forever(self, poll_interval=0.05):
        super().serve_forever(poll_interval=poll_interval)

    def server_close(self):
        self.interrupt_active()
        super().server_close()

    def handle_error(self, request, client_address):
        if self.budget.closed_reason != "controller-teardown":
            super().handle_error(request, client_address)

    def connection(self):
        host, port = self.upstream
        timeout = self.operation_timeout()
        if self.upstream_tls:
            context = self.upstream_context or ssl.create_default_context()
            return http.client.HTTPSConnection(
                host, port, timeout=timeout, context=context
            )
        return http.client.HTTPConnection(host, port, timeout=timeout)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def setup(self):
        self.request = CooperativeSocket(self.server, self.request)
        self.connection = self.request
        self.server.register(self.request)
        try:
            super().setup()
        except BaseException:
            self.server.unregister(self.request)
            raise

    def finish(self):
        try:
            super().finish()
        finally:
            self.server.unregister(self.request)

    def _send_empty(self, status):
        self.send_response(status)
        self.send_header("Content-Length", "0")
        self.send_header("Connection", "close")
        self.end_headers()

    def do_POST(self):
        self.close_connection = True
        started = self.server.clock()
        admission = None
        connection = None
        response = None
        upstream_socket = None
        phase = "downstream-request"
        request_bytes = 0
        response_bytes = 0
        http_status = None
        response_started = False
        try:
            if self.path not in ("/backend-api/codex/responses", "/responses"):
                self.server.record({"outcome": "request-refused", "reason": "wrong-path"})
                self._send_empty(404)
                return
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= REQUEST_BYTES:
                self.server.record({"outcome": "request-refused", "reason": "body-bound"})
                self._send_empty(413)
                return
            body = self.rfile.read(length)
            request_bytes = len(body)
            if request_bytes != length:
                raise ValueError("incomplete-request")
            data = json.loads(body)
            if data.get("model") != self.server.model:
                self.server.record({"outcome": "request-refused", "reason": "model-mismatch"})
                self._send_empty(400)
                return
            controls = {
                key: hashlib.sha256(
                    json.dumps(data.get(key), sort_keys=True).encode("utf-8")
                ).hexdigest()
                for key in ("instructions", "tools", "reasoning")
            }
            phase = "budget-admission"
            admission = self.server.budget.acquire()
            if isinstance(admission, Refusal):
                self.server.record(
                    {
                        "outcome": "budget-refused",
                        "reason": admission.reason,
                        "elapsed_ms": admission.elapsed_ms,
                        "request_bytes": request_bytes,
                        "control_sha256": controls,
                    }
                )
                self._send_empty(429)
                return
            self.server.record(
                {
                    "call": admission.identity,
                    "ordinal": admission.ordinal,
                    "outcome": "admitted",
                    "elapsed_ms": admission.elapsed_ms,
                    "request_bytes": request_bytes,
                    "control_sha256": controls,
                }
            )
            connection = self.server.connection()
            phase = "upstream-connect"
            connection.connect()
            raw_upstream_socket = connection.sock
            if raw_upstream_socket is None:
                raise ConnectionError("upstream connection opened without a socket")
            upstream_socket = CooperativeSocket(self.server, raw_upstream_socket)
            connection.sock = upstream_socket
            self.server.register(upstream_socket)
            headers = {
                name: value
                for name, value in self.headers.items()
                if name.lower()
                not in ("host", "connection", "content-length", "transfer-encoding")
            }
            headers["Connection"] = "close"
            phase = "upstream-request"
            connection.request("POST", self.path, body=body, headers=headers)
            phase = "upstream-response-head"
            upstream_socket.set_read_phase(phase)
            response = connection.getresponse()
            http_status = response.status
            expected = response.getheader("Content-Length")
            expected = int(expected) if expected is not None else None
            if expected is not None and not 0 <= expected <= RESPONSE_BYTES:
                raise ValueError("response-bound")
            phase = "downstream-response-head"
            self.send_response(response.status)
            self.send_header(
                "Content-Type", response.getheader("Content-Type", "text/event-stream")
            )
            self.send_header("Connection", "close")
            self.end_headers()
            response_started = True
            while True:
                phase = "upstream-response-stream"
                upstream_socket.set_read_phase(phase)
                chunk = response.read1(CHUNK_BYTES)
                if not chunk:
                    break
                response_bytes += len(chunk)
                if response_bytes > RESPONSE_BYTES:
                    raise ValueError("response-bound")
                phase = "downstream-response-stream"
                self.wfile.write(chunk)
                self.wfile.flush()
            if expected is not None and response_bytes != expected:
                raise http.client.IncompleteRead(b"", expected - response_bytes)
            self.server.record(
                {
                    "call": admission.identity,
                    "ordinal": admission.ordinal,
                    "outcome": "response-ended",
                    "phase": "complete",
                    "http_status": http_status,
                    "request_bytes": request_bytes,
                    "response_bytes": response_bytes,
                    "elapsed_ms": int(max(0.0, self.server.clock() - started) * 1000),
                }
            )
        except Exception as error:  # metadata only; messages can contain secrets
            self.server.budget.remaining()
            if self.server.budget.closed_reason == "deadline":
                error = GlobalDeadlineExceeded("experiment deadline elapsed")
            record = {
                "outcome": "transport-failed" if admission is not None else "request-failed",
                "phase": phase,
                "error_type": type(error).__name__,
                "request_bytes": request_bytes,
                "response_bytes": response_bytes,
                "elapsed_ms": int(max(0.0, self.server.clock() - started) * 1000),
            }
            if isinstance(admission, Admission):
                record.update({"call": admission.identity, "ordinal": admission.ordinal})
            if http_status is not None:
                record["http_status"] = http_status
            self.server.record(record)
            if not response_started:
                try:
                    self._send_empty(504 if isinstance(error, socket.timeout) else 502)
                except OSError:
                    pass
        finally:
            if response is not None:
                response.close()
            if connection is not None:
                try:
                    connection.close()
                finally:
                    if upstream_socket is not None:
                        self.server.unregister(upstream_socket)


def controls():
    """Cheap zero-network cap/deadline/concurrency controls."""
    from concurrent.futures import ThreadPoolExecutor

    clock = [100.0]
    budget = Budget(clock=lambda: clock[0])
    assert isinstance(budget.acquire(), Refusal)
    budget.arm()
    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(lambda _index: budget.acquire(), range(100)))
    admitted = sorted(row.ordinal for row in results if isinstance(row, Admission))
    assert admitted == list(range(1, 65))
    assert budget.acquire() == Refusal("call-cap", 0)
    timer = Budget(clock=lambda: clock[0])
    timer.arm()
    clock[0] += 900
    assert timer.acquire() == Refusal("deadline", 900_000)
    print(json.dumps({"controls": "passed", "calls": 64, "deadline_ms": 900_000}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", nargs="?", choices=("controls", "serve"), default="controls")
    parser.add_argument("--listen", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=19473)
    parser.add_argument("--certificate")
    parser.add_argument("--key")
    parser.add_argument("--plain-http", action="store_true")
    parser.add_argument("--upstream-host", default="chatgpt.com")
    parser.add_argument("--upstream-port", type=int, default=443)
    parser.add_argument("--upstream-tls", action="store_true")
    parser.add_argument("--transport-timeout", type=float, default=60)
    parser.add_argument("--maximum-calls", type=int, default=64)
    parser.add_argument("--maximum-seconds", type=float, default=900)
    parser.add_argument("--model", default="gpt-5.6-sol")
    parser.add_argument("--log")
    parser.add_argument("--announce-ready", action="store_true")
    arguments = parser.parse_args()
    if arguments.action == "controls":
        controls()
        return
    if arguments.log is None:
        parser.error("serve requires --log")
    if arguments.plain_http:
        if arguments.certificate is not None or arguments.key is not None:
            parser.error("--plain-http cannot be combined with a certificate or key")
        try:
            listen = socket.gethostbyname(arguments.listen)
            upstream = socket.gethostbyname(arguments.upstream_host)
        except OSError as error:
            parser.error(f"plain-http address resolution failed: {type(error).__name__}")
        if not listen.startswith("127.") or not upstream.startswith("127."):
            parser.error("--plain-http is restricted to a loopback downstream and upstream")
    elif arguments.certificate is None or arguments.key is None:
        parser.error("serve requires --certificate and --key unless --plain-http is used")
    budget = Budget(arguments.maximum_calls, arguments.maximum_seconds)
    budget.arm()
    with open(arguments.log, "x", encoding="utf-8") as log:
        gate = Gate(
            (arguments.listen, arguments.port),
            budget,
            log,
            (arguments.upstream_host, arguments.upstream_port),
            certificate=arguments.certificate,
            key=arguments.key,
            upstream_tls=arguments.upstream_tls,
            transport_timeout=arguments.transport_timeout,
            model=arguments.model,
        )
        try:
            if arguments.announce_ready:
                print("RELAY_READY", flush=True)
            gate.serve_forever()
        finally:
            budget.close()
            gate.server_close()


if __name__ == "__main__":
    main()
