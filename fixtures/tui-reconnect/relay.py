"""Finite loopback relay: close SSE on compaction continuation or status edges,
delay read heads."""

import argparse
import base64
import json
import os
import socket
import threading
import time
from pathlib import Path


def write_receipt(path, value):
    """Publish complete JSON; a polling reader must never see a partial write."""
    temporary = path.with_name("." + path.name + ".tmp")
    try:
        with temporary.open("w", encoding="utf-8") as stream:
            stream.write(json.dumps(value) + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


class Relay:
    def __init__(self, upstream, mode, output):
        host, port = upstream.split(":")
        if host != "127.0.0.1":
            raise ValueError("fixture upstream must be loopback")
        self.upstream = (host, int(port))
        self.mode = mode
        self.output = Path(output)
        self.output.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self.armed = threading.Event()
        self.scheduled = threading.Event()
        self.streams = []
        self.terminal_delay_used = False
        self.event_delay_used = False
        self.message_delay_used = False
        self.statusgap_busy_cut = threading.Event()
        self.statusgap_idle_cut = threading.Event()
        self.started = time.monotonic()

    def record(self, event, **fields):
        with self.lock:
            with (self.output / "relay.jsonl").open("a", encoding="utf-8") as log:
                log.write(json.dumps({"seconds": time.monotonic() - self.started,
                                      "event": event, **fields}) + "\n")

    @staticmethod
    def read_request(client):
        raw = bytearray()
        for _ in range(128):
            chunk = client.recv(4096)
            if not chunk:
                raise OSError("request closed before completion")
            raw.extend(chunk)
            if len(raw) > 1024 * 1024:
                raise ValueError("request bound")
            if b"\r\n\r\n" in raw:
                head, body = raw.split(b"\r\n\r\n", 1)
                size = next((int(line.split(b":", 1)[1]) for line in head.split(b"\r\n")
                             if line.lower().startswith(b"content-length:")), 0)
                if len(body) >= size:
                    method, path, _ = head.split(b"\r\n", 1)[0].decode().split(" ")
                    return bytes(raw), method, path.split("?", 1)[0]
        raise ValueError("request iteration bound")

    def delay(self, method, path):
        if self.mode in ("passthrough", "statusgap"):
            # The gap is the status-edge cut itself; no read head is delayed.
            return 0
        if self.mode == "preset":
            return 2 if path.endswith("/preset") else 0
        if not self.armed.is_set():
            return 0
        if self.mode == "terminal":
            if method == "GET" and path.endswith("/message"):
                with self.lock:
                    if not self.terminal_delay_used:
                        self.terminal_delay_used = True
                        return 4
            return 0
        if self.mode == "compact-finish":
            # One FINITE read-head gap per channel: the first head after the
            # compaction continuation cuts the stream is delayed, then the
            # client's recovery reads pass, so the continuation can actually
            # reach the TUI after the gap.
            if path == "/api/event":
                with self.lock:
                    if not self.event_delay_used:
                        self.event_delay_used = True
                        return 6
                return 0
            if method == "GET" and path.endswith("/message"):
                with self.lock:
                    if not self.message_delay_used:
                        self.message_delay_used = True
                        return 6
                return 0
            return 0
        if path == "/api/health" and self.mode == "health":
            return 6
        if path == "/api/event":
            return {"event": 6, "both": 6, "message": 0, "slow": 6, "health": 0}[self.mode]
        if method == "GET" and path.endswith("/message"):
            return {"event": 0, "both": 6, "message": 6, "slow": 1, "health": 0}[self.mode]
        return 0

    def handle(self, client, number):
        try:
            with client:
                client.settimeout(8)
                raw, method, path = self.read_request(client)
                self.record("request", number=number, method=method, path=path,
                            armed=self.armed.is_set())
                if method == "POST" and path.endswith("/compact"):
                    self.scheduled.set()
                if (method == "POST" and path.endswith("/prompt")
                        and (self.mode in ("terminal", "statusgap")
                             or self.scheduled.is_set())
                        and not self.armed.is_set()):
                    self.armed.set()
                    self.record("fault_armed", number=number)
                    with self.lock:
                        old = list(self.streams)
                    for stream in ([] if self.mode in ("terminal", "statusgap") else old):
                        try:
                            stream.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass
                with socket.create_connection(self.upstream, timeout=2) as target:
                    target.settimeout(20)
                    target.sendall(raw)
                    if path == "/api/event":
                        with self.lock:
                            self.streams.append(client)
                    delay = self.delay(method, path)
                    if delay:
                        self.record("delay_begin", number=number, path=path, delay=delay)
                        time.sleep(delay)
                        self.record("delay_end", number=number, path=path)
                    total = 0
                    tail = b""
                    status_buf = bytearray()
                    for _ in range(1024):
                        chunk = target.recv(8192)
                        if not chunk:
                            break
                        total += len(chunk)
                        if total > 8 * 1024 * 1024:
                            raise ValueError("response bound")
                        tail = (tail + chunk)[-32768:]
                        if b'"type":"session.compaction.ended"' in tail:
                            self.record("compaction_ended", number=number)
                            tail = b""
                        if path == "/api/event" and self.mode in ("passthrough", "statusgap"):
                            status_buf.extend(chunk)
                            if len(status_buf) > 65536:
                                del status_buf[: len(status_buf) - 65536]
                            cut = self.observe_status(number, status_buf,
                                                      self.mode == "statusgap")
                            if cut:
                                # The edge frame never reaches the client: the
                                # resubscribing client reads the status from
                                # hydration, which is exactly what is under test.
                                self.record("stream_cut", number=number, edge=cut)
                                try:
                                    client.shutdown(socket.SHUT_RDWR)
                                except OSError:
                                    pass
                                break
                        client.sendall(chunk)
                self.record("response_end", number=number, path=path)
        except (OSError, ValueError) as error:
            self.record("connection_end", number=number, error=str(error))
        finally:
            with self.lock:
                if client in self.streams:
                    self.streams.remove(client)

    def observe_status(self, number, buf, cut_enabled):
        """Consume complete SSE frames from buf; record every session.status
        edge, and in statusgap mode cut the stream on the FIRST busy and the
        FIRST idle edge (one-shot per direction). Returns the edge that caused
        a cut, or None when no cut is due."""
        cut = None
        while b"\n\n" in buf:
            segment, rest = bytes(buf).split(b"\n\n", 1)
            del buf[: len(segment) + 2]
            edge = self.status_edge(segment)
            if edge is None:
                continue
            self.record("status_edge", number=number, status=edge)
            if not cut_enabled:
                continue
            if edge == "busy" and not self.statusgap_busy_cut.is_set():
                self.statusgap_busy_cut.set()
                cut = "busy"
            elif edge == "idle" and not self.statusgap_idle_cut.is_set():
                self.statusgap_idle_cut.set()
                cut = "idle"
            if cut:
                break
        return cut

    @staticmethod
    def status_edge(segment):
        """The session.status kind on one SSE segment, if it carries one.
        Anything the frozen record does not declare stays unobserved."""
        for line in segment.split(b"\n"):
            if not line.startswith(b"data: "):
                continue
            try:
                event = json.loads(line[len(b"data: "):])
            except ValueError:
                continue
            if event.get("type") != "session.status":
                continue
            status = event.get("data", {}).get("status", {}).get("type")
            if status in ("busy", "idle", "retry"):
                return status
        return None

    def health(self):
        token = base64.b64encode(b"alpha:zephyr").decode()
        with socket.create_connection(self.upstream, timeout=2) as stream:
            stream.sendall((f"GET /api/health HTTP/1.1\r\nHost: {self.upstream[0]}\r\n"
                            f"Authorization: Basic {token}\r\nConnection: close\r\n\r\n").encode())
            data = bytearray()
            for _ in range(128):
                chunk = stream.recv(4096)
                if not chunk:
                    break
                data.extend(chunk)
                if len(data) > 16384:
                    raise ValueError("health response bound")
        return json.loads(data.split(b"\r\n\r\n", 1)[1])

    def run(self):
        for name in ("health.json", "ready.json"):
            stale = self.output / name
            if stale.exists():
                stale.unlink()
        write_receipt(self.output / "health.json", self.health())
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(16)
            listener.settimeout(0.1)
            write_receipt(self.output / "ready.json", {
                "address": f"127.0.0.1:{listener.getsockname()[1]}"})
            for number in range(1, 129):
                if time.monotonic() - self.started > 45:
                    break
                for _ in range(450):
                    if time.monotonic() - self.started > 45:
                        return
                    try:
                        client, _ = listener.accept()
                        break
                    except TimeoutError:
                        continue
                else:
                    return
                threading.Thread(target=self.handle, args=(client, number), daemon=True).start()
        self.record("relay_end")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("upstream")
    parser.add_argument(
        "mode",
        choices=(
            "event", "both", "message", "slow", "health", "terminal", "preset",
            "compact-finish", "passthrough", "statusgap",
        ),
    )
    parser.add_argument("output")
    args = parser.parse_args()
    Relay(args.upstream, args.mode, args.output).run()
