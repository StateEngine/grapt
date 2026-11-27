"""Safety and evidence checks for the disposable acceptance driver itself."""
from pathlib import Path
import subprocess
import sys
import tempfile
import socket
import unittest
from unittest.mock import patch
from types import SimpleNamespace
import urllib.error
import urllib.request

from acceptance_peer import AGENT, CAPABILITY, PRIVATE_CURSOR, PROTOCOL, Mail, Model, Server
import headless_acceptance as driver


class AcceptanceDriver(unittest.TestCase):
    def test_cancelled_mail_poll_releases_its_request_slot_before_rebinding(self):
        mail = Mail()
        with Server(0, mail.dispatch, with_connection=True) as server:
            # More than the four-slot bound, with no timeout between rebinds.
            for index in range(8):
                with socket.create_connection(("127.0.0.1", server.server_port), timeout=2) as client:
                    body = b'{"operation":"next_change","args":{}}'
                    client.sendall((f"POST /{PROTOCOL} HTTP/1.0\r\nAuthorization: Bearer {CAPABILITY}\r\n"
                                    f"Content-Length: {len(body)}\r\n\r\n").encode() + body)
                    driver.wait(lambda: mail.count("next_change") == index + 1, "poll entry", seconds=2)
                # A new non-poll request must succeed while the old poll exits.
                request = urllib.request.Request(server.endpoint + "/" + PROTOCOL,
                    b'{"operation":"close","args":{}}', method="POST",
                    headers={"Authorization": "Bearer " + CAPABILITY})
                with urllib.request.urlopen(request, timeout=2) as response:
                    self.assertEqual(response.status, 200)
            self.assertEqual(server.errors, [])

    def test_release_product_is_refused_before_creating_scratch(self):
        if sys.platform == "win32":
            self.skipTest("the fake executable is a POSIX shell script")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            product = root / "product"
            product.write_text("#!/bin/sh\nprintf '0.1.0\\n'\n")
            product.chmod(0o700)
            scratch = root / "scratch"
            result = subprocess.run([sys.executable, str(Path(__file__).with_name("headless_acceptance.py")),
                "--cli", str(product), "--product", str(product), "--scratch", str(scratch)],
                capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("release binaries are refused", result.stderr)
            self.assertFalse(scratch.exists())

    def test_model_refuses_private_binding_before_recording_request(self):
        for private in (CAPABILITY, PRIVATE_CURSOR):
            model = Model()
            with self.assertRaisesRegex(ValueError, "private binding"):
                model.dispatch("/responses", {"Authorization": "Bearer disposable-model-dummy"},
                               {"input": [{"content": private}]})
            self.assertEqual(model.count(), 0)

    def test_mail_cannot_open_under_a_foreign_principal_or_project(self):
        for field, value in (("expected_recipient", "stranger"), ("expected_local_project", "foreign")):
            mail = Mail()
            request = {"requested_protocol": PROTOCOL, "expected_recipient": AGENT,
                       "expected_local_project": "secode"}
            request[field] = value
            with self.assertRaisesRegex(ValueError, "expected principal"):
                mail.dispatch("/" + PROTOCOL, {"Authorization": "Bearer " + CAPABILITY},
                              {"operation": "open", "args": {"request": request}})

    def test_foreign_delivery_ack_does_not_get_a_receipt(self):
        mail = Mail()
        with self.assertRaisesRegex(ValueError, "foreign mail acknowledgement"):
            mail.dispatch("/" + PROTOCOL, {"Authorization": "Bearer " + CAPABILITY}, {
                "operation": "acknowledge_delivery", "args": {
                    "ack": {"recipient": "stranger", "source_message": "msg_native_long"}}})
        self.assertEqual(mail.acks, [])

    def test_mail_port_cannot_be_silently_reused(self):
        with Server(0, Model().dispatch) as owned:
            with self.assertRaises(OSError):
                Server(owned.server_port, Model().dispatch)

    def test_oversized_http_request_is_refused_and_recorded(self):
        with Server(0, Model().dispatch) as owned:
            request = urllib.request.Request(owned.endpoint + "/responses", b"{}", method="POST",
                headers={"Content-Length": str(8 * 1024 * 1024 + 1)})
            with self.assertRaises(urllib.error.HTTPError) as error:
                urllib.request.urlopen(request, timeout=2)
            self.assertEqual(error.exception.code, 400)
            error.exception.close()
            self.assertEqual(owned.errors, ["request byte bound"])

    def test_duplicate_completing_during_joined_drain_cannot_pass(self):
        # Drive the complete assertion path, with an extra provider dispatch or
        # MCP completion becoming visible only when stop has joined the product.
        # The control proves this simulated journey otherwise satisfies the bar.
        for late in (None, "model", "stateengine_msg_read", "stateengine_msg_send"):
            with self.subTest(late=late), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                model = Model()
                packet = {"input": [{"content": "SEAT_PACKET_TASK FULL_BODY_END msg_native_reply"}]}

                class Peer:
                    errors = []
                    endpoint = "http://127.0.0.1:1"
                    def __init__(self, *_args, **_kwargs): pass
                    def __enter__(self): return self
                    def __exit__(self, *_args): pass

                class Mail:
                    operations = []
                    acks = ["msg_native_long", "quiet_fixture"]
                    polls, fetches, closes, signals = 1, 1, 0, 0
                    def dispatch(self, *_args): pass
                    def count(self, name):
                        return {"next_change": self.polls, "fetch_snapshot_page": self.fetches,
                                "close": self.closes}[name]
                    def signal(self):
                        self.signals += 1
                        self.polls += 1
                        self.fetches += 1
                        if self.signals == 1:
                            model.bodies.extend([packet] * 3)

                mail = Mail()

                class Journey:
                    mode = "mail"
                    def __init__(self, *_args):
                        self.root = root
                        (root / "mcp-calls.txt").write_text("stateengine_msg_read\nstateengine_msg_send\n")
                    def start(self, _index): pass
                    def provision(self): return SimpleNamespace(returncode=0)
                    def run(self, _name):
                        model.bodies.append(packet)
                        return SimpleNamespace(returncode=0)
                    def history(self): return {"data": "ROW_A_MAIL_DONE"}
                    def stop(self):
                        mail.closes += 1
                        if late == "model":
                            model.bodies.append(packet)
                        elif late:
                            with (root / "mcp-calls.txt").open("a") as calls:
                                calls.write(late + "\n")
                    def cleanup(self): pass

                with patch.object(driver, "Server", Peer), patch.object(driver, "Journey", Journey), \
                     patch.object(driver, "Model", return_value=model), patch.object(driver, "Mail", return_value=mail):
                    if late is None:
                        self.assertTrue(driver.mail_case(SimpleNamespace())["passed"])
                    else:
                        with self.assertRaisesRegex(RuntimeError, "joined drain revealed duplicate"):
                            driver.mail_case(SimpleNamespace())
                        self.assertFalse((root / "result.json").exists())


if __name__ == "__main__":
    unittest.main()
