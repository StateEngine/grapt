"""The opt-in relay readiness signal witnesses a successful bind."""

import contextlib
import io
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import patch

import request_gate


class RelayReadinessTests(unittest.TestCase):
    def serve(self, root, *, port=0, announce=True, output=None):
        arguments = [
            "request_gate.py", "serve", "--plain-http", "--port", str(port),
            "--upstream-host", "127.0.0.1", "--log", str(root / "relay.jsonl"),
        ]
        if announce:
            arguments.append("--announce-ready")
        if output is None:
            output = io.StringIO()

        def observe_bound(gate):
            expected = "RELAY_READY\n" if announce else ""
            self.assertEqual(output.getvalue(), expected)
            with socket.create_connection(gate.server_address, timeout=2):
                pass

        with patch.object(sys, "argv", arguments), contextlib.redirect_stdout(output):
            with patch.object(request_gate.Gate, "serve_forever", observe_bound):
                request_gate.main()
        return output.getvalue()

    def test_readiness_follows_the_actual_bind(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(self.serve(Path(directory)), "RELAY_READY\n")

    def test_an_occupied_port_cannot_announce_readiness(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket() as occupied:
            occupied.bind(("127.0.0.1", 0))
            occupied.listen()
            output = io.StringIO()
            with self.assertRaises(OSError):
                self.serve(Path(directory), port=occupied.getsockname()[1], output=output)
            self.assertEqual(output.getvalue(), "")

    def test_readiness_output_is_opt_in(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(self.serve(Path(directory), announce=False), "")


if __name__ == "__main__":
    unittest.main()
