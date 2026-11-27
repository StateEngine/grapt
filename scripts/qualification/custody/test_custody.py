#!/usr/bin/env python3
"""Adversarial evidence controls; QCU mutants load a copied receipt module."""

import copy
import ctypes
import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock


HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parents[2] / "fixtures/qualification-custody"
MODULE = Path(os.environ.get("QCU_MODULE_PATH", HERE / "receipt.py"))
spec = importlib.util.spec_from_file_location("qcu_receipt_under_test", MODULE)
custody = importlib.util.module_from_spec(spec)
spec.loader.exec_module(custody)

watch_spec = importlib.util.spec_from_file_location("qcu_source_watch", HERE / "source_watch.py")
watch_module = importlib.util.module_from_spec(watch_spec)
watch_spec.loader.exec_module(watch_module)


GOOD_MANIFEST = json.dumps({"prefix": "PLR", "mutations": [
    {"id": "PLR101", "expected_diagnostic": "intended diagnostic"},
]}).encode("utf-8")


def good_manifest():
    return custody.load_manifest(GOOD_MANIFEST)


def pinned_fixture(name, sha256):
    """Retained evidence bytes, exact up to a CRLF checkout conversion."""
    encoded = (FIXTURES / name).read_bytes()
    if hashlib.sha256(encoded).hexdigest() != sha256:
        encoded = encoded.replace(b"\r\n", b"\n")
    if hashlib.sha256(encoded).hexdigest() != sha256:
        raise AssertionError(f"retained fixture {name} differs from its pinned digest")
    return encoded


def genuine_qcu201():
    """Sanitized QCU201 receipt and its executed manifest bytes.

    Public fixture metadata uses neutral locations and coordination references;
    executable fingerprints and compiler-row bindings retain their values.
    """
    receipt = json.loads(pinned_fixture(
        "qcu201-genuine-receipt.json",
        "dc7f17b7bf13eb8a6bc27f3e66fc472828385ca9f94fe9eaeae4104562667af7"))
    manifest = custody.load_manifest(pinned_fixture(
        "qcu201-pinned-manifest.json",
        "5b0b10cf927e72e014c7d6bbe8b0676a6971bbb734008d108fd458408077c042"))
    return receipt, manifest


def good_entry():
    compiler = json.dumps({"reason": "compiler-artifact", "executable": "/evidence/binary"})
    green_compiler = json.dumps({"reason": "compiler-artifact", "executable": "/evidence/green"})
    artifact = {
        "path": "/evidence/binary", "sha256": "a" * 64,
        "sha256_after": "a" * 64, "compiler_row_json": compiler,
        "compiler_row_sha256": hashlib.sha256(compiler.encode()).hexdigest(),
    }
    return {
        "id": "PLR101", "expected_diagnostic": "intended diagnostic",
        "diagnostic": "panic: intended diagnostic", "source_changed": False,
        "artifact": artifact,
        "build": {"command": ["cargo", "test", "--no-run"], "exit": 0},
        "red": {"command": [artifact["path"], "--exact", "witness"],
                "exit": 101, "tests": 1, "duration_seconds": 0.1,
                "output_sha256": "b" * 64},
        "green": {"command": ["/evidence/green", "--exact", "witness"],
                  "exit": 0, "tests": 1, "duration_seconds": 0.1,
                  "output_sha256": "c" * 64,
                  "artifact": {"path": "/evidence/green", "sha256": "d" * 64,
                               "sha256_after": "d" * 64,
                               "compiler_row_json": green_compiler,
                               "compiler_row_sha256": hashlib.sha256(green_compiler.encode()).hexdigest()}},
    }


def good_receipt():
    identity = {"commit": "1" * 40, "tree": "2" * 40, "status": ""}
    harness = {"/evidence/harness": "e" * 64,
               "/evidence/manifest.json": good_manifest()["sha256"]}
    return {
        "schema": custody.SCHEMA, "outcome": "accepted",
        "candidate": {"before": identity, "after": identity.copy()},
        "harness": {"before": harness, "after": harness.copy()},
        "source_changed": False, "entries": [good_entry()],
    }


class CustodyControls(unittest.TestCase):
    def test_positive_receipt_survives_independent_readback(self):
        with tempfile.TemporaryDirectory(prefix="qcu-positive-") as directory:
            path = Path(directory) / "receipt.json"
            digest = custody.write_receipt(path, good_receipt(), ["PLR101"], good_manifest())
            self.assertEqual(digest, custody.sha256_file(path))
            self.assertEqual(
                custody.read_receipt(path, ["PLR101"], good_manifest())["outcome"], "accepted")

    def test_stale_executable_path_refuses(self):
        entry = good_entry()
        entry["red"]["command"][0] = "/evidence/stale-binary"
        with self.assertRaisesRegex(custody.CustodyError, "stale or unbound executed binary"):
            custody.verify_entry(entry, good_manifest()["rows"]["PLR101"])

    def test_compiler_row_executable_mismatch_refuses(self):
        receipt = good_receipt()
        entry = receipt["entries"][0]
        entry["artifact"]["path"] = "/evidence/stale-binary"
        entry["red"]["command"][0] = "/evidence/stale-binary"
        with self.assertRaisesRegex(custody.CustodyError, "compiler row executable differs"):
            custody.verify_receipt(receipt, ["PLR101"], good_manifest())

    def test_green_compiler_row_executable_mismatch_refuses(self):
        receipt = good_receipt()
        entry = receipt["entries"][0]
        entry["green"]["artifact"]["path"] = "/evidence/stale-green"
        entry["green"]["command"][0] = "/evidence/stale-green"
        with self.assertRaisesRegex(custody.CustodyError, "compiler row executable differs"):
            custody.verify_receipt(receipt, ["PLR101"], good_manifest())

    def test_compiler_row_reason_refuses(self):
        receipt = good_receipt()
        entry = receipt["entries"][0]
        row = json.dumps({"reason": "build-script-executed", "executable": "/evidence/binary"})
        entry["artifact"]["compiler_row_json"] = row
        entry["artifact"]["compiler_row_sha256"] = hashlib.sha256(row.encode()).hexdigest()
        with self.assertRaisesRegex(custody.CustodyError, "wrong reason"):
            custody.verify_receipt(receipt, ["PLR101"], good_manifest())

    def test_executable_changed_after_run_refuses(self):
        entry = good_entry()
        entry["artifact"]["sha256_after"] = "f" * 64
        with self.assertRaisesRegex(custody.CustodyError, "executed artifact digest changed"):
            custody.verify_entry(entry, good_manifest()["rows"]["PLR101"])

    def test_source_changed_during_run_refuses(self):
        entry = good_entry()
        entry["source_changed"] = True
        with self.assertRaisesRegex(custody.CustodyError, "source changed during run"):
            custody.verify_entry(entry, good_manifest()["rows"]["PLR101"])

    def test_wrong_mutant_diagnostic_refuses(self):
        entry = good_entry()
        entry["diagnostic"] = "panic: unrelated assertion"
        with self.assertRaisesRegex(custody.CustodyError, "wrong mutant diagnostic"):
            custody.verify_entry(entry, good_manifest()["rows"]["PLR101"])

    def test_genuine_receipt_is_accepted_against_its_pinned_manifest(self):
        receipt, manifest = genuine_qcu201()
        self.assertTrue(custody.verify_receipt(receipt, ["QCU201"], manifest))

    def test_unknown_source_observation_refuses_at_both_levels(self):
        # QCU-R3-3: absence or null is not a measured unchanged source.
        genuine, manifest = genuine_qcu201()
        shapes = {
            "receipt absent": lambda receipt: receipt.pop("source_changed"),
            "receipt null": lambda receipt: receipt.update(source_changed=None),
            "receipt malformed": lambda receipt: receipt.update(source_changed="false"),
            "entry null": lambda receipt: receipt["entries"][0].update(source_changed=None),
            "entry malformed": lambda receipt: receipt["entries"][0].update(source_changed=0),
        }
        for label, mutate in shapes.items():
            receipt = copy.deepcopy(genuine)
            mutate(receipt)
            with self.subTest(label), self.assertRaisesRegex(
                    custody.CustodyError, "source-change observation is absent or malformed"):
                custody.verify_receipt(receipt, ["QCU201"], manifest)
        receipt = copy.deepcopy(genuine)
        del receipt["entries"][0]["source_changed"]
        with self.assertRaisesRegex(custody.CustodyError, "QCU201: missing source_changed"):
            custody.verify_receipt(receipt, ["QCU201"], manifest)

    def test_receipt_expectation_is_bound_to_the_pinned_manifest_row(self):
        # QCU-R3-2: the receipt cannot supply the expectation it is judged by.
        genuine, manifest = genuine_qcu201()
        for expected in ("", "unrelated"):
            receipt = copy.deepcopy(genuine)
            receipt["entries"][0]["diagnostic"] = "unrelated failure"
            receipt["entries"][0]["expected_diagnostic"] = expected
            with self.subTest(expected=expected), self.assertRaisesRegex(
                    custody.CustodyError,
                    "receipt expectation differs from the pinned manifest row"):
                custody.verify_receipt(receipt, ["QCU201"], manifest)

    def test_manifest_must_be_pinned_and_declare_the_row(self):
        genuine, manifest = genuine_qcu201()
        rows = json.loads(pinned_fixture(
            "qcu201-pinned-manifest.json",
            "5b0b10cf927e72e014c7d6bbe8b0676a6971bbb734008d108fd458408077c042"))
        rows["mutations"][0]["expected_diagnostic"] = "unrelated"
        with self.assertRaisesRegex(custody.CustodyError, "not the one the harness pinned"):
            custody.verify_receipt(copy.deepcopy(genuine), ["QCU201"],
                                   custody.load_manifest(json.dumps(rows).encode("utf-8")))
        undeclared = {"sha256": manifest["sha256"], "rows": {
            "QCU201": dict(manifest["rows"]["QCU201"], expected_diagnostic=" ")}}
        with self.assertRaisesRegex(custody.CustodyError, "declares no expected diagnostic"):
            custody.verify_receipt(copy.deepcopy(genuine), ["QCU201"], undeclared)
        missing = {"sha256": manifest["sha256"], "rows": {}}
        with self.assertRaisesRegex(custody.CustodyError, "no declared row"):
            custody.verify_receipt(copy.deepcopy(genuine), ["QCU201"], missing)

    def test_absent_receipt_refuses(self):
        with tempfile.TemporaryDirectory(prefix="qcu-absent-") as directory:
            with self.assertRaisesRegex(custody.CustodyError, "absent or unreadable"):
                custody.read_receipt(Path(directory) / "missing.json", ["PLR101"],
                                     good_manifest())

    def test_candidate_changed_refuses(self):
        receipt = good_receipt()
        receipt["candidate"]["after"]["tree"] = "3" * 40
        with self.assertRaisesRegex(custody.CustodyError, "candidate changed"):
            custody.verify_receipt(receipt, ["PLR101"], good_manifest())

    def test_compiler_json_selects_exact_artifact(self):
        with tempfile.TemporaryDirectory(prefix="qcu-artifact-") as directory:
            root = Path(directory)
            source = root / "src/lib.rs"
            source.parent.mkdir()
            source.write_text("// fixture\n", encoding="utf-8")
            target = root / "target"
            target.mkdir()
            correct = target / "current"
            correct.write_bytes(b"current")
            stale = target / "stale"
            stale.write_bytes(b"stale")
            rows = [
                {"reason": "compiler-artifact", "target": {"src_path": str(root / "other.rs")},
                 "executable": str(stale)},
                {"reason": "compiler-artifact", "target": {"src_path": str(source)},
                 "executable": str(correct)},
            ]
            resolved = custody.resolve_artifact(
                "\n".join(json.dumps(row) for row in rows), source, target
            )
            self.assertEqual(resolved["path"], str(correct.resolve()))
            self.assertEqual(resolved["sha256"], custody.sha256_file(correct))

    def test_restored_edit_still_emits_source_event(self):
        with tempfile.TemporaryDirectory(prefix="qcu-watch-") as directory:
            source = Path(directory) / "CONTRACT.md"
            source.write_text("accepted\n", encoding="utf-8")
            with watch_module.SourceWatch(directory) as watch:
                source.write_text("changed during run\n", encoding="utf-8")
                source.write_text("accepted\n", encoding="utf-8")
                time.sleep(0.05)
            with self.assertRaisesRegex(watch_module.SourceChanged, "source changed"):
                watch.assert_unchanged()

    def test_source_directory_named_target_is_watched(self):
        with tempfile.TemporaryDirectory(prefix="qcu-watch-target-") as directory:
            source = Path(directory) / "target/CONTRACT.md"
            source.parent.mkdir()
            source.write_text("accepted\n", encoding="utf-8")
            with watch_module.SourceWatch(directory) as watch:
                source.write_text("edited and restored\n", encoding="utf-8")
                source.write_text("accepted\n", encoding="utf-8")
                time.sleep(0.05)
            with self.assertRaisesRegex(watch_module.SourceChanged, "source changed"):
                watch.assert_unchanged()

    @unittest.skipUnless(os.name == "nt", "Windows watcher API")
    def test_idle_windows_watch_closes_without_fake_overflow(self):
        with tempfile.TemporaryDirectory(prefix="qcu-watch-idle-") as directory:
            for _ in range(8):
                with watch_module.SourceWatch(directory) as watch:
                    pass
                watch.assert_unchanged()

    @unittest.skipUnless(os.name == "nt", "Windows watcher API")
    def test_active_windows_zero_byte_event_refuses_as_overflow(self):
        class ZeroByteKernel:
            def ReadDirectoryChangesW(self, *_args):
                ctypes.cast(_args[5], ctypes.POINTER(ctypes.c_ulong)).contents.value = 0
                return 1

        with tempfile.TemporaryDirectory(prefix="qcu-watch-overflow-") as directory:
            watch = watch_module.SourceWatch(directory)
            watch.kernel = ZeroByteKernel()
            watch.handle = 1
            watch._windows_loop()
            with self.assertRaisesRegex(watch_module.SourceChanged, "buffer overflow"):
                watch.assert_unchanged()

    def test_direct_witness_uses_package_cwd_and_exact_artifact(self):
        runner_path = HERE.parent / "plugin-linux-reap/run-mutations.py"
        runner_spec = importlib.util.spec_from_file_location("qcu_plr_runner", runner_path)
        runner = importlib.util.module_from_spec(runner_spec)
        runner_spec.loader.exec_module(runner)
        entry = {"package": "grapt-plugin", "test": "witness"}
        output = "running 1 test\ntest witness ... FAILED\ntest result: FAILED. 0 passed; 1 failed\n"
        fake = subprocess.CompletedProcess(["/exact/binary"], 101, output, "")
        budget = [1200.0]
        with mock.patch.object(runner.subprocess, "run", return_value=fake) as called:
            record, observed = runner.execute_witness(
                entry, "/candidate", {"path": "/exact/binary"},
                "/isolated-target", budget,
            )
        self.assertEqual(called.call_args.args[0][0], "/exact/binary")
        self.assertEqual(called.call_args.args[0][1:3], ["--exact", "witness"])
        self.assertEqual(
            called.call_args.kwargs["cwd"],
            os.path.join("/candidate", "crates", "grapt-plugin"),
        )
        self.assertEqual((record["exit"], record["tests"], observed), (101, 1, output))
        self.assertLess(budget[0], 1200.0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
