"""Bounded, synthetic v2 mailbox for the private-network seat walk.

The caller must put the whole walk in a new network namespace. This fixture
uses the product's ordinary port and never replaces a live StateEngine door.
"""
import asyncio
import json
from pathlib import Path
import threading
import time

CAPABILITY = "NATIVE_FIXTURE_CAPABILITY"
PROJECT = "native-fixture"
TARGET_PROJECT = "secode"
AGENT = "agt_native_fixture"


class Mailbox:
    def __init__(self, limits):
        self.limits = limits
        self.rows = []
        self.calls = []
        self.requests = []
        self.changed = asyncio.Event()
        self.active = 0
        self.snapshots = {}
        self.generation = 0
        self.acknowledged = set()
        self.fetched = set()
        self.error = None

    def publish(self, message):
        if message not in ("msg_seat_idle", "msg_seat_busy") or len(self.rows) >= 2:
            raise ValueError("fixture mail exceeds its declared two-message journey")
        if any(row["source_message"] == message for row in self.rows):
            raise ValueError("duplicate fixture publication")
        self.rows.append({
            "protocol": "remote-seat-mail-transport/v1", "source_binding": "bind-seat",
            "projects": {"source": PROJECT, "target": TARGET_PROJECT},
            "source_message": message, "source_cursor": "cursor-" + message,
            "sender": "agt_native_sender", "recipient": AGENT, "kind": "handoff",
            "body": "SEAT_MAIL_" + message.removeprefix("msg_seat_").upper(),
            "refs": [], "supersession": {"supersedes": None, "superseded_by": None},
            "archive": {"disposition": "active", "reason": None},
            "source_created": "2026-10-25T08:00:00.000Z", "attention": "wake",
        })
        self.changed.set()

    async def operation(self, name, args, reader):
        if len(self.calls) >= 256:
            raise ValueError("mail fixture operation bound")
        self.calls.append(name)
        self.requests.append({"operation": name, "at_ns": time.monotonic_ns(),
            "generation": self.generation,
            "checkpoint": args.get("resume", {}).get("from_checkpoint"),
            "after": args.get("after"),
            "snapshot": args.get("snapshot", {}).get("snapshot_id"),
            "message": args.get("ack", {}).get("source_message")})
        if name == "open":
            request = args["request"]
            assert request["expected_local_project"] == TARGET_PROJECT
            assert request["expected_recipient"] == AGENT
            assert request["requested_protocol"] == "remote-seat-mail-transport/v2"
            self.generation += 1
            return {
                "binding_id": "bind-seat", "protocol": "remote-seat-mail-transport/v2",
                "adapter_version": "stateengine-native-mail/2",
                "projects": {"source": PROJECT, "target": TARGET_PROJECT},
                "verified_principal": AGENT, "local_recipient": AGENT,
                "mapping_version": "exact-seat-id/v1",
                "takeover": {"generation": self.generation, "displaced": None},
                "limits": self.limits,
                "capabilities": {"atomic_subscribe_and_snapshot": True,
                    "durable_delivery_ack": True, "bulk_dismiss": True,
                    "explicit_project_pair": True, "opaque_authenticated_cursors": True,
                    "advisory_payload_bytes": 0, "coalesced_pending_signals": 1},
            }
        if name == "subscribe_and_snapshot":
            if len(self.snapshots) >= 64:
                raise ValueError("fixture snapshot bound")
            ident = "snap-seat-" + str(len(self.snapshots))
            checkpoint = args["resume"]["from_checkpoint"]
            start = 0 if checkpoint is None else next(
                i + 1 for i, row in enumerate(self.rows) if row["source_cursor"] == checkpoint)
            self.snapshots[ident] = list(self.rows[start:])
            self.changed.clear()
            return {"subscription_id": "sub-seat", "generation": self.generation, "snapshot": {
                "snapshot_id": ident, "barrier": "barrier-seat", "start_after": checkpoint,
                "retained_origin": "retained-origin/v1"}}
        if name == "fetch_snapshot_page":
            rows = self.snapshots[args["snapshot"]["snapshot_id"]]
            if args["after"] is not None:
                matches = [index for index, row in enumerate(self.rows)
                           if row["source_cursor"] == args["after"]]
                if not matches:
                    raise ValueError("unknown fixture page cursor")
                remaining = {row["source_message"] for row in self.rows[matches[0] + 1:]}
                rows = [row for row in rows if row["source_message"] in remaining]
            self.fetched.update(row["source_message"] for row in rows)
            return {"messages": rows, "refused_rows": [], "next_after": None,
                    "last_cursor": rows[-1]["source_cursor"] if rows else None}
        if name == "acknowledge_delivery":
            ack = args["ack"]
            if not any(row["source_message"] == ack["source_message"] for row in self.rows):
                raise ValueError("unknown fixture acknowledgement")
            self.acknowledged.add(ack["source_message"])
            return {"tag": "Recorded", "receipt": {key: ack[key] for key in (
                "ack_id", "source_message", "projects")}, "primary_delivery": {"witnessed": True},
                "floor_projection": {"state": "unarmed", "outcome": "refused",
                    "disposition": None, "reason": "secondary-unarmed",
                    "outcome_record": {"state": "recorded"}, "retry_required": False,
                    "retry": None}}
        if name == "next_change":
            signal = asyncio.create_task(self.changed.wait())
            closed = asyncio.create_task(reader.read(1))
            try:
                done, _ = await asyncio.wait((signal, closed), timeout=30,
                                            return_when=asyncio.FIRST_COMPLETED)
                if closed in done:
                    return None
                if signal not in done:
                    raise TimeoutError("fixture mailbox poll bound")
                return {"generation": self.generation, "token": None}
            finally:
                for task in (signal, closed):
                    task.cancel()
                await asyncio.gather(signal, closed, return_exceptions=True)
        if name == "close":
            return {"joined_tasks": 0, "ticks_used": 1}
        raise ValueError("unexpected mailbox fixture operation")

    async def handle(self, reader, writer):
        self.active += 1
        try:
            if self.active > 8:
                raise ValueError("mail fixture connection bound")
            header = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), 5)
            if len(header) > 8192:
                raise ValueError("fixture HTTP header bound")
            lines = header.decode("ascii").split("\r\n")
            assert lines[0] == "POST /remote-seat-mail-transport/v2 HTTP/1.1"
            headers = {name.lower(): value.strip() for line in lines[1:] if line
                       for name, value in [line.split(":", 1)]}
            assert headers["authorization"] == "Bearer " + CAPABILITY
            length = int(headers["content-length"])
            if not 0 < length <= 32768:
                raise ValueError("fixture HTTP body bound")
            body = json.loads(await asyncio.wait_for(reader.readexactly(length), 5))
            value = await self.operation(body["operation"], body["args"], reader)
            if value is not None:
                payload = json.dumps({"ok": True, "value": value}).encode()
                writer.write(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                             + f"Content-Length: {len(payload)}\r\nConnection: close\r\n\r\n".encode()
                             + payload)
                await writer.drain()
        except (ConnectionResetError, BrokenPipeError):
            pass  # Dropping an owned poll closes its connection.
        except Exception as error:
            self.error = self.error or type(error).__name__ + ": " + str(error)[:256]
        finally:
            self.active -= 1
            writer.close()
            try:
                await writer.wait_closed()
            except (ConnectionResetError, BrokenPipeError):
                pass


async def listen(limits_path):
    mailbox = Mailbox(json.loads(Path(limits_path).read_text())["limits"])
    server = await asyncio.start_server(mailbox.handle, "127.0.0.1", 4877, limit=8192)
    return mailbox, server


class MailPeer:
    """Keep the stub responsive while the real clients run synchronously."""
    def __init__(self, limits):
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self.loop.run_forever)
        self.thread.start()
        try:
            self.mailbox, self.server = asyncio.run_coroutine_threadsafe(
                listen(limits), self.loop).result(timeout=5)
        except BaseException:
            self.loop.call_soon_threadsafe(self.loop.stop)
            self.thread.join(timeout=5)
            self.loop.close()
            raise

    def publish(self, message):
        async def send():
            self.mailbox.publish(message)
        asyncio.run_coroutine_threadsafe(send(), self.loop).result(timeout=5)

    def close(self):
        async def stop():
            self.server.close()
            await self.server.wait_closed()
            tasks = [task for task in asyncio.all_tasks() if task is not asyncio.current_task()]
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
        try:
            asyncio.run_coroutine_threadsafe(stop(), self.loop).result(timeout=5)
        finally:
            self.loop.call_soon_threadsafe(self.loop.stop)
            self.thread.join(timeout=5)
            if self.thread.is_alive():
                raise RuntimeError("mail fixture thread did not join")
            self.loop.close()
