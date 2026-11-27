"""Shard planning must preserve coverage and bounded timing inputs."""
import json
import os
from contextlib import nullcontext
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import hosted_coverage as allocation
import junit_report
import run as runner


class HostedShardContracts(unittest.TestCase):
    def test_overlapping_published_shards_cannot_repeat_hosted_tests(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            results = root / "ci-results"
            results.mkdir()
            (results / "archive-paths.json").write_text(json.dumps({
                "workspace": str(root), "target": str(root / "target")}))
            expected = [["fixture", name] for name in ("first", "second")]
            (results / "test-allocation.json").write_text(json.dumps([
                {"binary": binary, "package": "fixture", "name": name,
                 "route": "hosted"} for binary, name in expected]))
            (results / "shard-plan.json").write_text(json.dumps({
                "schema": 1, "mode": "time", "total": 2,
                "shards": {str(shard): {"expression": "all()", "expected": expected}
                           for shard in (1, 2)}}))
            report = root / "target/nextest/gate/junit.xml"
            report.parent.mkdir(parents=True)
            report.write_text('<testsuites><testsuite>' + ''.join(
                f'<testcase classname="fixture" name="{name}" time="1"/>'
                for name in ("first", "second")) + '</testsuite></testsuites>')
            with patch.object(runner, "ROOT", root), \
                    patch.dict(os.environ, CI_PACKAGES='["fixture"]', CI_NIGHTLY="false"), \
                    patch.object(runner.gate_scratch, "scratch", side_effect=nullcontext), \
                    patch.object(runner.gate_scratch, "judge_headless_residue", return_value=0), \
                    patch.object(runner, "run") as command:
                refused = []
                for shard in (1, 2):
                    with patch.object(sys, "argv", ["run.py", "test", "--shard", str(shard)]):
                        try:
                            runner.main()
                        except ValueError:
                            refused.append(shard)
                self.assertEqual(refused, [1, 2],
                                 "overlapping shards must refuse before execution")
                command.assert_not_called()

    def test_timing_input_exceeding_its_bound_keeps_the_fallback(self):
        names = ("first", "second")
        document = {"rust-suites": {"fixture": {
            "package-name": "fixture",
            "testcases": {name: {"ignored": False} for name in names},
        }}}
        metadata = {"packages": [{"name": "fixture", "dependencies": []}]}

        def command(arguments, **_options):
            if "metadata" in arguments:
                return json.dumps(metadata)
            if "list" in arguments:
                return json.dumps(document)
            return ""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            timing = root / "scripts" / "ci"
            timing.mkdir(parents=True)
            (timing / "test-times.json").write_text(json.dumps({
                "schema": 1, "source": "fixture-run",
                "binaries": {f"b{index:04d}": {"total": 1.0}
                             for index in range(60)}}),
                                                    encoding="utf-8")
            with patch.object(runner, "ROOT", root), \
                    patch.dict(os.environ, CI_PACKAGES='["fixture"]', CI_NIGHTLY="false"), \
                    patch.object(sys, "argv", ["run.py", "archive", "--platform", "linux"]), \
                    patch.object(runner, "run", side_effect=command), \
                    patch.object(allocation, "MAX_BYTES", 512):
                runner.main()
            plan = json.loads((root / "ci-results/shard-plan.json").read_text())
            self.assertEqual(plan["mode"], "fallback",
                             "an oversized timing input must select the shared fallback")
            self.assertEqual(plan["provenance"]["reason"], "timing-input-unavailable")
            self.assertEqual(plan["expression"], "all()")

    def test_windows_archive_publishes_fallback_for_an_oversize_selector(self):
        names = [f"test_{index:04}_" + "long_name_" * 10 for index in range(800)]
        document = {"rust-suites": {"fixture": {
            "package-name": "fixture",
            "testcases": {name: {"ignored": False} for name in names},
        }}}
        metadata = {"packages": [{"name": "fixture", "dependencies": []}]}

        def command(arguments, **_options):
            if "metadata" in arguments:
                return json.dumps(metadata)
            if "list" in arguments:
                return json.dumps(document)
            return ""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            timing = root / "scripts" / "ci"
            timing.mkdir(parents=True)
            (timing / "test-times.json").write_text(json.dumps({
                "schema": 1, "source": "fixture-run",
                "binaries": {"fixture": {"total": 800.0,
                                         "tests": {name: 1.0 for name in names}}}}),
                                                    encoding="utf-8")
            with patch.object(runner, "ROOT", root), \
                    patch.dict(os.environ, CI_PACKAGES='["fixture"]', CI_NIGHTLY="false"), \
                    patch.object(sys, "argv", ["run.py", "archive", "--platform", "windows"]), \
                    patch.object(runner, "run", side_effect=command), \
                    patch.object(runner, "validate_shard_selectors") as validate:
                runner.main()
            plan = json.loads((root / "ci-results/shard-plan.json").read_text())
            self.assertEqual(plan["mode"], "fallback",
                             "an oversized Windows command must select the shared fallback")
            self.assertEqual(plan["expression"], "all()")
            validate.assert_not_called()

    def test_zero_shard_plan_cannot_skip_allocated_hosted_tests(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            results = root / "ci-results"
            results.mkdir()
            (results / "archive-paths.json").write_text(json.dumps({
                "workspace": str(root), "target": str(root / "target")}))
            (results / "test-allocation.json").write_text(json.dumps([
                {"binary": "fixture", "package": "fixture", "name": name,
                 "route": "hosted"} for name in ("first", "second")]))
            (results / "shard-plan.json").write_text(json.dumps({
                "schema": 1, "mode": "fallback", "total": 0,
                "expression": "all()", "shards": {}}))
            with patch.object(runner, "ROOT", root), \
                    patch.dict(os.environ, CI_PACKAGES='["fixture"]', CI_NIGHTLY="false"), \
                    patch.object(sys, "argv", ["run.py", "test", "--shard", "1"]), \
                    patch.object(runner, "run") as command:
                with self.assertRaises(ValueError,
                                       msg="a corrupt plan must not turn hosted coverage into a skip"):
                    runner.main()
                command.assert_not_called()

    def test_gate_junit_read_is_bounded(self):
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary) / "junit.xml"
            report.write_text("<testsuites>" + " " * 4096 + "</testsuites>",
                              encoding="utf-8")
            with patch.object(junit_report, "MAX_BYTES", 512):
                with self.assertRaises(ValueError,
                                       msg="a report must not bypass the byte bound"):
                    junit_report.executed_identities(report)


if __name__ == "__main__":
    unittest.main()
