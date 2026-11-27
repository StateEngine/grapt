"""A loopback TCP relay that can CUT every live connection on command.

The reconnect journey needs a connection that really breaks while the server
stays where it was. Restarting the product cannot do that: it admits only
`--port 0` (`IA0-1R admits only --port 0`), so it returns on a different port,
and a client handed a new address is exercising configuration rather than
recovery.

So the clients are pointed at this relay instead. It forwards to the product
byte for byte and, when told to cut, drops every socket it is holding. To each
client that is exactly what a dropped connection looks like — the server is
still listening at the same address, and reconnecting works.

Nothing is inspected or rewritten in flight: this moves bytes and closes
sockets. It is deliberately not a proxy that understands HTTP, because a relay
that parsed the traffic could change what the comparison measures.

Usage:
    python cut-proxy.py --listen 127.0.0.1:PORT --target 127.0.0.1:PORT
                        --control <file>

Touching the control file's `cut` sibling drops the live connections; the relay
keeps listening, so the next connect succeeds.
"""

from __future__ import annotations

import argparse
import socket
import sys
import threading
import time
from pathlib import Path


def address(raw: str) -> tuple[str, int]:
    host, _, port = raw.rpartition(":")
    return host, int(port)


class Relay:
    def __init__(self, listen: tuple[str, int], target: tuple[str, int]) -> None:
        self.listen = listen
        self.target = target
        self.live: list[socket.socket] = []
        self.lock = threading.Lock()
        self.cuts = 0

    def cut(self) -> int:
        """Drop every live socket. Returns how many were dropped."""
        with self.lock:
            held = list(self.live)
            self.live.clear()
        for sock in held:
            try:
                # RST rather than a graceful close, so the peer sees a broken
                # connection rather than an orderly end of stream. A tidy FIN
                # would let a client treat it as the server saying "done".
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, b"\x01\x00\x00\x00\x00\x00\x00\x00")
                sock.close()
            except OSError:
                pass
        self.cuts += 1
        return len(held)

    def hold(self, sock: socket.socket) -> None:
        with self.lock:
            self.live.append(sock)

    def release(self, sock: socket.socket) -> None:
        with self.lock:
            if sock in self.live:
                self.live.remove(sock)

    def pump(self, source: socket.socket, sink: socket.socket) -> None:
        try:
            while True:
                chunk = source.recv(65536)
                if not chunk:
                    break
                sink.sendall(chunk)
        except OSError:
            pass
        finally:
            for sock in (source, sink):
                try:
                    sock.close()
                except OSError:
                    pass

    def serve_one(self, client: socket.socket) -> None:
        try:
            upstream = socket.create_connection(self.target, timeout=30)
            # No read timeout once connected. A subscription is idle for long
            # stretches by design, and a relay that timed it out would cut the
            # connection on its own — manufacturing the very event this
            # instrument exists to trigger deliberately.
            upstream.settimeout(None)
            client.settimeout(None)
        except OSError:
            client.close()
            return
        self.hold(client)
        self.hold(upstream)
        for source, sink in ((client, upstream), (upstream, client)):
            threading.Thread(target=self.pump, args=(source, sink), daemon=True).start()

    def run(self, control: Path) -> None:
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(self.listen)
        listener.listen(64)
        listener.settimeout(0.25)
        control.write_text("ready", encoding="utf-8")
        cut_flag = control.with_suffix(".cut")
        stop_flag = control.with_suffix(".stop")
        while True:
            if stop_flag.exists():
                return
            if cut_flag.exists():
                dropped = self.cut()
                cut_flag.unlink(missing_ok=True)
                control.write_text(f"cut {self.cuts} dropped {dropped}", encoding="utf-8")
            try:
                client, _ = listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=self.serve_one, args=(client,), daemon=True).start()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--listen", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--control", required=True, type=Path)
    arguments = parser.parse_args()
    relay = Relay(address(arguments.listen), address(arguments.target))
    relay.run(arguments.control)
    return 0


if __name__ == "__main__":
    sys.exit(main())
