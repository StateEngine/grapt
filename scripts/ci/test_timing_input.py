"""The checked-in timing input: report parsing, loader, and generator."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import generate_test_times as generator
import hosted_coverage as allocation
import junit_report


def junit(cases):
    """One junit document; cases are (binary, name, time, skipped) rows."""
    body = []
    for binary, name, elapsed, skipped in cases:
        time_attr = "" if elapsed is None else f' time="{elapsed}"'
        marker = "<skipped/>" if skipped else ""
        body.append(f'<testcase classname="{binary}" name="{name}"{time_attr}>{marker}</testcase>')
    return f"<testsuites><testsuite>{''.join(body)}</testsuite></testsuites>"


class JunitReportContracts(unittest.TestCase):
    def test_parse_excludes_skipped_and_counts(self):
        document = junit([
            ("a", "executed", "2.5", False),
            ("a", "skipped", "0", True),
            ("a", "missing_time", None, False),
            ("a", "negative", "-1", False),
            ("", "no_binary", "1", False),
        ])
        table, counters = junit_report.parse_junit(document)
        self.assertEqual(table, {("a", "executed"): 2.5})
        self.assertEqual(counters, {"observations": 5, "skipped": 1,
                                    "absent_time": 1, "malformed": 2, "duplicates": 0})

    def test_parse_later_in_file_observation_wins(self):
        document = junit([("a", "retried", "4", False), ("a", "retried", "1", False)])
        table, counters = junit_report.parse_junit(document)
        self.assertEqual(table, {("a", "retried"): 1.0})
        self.assertEqual(counters["duplicates"], 1)

    def test_parse_rejects_an_unparseable_document(self):
        with self.assertRaises(ValueError):
            junit_report.parse_junit("<testsuites>not closed")


class TimingInputLoaderContracts(unittest.TestCase):
    def _write(self, root, payload):
        path = root / "test-times.json"
        if isinstance(payload, (dict, list)):
            path.write_text(json.dumps(payload), encoding="utf-8")
        else:
            path.write_text(payload, encoding="utf-8")
        return path

    def test_missing_file_fails_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            with self.assertRaises(ValueError):
                allocation.timing_input(Path(raw) / "absent.json")

    def test_malformed_json_fails_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            path = self._write(root, "{not json")
            with self.assertRaises(ValueError):
                allocation.timing_input(path)

    def test_schema_mismatch_fails_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for document in (
                    {"schema": 2, "source": "s",
                     "binaries": {"a": {"total": 1}}},
                    {"schema": 1, "source": "s", "extra": True,
                     "binaries": {"a": {"total": 1}}},
                    {"schema": 1, "source": "s", "binaries": {}},
                    {"schema": 1, "source": "s", "binaries": {"": {"total": 1}}},
                    {"schema": 1, "source": "s", "binaries": {"a": {}}},
                    {"schema": 1, "source": "s", "binaries": {"a": {"total": 1, "tests": {}}}},
            ):
                with self.subTest(document=sorted(document)):
                    path = self._write(root, document)
                    with self.assertRaises(ValueError):
                        allocation.timing_input(path)

    def test_negative_or_nan_duration_fails_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for entry in ({"total": -1}, {"total": 1, "tests": {"t": float("nan")}},
                          {"total": 1, "tests": {"t": "fast"}}):
                with self.subTest(entry=entry):
                    path = self._write(root, {"schema": 1, "source": "s",
                                              "binaries": {"a": entry}})
                    with self.assertRaises(ValueError):
                        allocation.timing_input(path)

    def test_unrepresentable_numeric_durations_select_the_malformed_input_path(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for entry in ({"total": 10 ** 400},
                          {"total": 1, "tests": {"sample": 10 ** 400}}):
                with self.subTest(location="tests" if "tests" in entry else "total"):
                    path = self._write(root, {"schema": 1, "source": "synthetic",
                                              "binaries": {"sample": entry}})
                    with self.assertRaises(ValueError):
                        allocation.timing_input(path)

    def test_oversized_file_fails_closed(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            path = self._write(root, {"schema": 1, "source": "s",
                                      "binaries": {f"b{index:04d}": {"total": 1.0}
                                                   for index in range(60)}})
            with patch.object(allocation, "MAX_BYTES", 512):
                with self.assertRaises(ValueError):
                    allocation.timing_input(path)

    def test_loads_mapped_and_aggregate_binaries(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            path = self._write(root, {"schema": 1, "source": "fixture-run",
                                      "binaries": {"mapped": {"total": 3.0,
                                                              "tests": {"x": 2.0, "y": 1.0}},
                                                   "aggregate": {"total": 7.0}}})
            weights, source = allocation.timing_input(path)
        self.assertEqual(source, "fixture-run")
        self.assertEqual(weights["totals"], {"mapped": 3.0, "aggregate": 7.0})
        self.assertEqual(weights["tests"], {"mapped": {"x": 2.0, "y": 1.0}})


class GeneratorContracts(unittest.TestCase):
    def _build(self, files, source="hosted-run-1"):
        return generator.build(files, source)

    def _write(self, root, name, document):
        path = root / name
        path.write_text(document, encoding="utf-8")
        return path

    def test_output_is_deterministic_and_byte_stable(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            first = self._write(root, "a.xml", junit([("a", "one", "1", False),
                                                      ("b", "two", "2", False)]))
            second = self._write(root, "b.xml", junit([("a", "three", "4", False)]))
            document_one, _ = self._build([first, second])
            document_two, _ = self._build([second, first])
            text_one = json.dumps(document_one, sort_keys=True, indent=2) + "\n"
            text_two = json.dumps(document_two, sort_keys=True, indent=2) + "\n"
        self.assertEqual(text_one, text_two, "byte stability must not depend on input order")

    def test_skipped_observations_never_replace_executed_durations(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            # Sorted file order: the skipped zero is seen first.
            first = self._write(root, "a.xml", junit([("a", "x", "0", True)]))
            second = self._write(root, "b.xml", junit([("a", "x", "1", False)]))
            document, counters = self._build([first, second])
        self.assertEqual(document["binaries"]["a"]["total"], 1.0)
        self.assertEqual(counters["skipped"], 1)

    def test_executed_duplicates_keep_first_file_order(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            first = self._write(root, "a.xml", junit([("a", "x", "1", False)]))
            second = self._write(root, "b.xml", junit([("a", "x", "5", False)]))
            document, counters = self._build([first, second])
        self.assertEqual(document["binaries"]["a"]["total"], 1.0)
        self.assertEqual(counters["cross_file_duplicates"], 1)

    def test_within_file_retry_rule_stays_separate(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            # In a.xml the later observation (2) wins within the file; across
            # files a.xml still precedes b.xml, so 2 beats 5.
            first = self._write(root, "a.xml",
                                junit([("a", "x", "1", False), ("a", "x", "2", False)]))
            second = self._write(root, "b.xml", junit([("a", "x", "5", False)]))
            document, counters = self._build([first, second])
        self.assertEqual(document["binaries"]["a"]["total"], 2.0)
        self.assertEqual(counters["duplicates"], 1)
        self.assertEqual(counters["cross_file_duplicates"], 1)

    def test_split_threshold_marks_only_large_binaries(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            document_path = self._write(root, "a.xml", junit([
                ("big", "one", "90", False),
                ("small", "two", "10", False),
            ]))
            document, _ = self._build([document_path])
        self.assertIn("tests", document["binaries"]["big"])
        self.assertNotIn("tests", document["binaries"]["small"])

    def test_no_executed_observations_fails(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            document_path = self._write(root, "a.xml",
                                        junit([("a", "x", "0", True)]))
            with self.assertRaises(ValueError):
                self._build([document_path])


if __name__ == "__main__":
    unittest.main()
