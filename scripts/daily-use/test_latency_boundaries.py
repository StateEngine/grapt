"""Synthetic timing fixtures; these are not measurements of a product journey."""
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from latency_boundaries import dispatches, interval, report


class LatencyTests(unittest.TestCase):
    def test_exact_message_join_and_submission_window(self):
        with tempfile.TemporaryDirectory(prefix="daily-use-latency-") as root:
            root = Path(root)
            database = root / "fixture.sqlite3"
            connection = sqlite3.connect(database)
            connection.execute("create table event(aggregate_family, aggregate_id, seq, type, created, data)")
            events = [
                ("session.input.admitted.1", 10, {"inputID":"input", "input":{"data":{"text":"hello"}}}),
                ("session.input.promoted.1", 11, {"inputID":"input"}),
                ("session.assistant-message.opened.2", 12, {"attempt":"one","message":"first","selection":{},"step":1}),
                ("session.content.appended.1", 15, {"message":"first","item":{"kind":"tool"}}),
                ("session.assistant-message.opened.2", 20, {"attempt":"two","message":"second","selection":{},"step":2}),
                ("session.content.appended.1", 25, {"message":"second","item":{"kind":"text"}}),
            ]
            for seq, (kind, created, data) in enumerate(events):
                connection.execute("insert into event values ('session','session',?,?,?,?)", (seq,kind,created,json.dumps(data)))
            # A later unrelated Session must not replace the comparison's binding.
            connection.execute("insert into event values ('session','zzz-other',0,?,?,?)",
                               (events[0][0], 100, json.dumps(events[0][2])))
            connection.commit()
            connection.close()
            log = root / "stderr.log"
            log.write_text("GRAPT_PROVIDER_DISPATCH attempt=one at_ms=13\nGRAPT_PROVIDER_DISPATCH attempt=two at_ms=22\n", encoding="utf-8")
            comparison = root / "comparison.json"
            comparison.write_text(json.dumps({"sides":[{"client":"retained","session_binding_error":None,"sessions_created":["session"],"steps":[
                {"kind":"key","detail":"Enter","submission":True,"action_started_ms":7,"action_completed_ms":9},
                {"kind":"capture","action_started_ms":29,"action_completed_ms":30}
            ]}]}), encoding="utf-8")
            observed = report(database,log,comparison)["samples"]
            self.assertEqual(observed[0]["submission_to_admission_bounds_ms"], [1,3])
            self.assertEqual(observed[0]["dispatch_to_first_content_ms"], 2)
            self.assertIsNone(observed[0]["first_text_ms"])
            self.assertEqual(observed[1]["dispatch_to_first_text_ms"], 3)
            self.assertTrue(all(row["dispatch_ms"] is None for row in report(database)["samples"]))

    def test_zero_is_a_value_and_backward_clocks_are_refused(self):
        self.assertEqual(interval(0, 0), 0)
        self.assertIsNone(interval(None, 0))
        with self.assertRaises(ValueError): interval(1, 0)

    def test_duplicate_and_malformed_dispatches_refuse(self):
        with tempfile.TemporaryDirectory(prefix="daily-use-latency-") as root:
            path = Path(root)/"stderr.log"
            for text in ["GRAPT_PROVIDER_DISPATCH attempt=a at_ms=0\n"*2,
                         "GRAPT_PROVIDER_DISPATCH attempt=a at_ms=unknown\n"]:
                path.write_text(text,encoding="utf-8")
                with self.assertRaises(ValueError): dispatches(path)
            path.write_text("GRAPT_PROVIDER_DISPATCH_LOSS count_at_least=3\n",encoding="utf-8")
            self.assertEqual(dispatches(path), ({},3))


if __name__ == "__main__":
    unittest.main()
