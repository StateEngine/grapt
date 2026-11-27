"""Keep the synthetic seat protocol aligned with its asserted journey."""
import asyncio
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import fixture_server
from seat_mail_fixture import Mailbox
import seat_provider_fixture
import seat_assertions


class RequiredReadTests(unittest.TestCase):
    def test_packet_acknowledgement_precedes_the_required_inbox_read(self):
        reply = fixture_server.result("stateengine_resume", {"packet_id": "native_seat_packet"},
                                      "seat-required-reads")
        self.assertEqual(reply["packet_acknowledgement"]["packet_id"], "native_seat_packet")
        self.assertEqual(reply["resume_status"], "needs_read")
        with self.assertRaises(ValueError):
            fixture_server.result("stateengine_resume", {"since": reply["cursor"]},
                                  "seat-required-reads")
        read = reply["required_reads"][0]
        inbox = fixture_server.result(read["tool"], read["arguments"], "seat-required-reads")
        self.assertEqual(inbox["messages"][0]["body"], "SEAT_REQUIRED_MAIL")
        ready = fixture_server.result("stateengine_resume", {"since": reply["cursor"]},
                                      "seat-required-reads")
        self.assertEqual(ready["resume_status"], "ready")
        self.assertEqual(ready["required_reads"], [])

    def test_synthetic_message_reads_are_exact(self):
        for message in ("msg_seat_idle", "msg_seat_busy"):
            result = fixture_server.result("stateengine_msg_read", {"msg_id": message},
                                           "seat-required-reads")
            self.assertEqual(result["message"]["msg_id"], message)
        with self.assertRaises(ValueError):
            fixture_server.result("stateengine_msg_read", {"msg_id": "foreign"}, "seat-required-reads")


class ModelTests(unittest.TestCase):
    def request(self, message, returned=None, body=None, call_id="read-1"):
        rows = [{"role": "user", "content": message}]
        if returned is not None:
            rows.extend([
                {"type": "function_call", "call_id": "read-1",
                 "name": "stateengine_stateengine_msg_read",
                 "arguments": json.dumps({"msg_id": message})},
                {"type": "function_call_output", "call_id": call_id,
                 "output": json.dumps({"message": {"msg_id": returned, "body": body},
                                       "read_acknowledgement": "recorded"})},
                {"role": "user", "content": "verified external continuity"}])
        return json.dumps({"input": rows}).encode()

    def observe(self, request):
        with tempfile.TemporaryDirectory() as root, patch.object(
                seat_provider_fixture, "observations", Path(root) / "requests.jsonl"):
            return seat_provider_fixture.observe(request)

    def test_exact_message_body_and_call_linkage_are_required(self):
        for returned, body, call_id in (
                ("msg_seat_idle", "SEAT_MAIL_IDLE", "read-1"),
                ("msg_seat_busy", "SEAT_MAIL_IDLE", "read-1"),
                ("msg_seat_busy", "SEAT_MAIL_BUSY", "foreign-call")):
            with self.subTest(returned=returned, body=body, call_id=call_id):
                with self.assertRaises(ValueError):
                    self.observe(self.request("msg_seat_busy", returned, body, call_id))

    def test_exact_mail_result_survives_following_automatic_context(self):
        for message, body in (("msg_seat_idle", "SEAT_MAIL_IDLE"),
                              ("msg_seat_busy", "SEAT_MAIL_BUSY")):
            label = self.observe(self.request(message, message, body))
            self.assertIn("SEAT_DONE_" + message,
                          seat_provider_fixture.frames(0, label)[0][0])

    def test_launch_tool_result_survives_following_automatic_context(self):
        request = {"input": [{"role": "user", "content": "SEAT_LAUNCH"},
            {"type": "function_call_output", "output": "synthetic result"},
            {"role": "user", "content": "verified external continuity"}]}
        label = self.observe(json.dumps(request).encode())
        self.assertIn("SEAT_DONE_SEAT_LAUNCH", seat_provider_fixture.frames(0, label)[0][0])


class JoinedTraceTests(unittest.TestCase):
    def trace(self, root):
        labels = ["SEAT_LAUNCH", "SEAT_LAUNCH", "msg_seat_idle", "msg_seat_idle",
                  "SEAT_BUSY", "msg_seat_busy", "msg_seat_busy", "summary", "SEAT_COMPACTED"]
        rows = [{"label": label, "tool_result": index in (1, 3, 6), "mail_read": None,
                 "at_ns": stamp, "private_absent": True, "packet": label != "summary",
                 "required_read": label not in ("summary", "SEAT_COMPACTED")}
                for index, (label, stamp) in enumerate(zip(labels, (1, 2, 3, 5, 6, 10, 12, 13, 15)))]
        names = ["stateengine_resume", "stateengine_msg_list", "stateengine_resume",
                 "stateengine_msg_read", "stateengine_msg_read", "stateengine_resume"]
        events = [{"name": name, "at_ns": stamp, "arguments": {}, "result": {}}
                  for name, stamp in zip(names, (0, 0, 0, 4, 11, 14))]
        for index, event, message in ((3, 3, "msg_seat_idle"), (6, 4, "msg_seat_busy")):
            body = {"msg_id": message, "body": "SEAT_MAIL_" + message.removeprefix("msg_seat_").upper()}
            rows[index]["mail_read"] = body
            events[event]["arguments"] = {"msg_id": message}
            events[event]["result"] = {"message": body, "read_acknowledgement": "recorded"}
        self.write_rows(root / "provider-observations.jsonl", rows)
        self.write_rows(root / "provider-attempts.jsonl", [{"at_ns": row["at_ns"]} for row in rows])
        self.write_rows(root / "mcp-calls.txt.jsonl", events)
        (root / "mcp-calls.txt").write_text("\n".join(names) + "\n")
        (root / "busy-held").write_text("6")
        (root / "busy-released").write_text("9")
        mailbox = SimpleNamespace(error=None, fetched={"msg_seat_idle", "msg_seat_busy"},
                                  acknowledged={"msg_seat_idle", "msg_seat_busy"})
        return rows, events, mailbox

    def write_rows(self, path, rows):
        path.write_text("".join(json.dumps(row) + "\n" for row in rows))

    def test_joined_trace_accepts_the_complete_declared_sequence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, _, mailbox = self.trace(root)
            seat_assertions.validate(root, 7, mailbox)

    def test_a_read_added_during_join_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, events, mailbox = self.trace(root)
            def close():
                with (root / "mcp-calls.txt").open("a") as output:
                    output.write("stateengine_msg_read\n")
                self.write_rows(root / "mcp-calls.txt.jsonl", events + [events[4]])
            journey = SimpleNamespace(evidence=root, held_after=7, clients=[], tui=None,
                witness=lambda: {"client": "headless"}, close=close,
                peer=SimpleNamespace(mailbox=mailbox))
            with self.assertRaisesRegex(AssertionError, "final MCP sequence"):
                seat_assertions.run_joined(journey)

    def test_a_provider_attempt_anywhere_in_the_held_interval_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows, _, mailbox = self.trace(root)
            rows[4]["at_ns"] = 8
            self.write_rows(root / "provider-observations.jsonl", rows)
            self.write_rows(root / "provider-attempts.jsonl", [{"at_ns": row["at_ns"]} for row in rows])
            with self.assertRaisesRegex(AssertionError, "provider entered while busy"):
                seat_assertions.validate(root, 7, mailbox)

    def test_failed_or_unsettled_provider_attempt_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rows, _, mailbox = self.trace(root)
            self.write_rows(root / "provider-attempts.jsonl",
                            [{"at_ns": row["at_ns"]} for row in rows] + [{"at_ns": 16}])
            with self.assertRaisesRegex(AssertionError, "unsettled provider attempt"):
                seat_assertions.validate(root, 7, mailbox)


class MailTests(unittest.IsolatedAsyncioTestCase):
    async def test_reopened_subscriptions_use_the_takeover_generation(self):
        mailbox = Mailbox({})
        request = {"request": {"expected_local_project": "secode",
            "expected_recipient": "agt_native_fixture",
            "requested_protocol": "remote-seat-mail-transport/v2"}}
        for _ in range(2):
            opened = await mailbox.operation("open", request, asyncio.StreamReader())
            snapshot = await mailbox.operation("subscribe_and_snapshot", {"resume": {
                "from_checkpoint": None}}, asyncio.StreamReader())
            self.assertEqual(snapshot["generation"], opened["takeover"]["generation"])

    async def test_real_http_open_uses_the_bound_synthetic_principal(self):
        mailbox = Mailbox({})
        server = await asyncio.start_server(mailbox.handle, "127.0.0.1", 0, limit=8192)
        try:
            reader, writer = await asyncio.open_connection(*server.sockets[0].getsockname())
            body = json.dumps({"operation": "open", "args": {"request": {
                "expected_local_project": "secode", "expected_recipient": "agt_native_fixture",
                "requested_protocol": "remote-seat-mail-transport/v2"}}}).encode()
            writer.write(b"POST /remote-seat-mail-transport/v2 HTTP/1.1\r\nHost: localhost\r\n"
                         b"Authorization: Bearer NATIVE_FIXTURE_CAPABILITY\r\n"
                         + f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
            await writer.drain()
            response = await asyncio.wait_for(reader.read(8192), 5)
            writer.close()
            await writer.wait_closed()
            value = json.loads(response.split(b"\r\n\r\n", 1)[1])["value"]
            self.assertEqual(value["verified_principal"], "agt_native_fixture")
            self.assertEqual(value["protocol"], "remote-seat-mail-transport/v2")
            self.assertEqual(value["projects"], {"source": "native-fixture", "target": "secode"})
            self.assertIsNone(mailbox.error)
        finally:
            server.close()
            await server.wait_closed()

    async def test_snapshot_barrier_and_checkpoint_do_not_repeat_old_mail(self):
        mailbox = Mailbox({})
        reader = asyncio.StreamReader()
        mailbox.publish("msg_seat_idle")
        first = await mailbox.operation("subscribe_and_snapshot", {"resume": {"from_checkpoint": None}}, reader)
        mailbox.publish("msg_seat_busy")
        page = await mailbox.operation("fetch_snapshot_page", {"snapshot": first["snapshot"], "after": None}, reader)
        self.assertEqual([row["source_message"] for row in page["messages"]], ["msg_seat_idle"])
        self.assertEqual(page["messages"][0]["projects"]["target"], "secode")
        self.assertEqual(mailbox.fetched, {"msg_seat_idle"})
        second = await mailbox.operation("subscribe_and_snapshot", {"resume": {
            "from_checkpoint": page["last_cursor"]}}, reader)
        page = await mailbox.operation("fetch_snapshot_page", {"snapshot": second["snapshot"],
            "after": second["snapshot"]["start_after"]}, reader)
        self.assertEqual([row["source_message"] for row in page["messages"]], ["msg_seat_busy"])
        self.assertEqual(page["refused_rows"], [])
        self.assertEqual(mailbox.fetched, {"msg_seat_idle", "msg_seat_busy"})
        with self.assertRaises(ValueError):
            mailbox.publish("msg_seat_busy")

    async def test_an_owned_poll_can_close_without_a_signal(self):
        mailbox = Mailbox({})
        reader = asyncio.StreamReader()
        reader.feed_eof()
        self.assertIsNone(await mailbox.operation("next_change", {}, reader))


if __name__ == "__main__":
    unittest.main()
