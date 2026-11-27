"""Refusal and continuity checks for the TUI journey's synthetic peers."""
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import socket
import tempfile
import time
import unittest

from acceptance_peer import CAPABILITY, PROTOCOL, Server
from tui_acceptance_peer import SUMMARY, TuiMail, TuiModel
from tui_acceptance import prepared_catalog_ready
import tui_fixture_server as mcp

AUTH = {"Authorization": "Bearer disposable-model-dummy"}
PACKET_INPUT = {"input": [{"role": "user", "content": "SEAT_PACKET_TASK"}]}


class TuiAcceptance(unittest.TestCase):
    def test_draft_ready_footer_does_not_imply_restored_catalog_readiness(self):
        header = "agent agt_live_headless1 model openai/gpt-live\nenter send"
        self.assertFalse(prepared_catalog_ready(header + "\nCatalog: loading… Drafts stay editable."))
        self.assertFalse(prepared_catalog_ready(header + "\nCatalog refresh failed: fixture"))
        self.assertFalse(prepared_catalog_ready("agent - model -\nenter send"))
        self.assertTrue(prepared_catalog_ready(header))

    def test_cancelled_long_poll_releases_fixture_request_custody(self):
        mail = TuiMail()
        began = time.monotonic()
        with Server(0, mail.dispatch, with_connection=True) as server:
            with socket.create_connection(("127.0.0.1", server.server_port), timeout=2) as client:
                body = json.dumps({"operation": "next_change", "args": {}}).encode()
                client.sendall((f"POST /{PROTOCOL} HTTP/1.0\r\nAuthorization: Bearer {CAPABILITY}\r\n"
                                f"Content-Length: {len(body)}\r\n\r\n").encode() + body)
                deadline = time.monotonic() + 1
                while mail.count("next_change") == 0 and time.monotonic() < deadline:
                    time.sleep(0.01)
                self.assertEqual(mail.count("next_change"), 1)
            # server_close joins the retained request; EOF must release it
            # rather than waiting out the five-second advisory deadline.
        self.assertLess(time.monotonic() - began, 2)
        self.assertEqual(server.errors, [])

    def fetch(self, mail, after=None):
        _type, raw = mail.dispatch("/" + PROTOCOL, {"Authorization": "Bearer " + CAPABILITY},
            {"operation": "fetch_snapshot_page", "args": {"after": after}})
        return json.loads(raw)["value"]

    def test_restart_cursor_preserves_new_mail_without_replaying_old_rows(self):
        mail = TuiMail()
        mail.publish("msg_tui_idle")
        first = self.fetch(mail)
        self.assertEqual([row["source_message"] for row in first["messages"]], ["quiet_fixture", "msg_tui_idle"])
        checkpoint = first["last_cursor"]
        self.assertEqual(self.fetch(mail, checkpoint)["messages"], [])
        mail.publish("msg_tui_busy")
        mail.publish("msg_tui_restart")
        resumed = self.fetch(mail, checkpoint)
        self.assertEqual([row["source_message"] for row in resumed["messages"]], ["msg_tui_busy", "msg_tui_restart"])
        self.assertEqual(self.fetch(mail, resumed["last_cursor"])["messages"], [])

    def test_foreign_cursor_is_refused_instead_of_becoming_empty_mail(self):
        with self.assertRaisesRegex(ValueError, "foreign mail cursor"):
            self.fetch(TuiMail(), "cursor-foreign")

    def test_duplicate_publication_cannot_fake_two_distinct_deliveries(self):
        mail = TuiMail()
        mail.publish("msg_tui_idle")
        with self.assertRaisesRegex(ValueError, "duplicate synthetic publication"):
            mail.publish("msg_tui_idle")

    def test_busy_request_remains_held_until_explicit_release(self):
        model = TuiModel()
        model.expect(("busy", ""))
        with ThreadPoolExecutor(max_workers=1) as pool:
            response = pool.submit(model.dispatch, "/responses", AUTH, PACKET_INPUT)
            try:
                self.assertTrue(model.busy_entered.wait(timeout=2))
                self.assertFalse(response.done())
                self.assertEqual(model.count(), 1)
            finally:
                model.busy_release.set()
            self.assertIn(b"ROW_A_BUSY_DONE", response.result(timeout=2)[1])

    def test_extra_model_request_cannot_receive_an_unplanned_success(self):
        model = TuiModel()
        with self.assertRaisesRegex(ValueError, "unexpected extra model execution"):
            model.dispatch("/responses", AUTH, PACKET_INPUT)
        self.assertEqual(model.count(), 1)  # The refused attempt is retained.

    def test_compacted_coding_request_requires_the_actual_summary(self):
        model = TuiModel()
        model.expect(("after-compact", ""))
        with self.assertRaisesRegex(ValueError, "compaction summary did not reach"):
            model.dispatch("/responses", AUTH, PACKET_INPUT)
        model = TuiModel()
        model.expect(("after-compact", ""))
        content = "SEAT_PACKET_TASK\n[Model-written continuation summary]\n" + SUMMARY
        response = model.dispatch("/responses", AUTH, {"input": [{"role": "user", "content": content}]})
        self.assertIn(b"ROW_A_COMPACT_DONE", response[1])

    def test_mcp_records_message_identity_and_busy_boundary_without_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            marker, events = root / "busy", root / "events"
            mcp.install(events, marker)
            with self.assertRaisesRegex(ValueError, "foreign TUI fixture message"):
                mcp.fixture.result("stateengine_msg_read", {"msg_id": "foreign"}, "ready")
            self.assertFalse(events.exists())
            marker.touch()
            read = mcp.fixture.result("stateengine_msg_read", {"msg_id": "msg_tui_busy"}, "ready")
            self.assertIn("FULL_BODY_END_msg_tui_busy", read["message"]["body"])
            marker.unlink()
            mcp.fixture.result("stateengine_msg_send", {"to": "agt_native_sender",
                "kind": "handoff", "body": "Read msg_tui_busy"}, "ready")
            rows = [json.loads(line) for line in events.read_text().splitlines()]
            self.assertEqual([row["while_busy"] for row in rows], [True, False])
            self.assertEqual([row["message"] for row in rows], ["msg_tui_busy"] * 2)
            self.assertNotIn(CAPABILITY, events.read_text())


if __name__ == "__main__":
    unittest.main()
