"""Focused refusal and attribution checks for correction evidence tools."""
import contextlib
import io
import json
import sqlite3
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from replay_mutations import prepare
from usage_latency import latency


class MutationAnchors(unittest.TestCase):
    def test_missing_duplicate_and_escaping_anchors_refuse_without_writing(self):
        with TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "source.rs"
            row = {"id": "fixture", "source": "source.rs", "edits": [{"anchor": "once", "replacement": "mutated"}]}
            for body in [b"absent", b"once once"]:
                path.write_bytes(body)
                with self.assertRaisesRegex(ValueError, "exactly one anchor"):
                    prepare(root, row)
                self.assertEqual(path.read_bytes(), body)
            with self.assertRaisesRegex(ValueError, "escapes checkout"):
                prepare(root, {**row, "source": "../outside.rs"})

    def test_valid_anchor_preserves_original_bytes_for_restoration(self):
        with TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path = root / "source.rs"
            original = "// café\r\nonce\r\n".encode("utf-8")
            path.write_bytes(original)
            _, saved, mutant = prepare(root, {"id": "fixture", "source": "source.rs", "edits": [{"anchor": "once", "replacement": "mutated"}]})
            self.assertEqual(saved, original)
            self.assertIn(b"mutated", mutant)
            self.assertEqual(path.read_bytes(), original)


class LatencyAttribution(unittest.TestCase):
    def test_multiple_executions_cannot_be_combined_into_one_latency_row(self):
        connection = sqlite3.connect(":memory:")
        try:
            connection.execute("CREATE TABLE event (aggregate_id TEXT, seq INTEGER, type TEXT, created INTEGER, data TEXT)")
            for seq, kind, created in [(1, "session.execution.started.1", 10), (2, "session.execution.started.1", 1000), (3, "session.content.appended.1", 1010), (4, "session.execution.succeeded.1", 1020)]:
                connection.execute("INSERT INTO event VALUES (?, ?, ?, ?, ?)", ("fixture-session", seq, kind, created, json.dumps({})))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                latency(connection)
            self.assertIn("unavailable (2 executions; this table requires one)", output.getvalue())
        finally:
            connection.close()


if __name__ == "__main__":
    unittest.main()
