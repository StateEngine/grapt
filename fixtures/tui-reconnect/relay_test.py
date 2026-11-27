"""Receipt readers must never see a writer's empty or partial file."""
import json
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

from relay import Relay


class AtomicReceipts(unittest.TestCase):
    def assert_publication_is_atomic(self, name):
        with tempfile.TemporaryDirectory(prefix="grapt-relay-receipt-") as root:
            relay = Relay("127.0.0.1:1", "preset", root)
            # Stop after publishing both receipts; this test needs no upstream.
            relay.started = time.monotonic() - 46
            target = Path(root) / name
            opened = threading.Event()
            finish = threading.Event()
            errors = []
            original_open = Path.open

            def pause_empty_writer(path, mode="r", *args, **kwargs):
                stream = original_open(path, mode, *args, **kwargs)
                if "w" in mode and path.name in (name, "." + name + ".tmp"):
                    # Deliberately stop after creation/truncation, before the
                    # first JSON byte. Before the fix this is ready.json itself.
                    opened.set()
                    if not finish.wait(5):
                        stream.close()
                        raise RuntimeError("receipt writer barrier expired")
                return stream

            def publish():
                try:
                    relay.run()
                except Exception as error:
                    errors.append(error)

            with patch.object(Relay, "health", return_value={"pid": 123}), \
                    patch.object(Path, "open", pause_empty_writer):
                writer = threading.Thread(target=publish)
                writer.start()
                try:
                    self.assertTrue(opened.wait(5), "receipt writer did not reach the barrier")
                    # The ordinary reader can now probe exactly the race window.
                    visible = target.exists()
                    observed = target.read_bytes() if visible else None
                finally:
                    finish.set()
                    writer.join(5)
                self.assertFalse(writer.is_alive(), "receipt writer did not finish")
                self.assertEqual(errors, [], f"receipt writer failed: {errors}")
                self.assertFalse(visible, f"partially written {name} was published: {observed!r}")
                receipt = json.loads(target.read_text(encoding="utf-8"))
                self.assertTrue(receipt, "complete receipt was empty")
                self.assertFalse(target.with_name("." + name + ".tmp").exists())

    def test_ready_receipt_is_invisible_until_complete(self):
        self.assert_publication_is_atomic("ready.json")

    def test_health_receipt_is_invisible_until_complete(self):
        self.assert_publication_is_atomic("health.json")


if __name__ == "__main__":
    unittest.main()
