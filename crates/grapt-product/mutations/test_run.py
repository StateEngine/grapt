import contextlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path


RUNNER = Path(__file__).with_name("run.py")

# The shipped declaration, mirrored here so a temp-manifest fixture satisfies the
# same law the real manifest does. A fixture that could skip the declaration would
# be exercising a runner the governed route never uses.
RULED_INVENTORIES = {
    "IPR": {"lane": "IA0-1R", "rows": 22},
    "DGH": {"lane": "DOGFOOD-1", "rows": 7},
    "RMH": {"lane": "REAL-MODEL-HEADLESS-1", "rows": 2},
    "FS": {"lane": "FSR1", "rows": 4},
    "LH": {"lane": "LIVE-HEADLESS-1", "rows": 21},
    "DTV": {"lane": "D2A-RUST-TOOL-VERTICAL", "rows": 13},
    "PC": {"lane": "PC1-PROMPT-CACHE-ROUTING", "rows": 5},
    "PU": {"lane": "PC2-USAGE-TRUTH", "rows": 10},
    "PCU": {"lane": "PC2-CACHE-USAGE-PRODUCT-CLOSURE", "rows": 19},
    "RDT": {"lane": "RETAINED-TUI-RUST-DOGFOOD-01", "rows": 7},
    "SCO": {"lane": "RETAINED-TUI-RUST-DOGFOOD-01", "rows": 4},
    "DDR": {"lane": "RETAINED-DESKTOP-DOGFOOD-D4-D5", "rows": 5},
}


def load_runner():
    spec = importlib.util.spec_from_file_location("grapt_product_mutations", RUNNER)
    if spec is None or spec.loader is None:
        raise RuntimeError("product mutation runner could not be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RecursivePrefixCensusTest(unittest.TestCase):
    def test_nested_manifest_collision_is_refused(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary = root / "crates/grapt-product/mutations/manifest.json"
            primary.parent.mkdir(parents=True)
            primary.write_text(
                json.dumps(
                    {
                        "mutations": [
                            {"id": f"IPR{number:02d}"} for number in range(1, 23)
                        ]
                    }
                ),
                encoding="utf-8",
            )
            nested = root / "crates/grapt-types/mutations/auth/manifest.json"
            nested.parent.mkdir(parents=True)
            nested.write_text(
                json.dumps({"mutations": [{"id": "IPR99"}]}),
                encoding="utf-8",
            )
            runner.ROOT = root
            runner.MANIFEST = primary
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                runner.load_rows("IPR")
            self.assertIn(
                "IPR REFUSED: prefix collision in crates/grapt-types/mutations/auth/manifest.json",
                stderr.getvalue().replace("\\", "/"),
            )


class ExactInventoryTest(unittest.TestCase):
    """The shipped manifest must satisfy both independent inventories.

    IPR is the accepted product inventory and must not move; FS is this
    repair's fixture-staging inventory. Selection is by whole id token, so
    neither inventory can absorb or shrink the other.
    """

    def setUp(self):
        self.runner = load_runner()
        self.document = json.loads(
            Path(self.runner.MANIFEST).read_text(encoding="utf-8")
        )

    def test_ipr_inventory_is_unchanged(self):
        self.assertEqual(
            self.runner.EXACT_INVENTORIES["IPR"],
            [f"IPR{number:02d}" for number in range(1, 23)],
        )
        rows = self.runner.load_rows("IPR")
        self.assertEqual(
            [row["id"] for row in rows],
            [f"IPR{number:02d}" for number in range(1, 23)],
        )
        for row in rows:
            # IPR rows stay cargo rows: none may acquire the profile suite.
            self.assertNotIn("suite", row)
            self.assertTrue(row["test_target"], row["id"])
            evidence = row.get("reds", [row.get("red")])
            self.assertTrue(evidence, row["id"])
            self.assertNotIn(None, evidence, row["id"])
            edits = row.get("edits") or [row]
            for edit in edits:
                self.assertNotEqual(edit["old"], edit["new"], row["id"])
                target = Path(self.runner.ROOT) / edit["file"]
                text = target.read_text(encoding="utf-8").replace("\r\n", "\n")
                self.assertEqual(text.count(edit["old"]), 1, row["id"])

    def test_fs_inventory_is_exact_and_declares_named_red_evidence(self):
        self.assertEqual(
            self.runner.EXACT_INVENTORIES["FS"],
            ["FS01", "FS02", "FS03", "FS04"],
        )
        rows = self.runner.load_rows("FS")
        self.assertEqual([row["id"] for row in rows], ["FS01", "FS02", "FS03", "FS04"])
        for row in rows:
            self.assertTrue(row["reds"], row["id"])
            self.assertNotEqual(row["old"], row["new"], row["id"])
            target = Path(self.runner.ROOT) / row["file"]
            text = target.read_text(encoding="utf-8")
            text = text.replace(chr(13) + chr(10), chr(10))
            self.assertEqual(text.count(row["old"]), 1, row["id"])
        suites = {row["id"]: row.get("suite") for row in rows}
        self.assertEqual(
            suites,
            {"FS01": "profile", "FS02": "profile", "FS03": "profile", "FS04": None},
        )
        self.assertEqual(
            self.runner.command(next(r for r in rows if r["id"] == "FS01"))[1:],
            ["remote-compute/test_profile.py"],
        )
        self.assertEqual(
            self.runner.command(next(r for r in rows if r["id"] == "FS04")),
            [
                "cargo", "test", "-p", "grapt-session",
                "--test", "session_list_acceptance_corpus",
                "the_corpus_manifest_validates_against_the_ratified_schema",
                "--no-fail-fast",
            ],
        )

    def test_dgh_inventory_is_exact_and_names_each_counterexample(self):
        self.assertEqual(
            self.runner.EXACT_INVENTORIES["DGH"],
            [f"DGH{number:02d}" for number in (1, 2, 3, 4, 5, 6, 9)],
        )
        rows = self.runner.load_rows("DGH")
        self.assertEqual(
            [row["id"] for row in rows],
            [f"DGH{number:02d}" for number in (1, 2, 3, 4, 5, 6, 9)],
        )
        for row in rows:
            self.assertTrue(row["red"], row["id"])
            edits = row.get("edits") or [row]
            for edit in edits:
                self.assertNotEqual(edit["old"], edit["new"], row["id"])
                target = Path(self.runner.ROOT) / edit["file"]
                text = target.read_text(encoding="utf-8").replace("\r\n", "\n")
                self.assertEqual(text.count(edit["old"]), 1, row["id"])

    def test_lh_inventory_is_exact_feature_gated_and_named(self):
        self.assertEqual(
            self.runner.EXACT_INVENTORIES["LH"],
            [f"LH{number:02d}" for number in range(1, 22)],
        )
        rows = self.runner.load_rows("LH")
        self.assertEqual(
            [row["id"] for row in rows],
            [f"LH{number:02d}" for number in range(1, 22)],
        )
        expected_features = {
            "LH01": ["loopback-provider-testkit"],
            "LH02": ["loopback-provider-testkit"],
            "LH03": ["loopback-provider-testkit"],
            "LH04": ["loopback-provider-testkit"],
            "LH05": ["loopback-provider-testkit"],
            "LH06": ["loopback-testkit"],
            "LH07": ["loopback-provider-testkit"],
            "LH08": ["loopback-provider-testkit"],
            "LH09": ["loopback-provider-testkit"],
            "LH10": ["loopback-provider-testkit"],
            "LH11": [],
            "LH12": [],
            "LH13": [],
            "LH14": ["loopback-provider-testkit"],
            "LH15": ["loopback-provider-testkit"],
            "LH16": [],
            "LH17": [],
            "LH18": [],
            "LH19": [],
            "LH20": [],
            "LH21": [],
        }
        for row in rows:
            self.assertEqual(row["features"], expected_features[row["id"]])
            self.assertTrue(row.get("red") or row.get("reds"), row["id"])
            for edit in row.get("edits") or [row]:
                self.assertNotEqual(edit["old"], edit["new"], row["id"])
                target = Path(self.runner.ROOT) / edit["file"]
                text = target.read_text(encoding="utf-8").replace("\r\n", "\n")
                self.assertEqual(text.count(edit["old"]), 1, row["id"])
        self.assertEqual(
            self.runner.command(rows[0]),
            [
                "cargo", "test", "-p", "grapt-product",
                "--features", "loopback-provider-testkit",
                "--test", "dogfood_headless",
                "live_headless_wire_failure_and_resurrection_witnesses",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[3]),
            [
                "cargo", "test", "-p", "grapt-product",
                "--features", "loopback-provider-testkit",
                "--lib",
                "dogfood::execution::interrupt_truth_tests::coordinator_noop_and_error_never_leak_interrupted_terminal_truth",
                "--no-fail-fast",
            ],
        )

        self.assertEqual(
            self.runner.command(rows[5]),
            [
                "cargo", "test", "-p", "grapt-provider",
                "--features", "loopback-testkit",
                "--lib", "route::tests::loopback_base_requires_exact_parsed_authority",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[10]),
            [
                "cargo", "test", "-p", "grapt-session",
                "--lib", "history::tests::terminal_commit_is_projected_without_an_assistant_message",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[11]),
            [
                "cargo", "test", "-p", "grapt-types",
                "--test", "direct_include_format",
                "every_direct_include_carrier_is_pinned_rustfmt_clean",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[12]),
            [
                "cargo", "test", "-p", "grapt-types",
                "--test", "no_god_files",
                "rust_sources_do_not_create_or_grow_god_files",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[13]),
            [
                "cargo", "test", "-p", "grapt-product",
                "--features", "loopback-provider-testkit",
                "--test", "dogfood_headless",
                "provider_request_reader_bounds_header_body_and_aggregate_without_coercing_length",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[14]),
            [
                "cargo", "test", "-p", "grapt-product",
                "--features", "loopback-provider-testkit",
                "--test", "dogfood_headless",
                "live_headless_wire_failure_and_resurrection_witnesses",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[15]),
            [
                "cargo", "test", "-p", "grapt-product",
                "--lib",
                "dogfood::usage::tests::live_usage_projection_withholds_absence_preserves_reported_values_and_refuses_ceiling",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(self.runner.command(rows[16]), self.runner.command(rows[15]))
        self.assertEqual(
            self.runner.command(rows[17]),
            [
                "cargo", "test", "-p", "grapt-runtime",
                "--lib",
                "durable_owner::tests::usage_observations_reach_the_log_with_their_phase_and_absent_fields",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(
            self.runner.command(rows[18]),
            [
                "cargo", "test", "-p", "grapt-product",
                "--lib",
                "dogfood::usage::tests::bounded_carrier_and_aggregation_refuse_crossing_the_exact_numeric_ceiling",
                "--no-fail-fast",
            ],
        )
        self.assertEqual(self.runner.command(rows[19]), self.runner.command(rows[18]))

    def test_dtv_inventory_is_exact_and_names_the_continuation_mutants(self):
        self.assertEqual(
            self.runner.EXACT_INVENTORIES["DTV"],
            [f"DTV{number:02d}" for number in range(1, 14)],
        )
        rows = self.runner.load_rows("DTV")
        self.assertEqual(
            [row["id"] for row in rows],
            [f"DTV{number:02d}" for number in range(1, 14)],
        )
        for row in rows:
            self.assertTrue(row["red"], row["id"])
            for edit in row.get("edits") or [row]:
                target = Path(self.runner.ROOT) / edit["file"]
                text = target.read_text(encoding="utf-8").replace("\r\n", "\n")
                self.assertEqual(text.count(edit["old"]), 1, row["id"])

    def test_pu_inventory_is_exact_and_names_usage_truth_counterexamples(self):
        self.assertEqual(
            self.runner.EXACT_INVENTORIES["PU"],
            [f"PU{number:02d}" for number in range(1, 11)],
        )
        rows = self.runner.load_rows("PU")
        self.assertEqual(
            [row["id"] for row in rows],
            [f"PU{number:02d}" for number in range(1, 11)],
        )
        for row in rows:
            self.assertEqual(row["package"], "grapt-provider", row["id"])
            self.assertEqual(row["features"], [], row["id"])
            self.assertEqual(row["test_target"], "lib", row["id"])
            self.assertTrue(row["red"], row["id"])
            self.assertNotEqual(row["old"], row["new"], row["id"])
            target = Path(self.runner.ROOT) / row["file"]
            text = target.read_text(encoding="utf-8").replace("\r\n", "\n")
            self.assertEqual(text.count(row["old"]), 1, row["id"])
        for row in rows[:9]:
            self.assertEqual(
                row["test"],
                "usage::tests::language_neutral_normalization_corpus_executes_every_route",
                row["id"],
            )
        self.assertEqual(
            rows[9]["test"],
            "routes::tests::malformed_present_usage_terminates_every_decoder_route",
        )

    def test_pcu_inventory_is_exact_and_every_anchor_is_live(self):
        rows = self.runner.load_rows("PCU")
        self.assertEqual(
            [row["id"] for row in rows],
            [f"PCU{number:02d}" for number in range(1, 20)],
        )
        for row in rows:
            self.assertTrue(row["red"], row["id"])
            for edit in row.get("edits") or [row]:
                target = Path(self.runner.ROOT) / edit["file"]
                text = target.read_text(encoding="utf-8").replace("\r\n", "\n")
                self.assertEqual(text.count(edit["old"]), 1, row["id"])

    def test_load_rows_validates_every_selected_cargo_surface(self):
        seen = []
        real_validate = self.runner.validate_cargo_row

        def watched(row):
            seen.append(row["id"])
            return real_validate(row)

        self.runner.validate_cargo_row = watched
        rows = self.runner.load_rows("LH")
        self.assertEqual(seen, [row["id"] for row in rows])

    def test_impossible_package_target_and_feature_tuples_are_refused(self):
        rows = self.runner.load_rows("LH")
        cases = [
            (
                {**rows[3], "test_target": "no_such_test_target"},
                "unavailable integration-test target 'no_such_test_target'",
            ),
            ({**rows[5], "features": ["loopback-provider-testkit"]}, "unavailable features"),
            ({**rows[5], "package": "grapt-no-such-package"}, "unknown cargo package"),
        ]
        for row, expected in cases:
            with self.subTest(expected=expected):
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                    self.runner.validate_cargo_row(row)
                self.assertIn(expected, stderr.getvalue())

    def test_selection_is_by_whole_id_token(self):
        self.assertEqual(self.runner.id_token("FS01"), "FS")
        self.assertEqual(self.runner.id_token("IPR22"), "IPR")
        self.assertEqual(self.runner.id_token("FSX01"), "FSX")
        ids = [row["id"] for row in self.document["mutations"]]
        self.assertEqual(
            sorted({self.runner.id_token(value) for value in ids}),
            ["DDR", "DGH", "DTV", "FS", "IPR", "LH", "PC", "PCU", "PU", "RDT", "RMH", "SCO"],
        )

    def test_an_unruled_prefix_is_refused(self):
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            self.runner.load_rows("ZZ")
        self.assertIn("ZZ REFUSED: unknown prefix", stderr.getvalue())


class FsPrefixCollisionTest(unittest.TestCase):
    def test_nested_manifest_fs_collision_is_refused(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary = root / "crates/grapt-product/mutations/manifest.json"
            primary.parent.mkdir(parents=True)
            primary.write_text(
                json.dumps(
                    {"mutations": [{"id": f"FS{number:02d}"} for number in range(1, 5)]}
                ),
                encoding="utf-8",
            )
            nested = root / "crates/grapt-store/mutations/manifest.json"
            nested.parent.mkdir(parents=True)
            nested.write_text(
                json.dumps({"mutations": [{"id": "FS09"}]}), encoding="utf-8"
            )
            runner.ROOT = root
            runner.MANIFEST = primary
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                runner.load_rows("FS")
            self.assertIn(
                "FS REFUSED: prefix collision in crates/grapt-store/mutations/manifest.json",
                stderr.getvalue().replace("\\", "/"),
            )

    def test_a_neighbouring_token_is_not_a_collision(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary = root / "crates/grapt-product/mutations/manifest.json"
            primary.parent.mkdir(parents=True)
            primary.write_text(
                json.dumps(
                    {
                        "inventories": RULED_INVENTORIES,
                        "mutations": [
                            {"id": f"FS{number:02d}", "suite": "profile"}
                            for number in range(1, 5)
                        ],
                    }
                ),
                encoding="utf-8",
            )
            nested = root / "crates/grapt-store/mutations/manifest.json"
            nested.parent.mkdir(parents=True)
            nested.write_text(
                json.dumps({"mutations": [{"id": "FSX01"}]}), encoding="utf-8"
            )
            runner.ROOT = root
            runner.MANIFEST = primary
            rows = runner.load_rows("FS")
            self.assertEqual([row["id"] for row in rows], ["FS01", "FS02", "FS03", "FS04"])


class DeclaredInventoryAuthorityTest(unittest.TestCase):
    """The declared count, the rows present and the runner inventory are one law.

    Before this, `inventories` was read by nothing. A census nobody asks cannot
    be wrong, so it was free to drift away from the rows it claimed to describe.
    """

    def _manifest(self, directory, inventories, ids):
        root = Path(directory)
        primary = root / "crates/grapt-product/mutations/manifest.json"
        primary.parent.mkdir(parents=True)
        primary.write_text(
            json.dumps({"inventories": inventories, "mutations": [{"id": i} for i in ids]}),
            encoding="utf-8",
        )
        runner = load_runner()
        runner.ROOT = root
        runner.MANIFEST = primary
        return runner

    def _refuses(self, runner, prefix, needle):
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
            runner.load_rows(prefix)
        self.assertIn(needle, stderr.getvalue())

    def test_a_declared_count_that_drifts_from_the_rows_is_refused(self):
        drifted = dict(RULED_INVENTORIES)
        drifted["FS"] = {"lane": "FSR1", "rows": 5}
        with tempfile.TemporaryDirectory() as directory:
            runner = self._manifest(
                directory, drifted, [f"FS{n:02d}" for n in range(1, 5)]
            )
            self._refuses(runner, "FS", "declared FS row count 5 does not match")

    def test_a_declared_family_set_that_drifts_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = self._manifest(
                directory,
                {"IPR": {"rows": 22}},
                [f"FS{n:02d}" for n in range(1, 5)],
            )
            self._refuses(runner, "FS", "are not the ruled families")

    def test_a_missing_inventories_block_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary = root / "crates/grapt-product/mutations/manifest.json"
            primary.parent.mkdir(parents=True)
            primary.write_text(
                json.dumps({"mutations": [{"id": f"FS{n:02d}"} for n in range(1, 5)]}),
                encoding="utf-8",
            )
            runner = load_runner()
            runner.ROOT = root
            runner.MANIFEST = primary
            self._refuses(runner, "FS", "manifest declares no inventories block")

    def test_the_shipped_manifest_declares_exactly_what_it_carries(self):
        runner = load_runner()
        document = json.loads(Path(runner.MANIFEST).read_text(encoding="utf-8"))
        for prefix, ids in runner.EXACT_INVENTORIES.items():
            self.assertEqual(document["inventories"][prefix]["rows"], len(ids), prefix)


class GovernedReachabilityTest(unittest.TestCase):
    """A passing full gate with FS unexecuted must be impossible."""

    def test_the_no_argument_default_draws_every_ruled_family(self):
        runner = load_runner()
        self.assertEqual(sorted(runner.RULED_ORDER), sorted(runner.EXACT_INVENTORIES))

    def test_ruled_order_is_exact_and_reversing_it_is_a_counterexample(self):
        runner = load_runner()
        # The pin. IPR is the accepted product inventory and runs first; FS is
        # the fixture-staging family added by FSR1.
        self.assertEqual(
            runner.RULED_ORDER,
            ("IPR", "DGH", "RMH", "FS", "LH", "DTV", "PC", "PU", "PCU", "RDT", "SCO", "DDR"),
        )
        # Reversible wrong-sequence counterexample: the reversed order is a
        # different sequence and this pin rejects it. Sorted-set equality alone
        # would accept it, which is exactly the weakness being closed.
        self.assertNotEqual(
            tuple(reversed(runner.RULED_ORDER)),
            ("IPR", "DGH", "RMH", "FS", "LH", "DTV", "PC", "PU", "PCU", "RDT", "SCO", "DDR"),
        )
        self.assertEqual(
            sorted(tuple(reversed(runner.RULED_ORDER))), sorted(runner.RULED_ORDER)
        )


class WorktreeIsolationTest(unittest.TestCase):
    def test_worktree_env_pins_cargo_target_dir_inside_the_worktree(self):
        runner = load_runner()
        worktree = Path(tempfile.gettempdir()) / "grapt-isolation-probe"
        environment = runner.worktree_env(worktree)
        self.assertEqual(environment["CARGO_TARGET_DIR"], str(worktree / "target"))

    def test_a_shared_ambient_target_cannot_survive_into_a_row(self):
        runner = load_runner()
        worktree = Path(tempfile.gettempdir()) / "grapt-isolation-probe"
        with unittest.mock.patch.dict(
            os.environ, {"CARGO_TARGET_DIR": "/shared/governed/cargo-target"}
        ):
            environment = runner.worktree_env(worktree)
        self.assertNotEqual(environment["CARGO_TARGET_DIR"], "/shared/governed/cargo-target")
        self.assertEqual(environment["CARGO_TARGET_DIR"], str(worktree / "target"))


class ResidueBackstopTest(unittest.TestCase):
    """Residue detection must cover non-Rust targets, which the old scan missed."""

    def _runner(self, directory, rows, on_disk, blobs):
        root = Path(directory)
        primary = root / "crates/grapt-product/mutations/manifest.json"
        primary.parent.mkdir(parents=True)
        primary.write_text(json.dumps({"mutations": rows}), encoding="utf-8")
        for relative, text in on_disk.items():
            target = root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
        runner = load_runner()
        runner.ROOT = root
        runner.MANIFEST = primary

        def fake_git_blob(head, relative):
            result = unittest.mock.Mock()
            result.returncode = 0 if relative in blobs else 1
            result.stdout = blobs.get(relative, "").encode("utf-8")
            result.stderr = b""
            return result

        runner.git_blob = fake_git_blob
        return runner

    def test_declared_targets_include_non_rust_files(self):
        runner = load_runner()
        targets = runner.declared_targets()
        self.assertIn("remote-compute/secode-linux.json", targets)
        self.assertIn("remote-compute/secode-linux-evidence.json", targets)
        self.assertIn(
            "fixtures/rust-reconstruction/session-list-keyset-v1/manifest.json", targets
        )
        # The old backstop globbed crates/**/src/**/*.rs only, so none of the
        # three above could ever have been checked.
        self.assertTrue(any(not t.endswith(".rs") for t in targets))

    def test_a_byte_identical_tree_passes(self):
        rows = [{"id": "FS01", "file": "remote-compute/secode-linux.json"}]
        with tempfile.TemporaryDirectory() as directory:
            runner = self._runner(
                directory,
                rows,
                {"remote-compute/secode-linux.json": '{"a": 1}\n'},
                {"remote-compute/secode-linux.json": '{"a": 1}\n'},
            )
            self.assertEqual(runner.residue_backstop("deadbeef"), 1)

    def test_utf8_bytes_do_not_depend_on_the_windows_text_locale(self):
        rows = [{"id": "FS01", "file": "remote-compute/secode-linux.json"}]
        with tempfile.TemporaryDirectory() as directory:
            runner = self._runner(
                directory,
                rows,
                {"remote-compute/secode-linux.json": '"§ —"\n'},
                {"remote-compute/secode-linux.json": '"§ —"\n'},
            )
            self.assertEqual(runner.residue_backstop("deadbeef"), 1)

    def test_residue_in_a_non_rust_target_is_detected(self):
        rows = [{"id": "FS01", "file": "remote-compute/secode-linux.json"}]
        with tempfile.TemporaryDirectory() as directory:
            runner = self._runner(
                directory,
                rows,
                {"remote-compute/secode-linux.json": '{"a": 2}\n'},
                {"remote-compute/secode-linux.json": '{"a": 1}\n'},
            )
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                runner.residue_backstop("deadbeef")
            message = stderr.getvalue()
            self.assertIn("mutation residue survived", message)
            self.assertIn("remote-compute/secode-linux.json: differs from HEAD", message)


class _Result:
    """A subprocess.CompletedProcess stand-in with only what the runner reads."""

    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class RestoredWitnessTest(unittest.TestCase):
    def test_same_checkout_must_pass_after_exact_byte_restoration(self):
        runner = load_runner()
        row = {"id": "RDT06"}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "source.rs").write_bytes(b"mutated")
            runner.git_blob = lambda head, relative: _Result(0, b"original\n")
            runner.command = lambda row: ["cargo", "test"]
            def verify(args, cwd=None, env=None):
                self.assertEqual(cwd, path)
                self.assertEqual((path / "source.rs").read_bytes(), b"original\n")
                self.assertEqual(env["CARGO_TARGET_DIR"], str(path / "target"))
                return _Result(0)
            runner.run = verify
            runner.restore_and_verify(row, path, "pinned", [{"file": "source.rs"}])

    def test_failure_of_the_restored_witness_refuses_attribution(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            runner.git_blob = lambda head, relative: _Result(0, b"original\n")
            runner.command = lambda row: ["cargo", "test"]
            runner.run = lambda *args, **kwargs: _Result(101, "fixture is broken")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                runner.restore_and_verify({"id": "RDT06"}, Path(directory), "pinned", [{"file": "source.rs"}])
            self.assertIn("red is not attributed", stderr.getvalue())


class GovernedMainDispatchTest(unittest.TestCase):
    """Drive main(), not its constants.

    The previous reachability tests asserted properties of RULED_ORDER and of
    the profile argv. Both are true of a runner that never draws a family: the
    original defect (`families = ["IPR"]`) leaves every one of those assertions
    green. Only calling main() and observing which families it actually draws
    can tell the two apart.
    """

    REVISION = "0123456789abcdef0123456789abcdef01234567"

    def _drive_main(self, argv, runner=None):
        runner = runner or load_runner()
        drawn = []
        residue_calls = []
        coverage_calls = []

        def fake_run(args, cwd=None, env=None):
            if args[:2] == ["git", "rev-parse"]:
                return _Result(0, self.REVISION + "\n", "")
            return _Result(0, "", "")

        def fake_run_family(prefix, head):
            drawn.append(prefix)
            return len(runner.EXACT_INVENTORIES[prefix])

        def fake_residue(revision):
            residue_calls.append(revision)
            return 0

        real_coverage = runner.coverage_refusal

        def watched_coverage(executed):
            coverage_calls.append(dict(executed))
            return real_coverage(executed)

        runner.run = fake_run
        runner.run_family = fake_run_family
        runner.residue_backstop = fake_residue
        runner.coverage_refusal = watched_coverage
        with tempfile.TemporaryDirectory() as empty:
            runner.ROOT = Path(empty)
            stdout, stderr = io.StringIO(), io.StringIO()
            code = None
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                with unittest.mock.patch.object(sys, "argv", ["run.py"] + argv):
                    try:
                        runner.main()
                    except SystemExit as exit_signal:
                        code = exit_signal.code
        return {
            "drawn": drawn,
            "residue_calls": residue_calls,
            "coverage_calls": coverage_calls,
            "exit": code,
            "stdout": stdout.getvalue(),
            "stderr": stderr.getvalue(),
        }

    # Correction item 1.
    def test_the_no_argument_run_draws_every_family_in_ruled_order(self):
        outcome = self._drive_main([])
        self.assertEqual(
            outcome["drawn"],
            ["IPR", "DGH", "RMH", "FS", "LH", "DTV", "PC", "PU", "PCU", "RDT", "SCO", "DDR"],
            "the governed no-argument form must draw every ruled family, in order",
        )
        self.assertIsNone(outcome["exit"])

    def test_an_explicit_prefix_draws_only_that_family(self):
        outcome = self._drive_main(["--prefix", "FS"])
        self.assertEqual(outcome["drawn"], ["FS"])

    # Correction item 2.
    def _exact_counts(self):
        runner = load_runner()
        return {prefix: len(ids) for prefix, ids in runner.EXACT_INVENTORIES.items()}

    def test_the_coverage_guard_accepts_only_the_exact_counts(self):
        runner = load_runner()
        self.assertIsNone(runner.coverage_refusal(self._exact_counts()))

    def test_the_coverage_guard_refuses_a_missing_family(self):
        runner = load_runner()
        refusal = runner.coverage_refusal({"IPR": 22})
        self.assertIsNotNone(refusal, "an incomplete draw must be refused")
        self.assertIn("missing ruled families", refusal)
        self.assertIn("'FS'", refusal)

    def test_the_coverage_guard_refuses_an_unknown_family(self):
        runner = load_runner()
        executed = self._exact_counts()
        executed["ZZ"] = 1
        refusal = runner.coverage_refusal(executed)
        self.assertIsNotNone(refusal)
        self.assertIn("unknown families ['ZZ']", refusal)

    def test_the_coverage_guard_refuses_a_short_family_count(self):
        runner = load_runner()
        executed = self._exact_counts()
        executed["LH"] = 3
        refusal = runner.coverage_refusal(executed)
        self.assertIsNotNone(refusal)
        self.assertIn("LH returned short count 3; expected 21", refusal)

    def test_the_coverage_guard_refuses_a_long_family_count(self):
        runner = load_runner()
        executed = self._exact_counts()
        executed["LH"] = 22
        refusal = runner.coverage_refusal(executed)
        self.assertIsNotNone(refusal)
        self.assertIn("LH returned long count 22; expected 21", refusal)

    def test_the_coverage_guard_refuses_a_non_integer_count(self):
        runner = load_runner()
        executed = self._exact_counts()
        executed["LH"] = "21"
        refusal = runner.coverage_refusal(executed)
        self.assertIsNotNone(refusal)
        self.assertIn("LH returned invalid row count '21'", refusal)

    def test_main_consults_the_coverage_guard_on_the_no_argument_form(self):
        outcome = self._drive_main([])
        self.assertEqual(
            outcome["coverage_calls"],
            [
                {
                    "IPR": 22,
                    "DGH": 7,
                    "RMH": 2,
                    "FS": 4,
                    "LH": 21,
                    "DTV": 13,
                    "PC": 5,
                    "PU": 10,
                    "PCU": 19,
                    "RDT": 7,
                    "SCO": 4,
                    "DDR": 5,
                }
            ],
            "main must submit what it actually executed to the coverage guard",
        )

    def test_an_explicit_prefix_is_not_subject_to_the_coverage_guard(self):
        outcome = self._drive_main(["--prefix", "FS"])
        self.assertEqual(outcome["coverage_calls"], [])

    # Correction item 3, second half.
    def test_main_calls_the_residue_backstop_with_the_resolved_revision(self):
        outcome = self._drive_main([])
        self.assertEqual(
            outcome["residue_calls"],
            [self.REVISION],
            "main must run the residue backstop against the resolved revision",
        )


class RowExecutionBindingTest(unittest.TestCase):
    """Drive run_family against a real worktree.

    Correction items 3 (first half) and 4. The helpers were witnessed and the
    call sites were not, so `worktree_env` could be perfect and unused, and the
    revision could be re-broken with the census still green.
    """

    def _repository(self, directory):
        root = Path(directory)
        (root / "crates/grapt-product/mutations").mkdir(parents=True)
        subject = root / "crates/subject"
        (subject / "src").mkdir(parents=True)
        (subject / "Cargo.toml").write_text(
            '[package]\nname = "subject"\nversion = "0.1.0"\nedition = "2021"\n',
            encoding="utf-8",
        )
        (subject / "src/lib.rs").write_text("pub fn subject() {}\n", encoding="utf-8")
        target = root / "subject.txt"
        target.write_text("GUARD\n", encoding="utf-8", newline="\n")
        for command in (
            ["git", "init", "--quiet", str(root)],
            ["git", "-C", str(root), "config", "user.email", "t@example.invalid"],
            ["git", "-C", str(root), "config", "user.name", "t"],
            ["git", "-C", str(root), "add", "subject.txt", "crates/subject"],
            ["git", "-C", str(root), "commit", "--quiet", "-m", "seed"],
        ):
            subprocess.run(command, check=True, capture_output=True)
        revision = subprocess.run(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
        return root, revision

    def _run_family_capturing(self, root, revision, row_id="FS01"):
        runner = load_runner()
        runner.ROOT = root
        manifest = root / "crates/grapt-product/mutations/manifest.json"
        manifest.write_text(
            json.dumps(
                {
                    "inventories": {"IPR": {"rows": 22}, "FS": {"rows": 1}},
                    "mutations": [
                        {
                            "id": row_id,
                            "package": "subject",
                            "test_target": "lib",
                            "test": "t",
                            "file": "subject.txt",
                            "old": "GUARD",
                            "new": "MUTATED",
                            "reds": ["named evidence"],
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
        runner.MANIFEST = manifest
        runner.EXACT_INVENTORIES = {"IPR": [f"IPR{n:02d}" for n in range(1, 23)], "FS": [row_id]}
        seen = {}
        real_run = runner.run

        def watched(args, cwd=None, env=None):
            if args and args[0] == "git":
                # `run`'s `cwd=ROOT` default is bound at definition time, so
                # reassigning runner.ROOT does not redirect it. Aim git at this
                # fixture's repository explicitly.
                return real_run(args, cwd=cwd if cwd is not None else root, env=env)
            if args and args[0] == "cmd":
                # `remove_worktree`'s Windows link scan. Not the row, and it
                # would otherwise clobber the captured row environment.
                return _Result(0, "", "")
            # Observe the bytes, not invocation order: only the mutant fails.
            seen["env"] = env
            seen["cwd"] = cwd
            subject = (Path(cwd) / "subject.txt").read_bytes()
            seen.setdefault("subjects", []).append(subject)
            return _Result(0) if subject == b"GUARD\n" else _Result(1, "named evidence", "")

        runner.run = watched
        stdout = io.StringIO()
        # `fail()` raises SystemExit, a BaseException that unittest does NOT
        # catch: it would abort the whole run at this point and truncate every
        # later result, which makes a mutation sweep read differently from one
        # invocation to the next. Convert it into an ordinary failure so the
        # suite reports deterministically.
        try:
            with contextlib.redirect_stdout(stdout):
                count = runner.run_family("FS", revision)
        except SystemExit as refusal:
            raise AssertionError(
                f"run_family refused (exit {refusal.code}): {stdout.getvalue().strip()}"
            ) from refusal
        return runner, seen, count

    def test_run_family_hands_each_row_a_worktree_scoped_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root, revision = self._repository(directory)
            runner, seen, count = self._run_family_capturing(root, revision, "FS01")
            self.assertEqual(count, 1)
            self.assertEqual(seen["subjects"], [b"MUTATED\n", b"GUARD\n"])
            self.assertIn("env", seen, "the row command must receive an explicit environment")
            target = seen["env"].get("CARGO_TARGET_DIR")
            self.assertIsNotNone(target, "the row must be given a CARGO_TARGET_DIR")
            self.assertEqual(
                target,
                str(Path(seen["cwd"]) / "target"),
                "the row's target must live INSIDE its own disposable worktree",
            )

    def test_an_ambient_shared_target_does_not_reach_a_row(self):
        with tempfile.TemporaryDirectory() as directory:
            root, revision = self._repository(directory)
            with unittest.mock.patch.dict(
                os.environ, {"CARGO_TARGET_DIR": "/governed/shared/cargo-target"}
            ):
                runner, seen, _ = self._run_family_capturing(root, revision, "FS02")
            self.assertNotEqual(
                seen["env"].get("CARGO_TARGET_DIR"), "/governed/shared/cargo-target"
            )

    # Correction item 4.
    def test_run_family_accepts_a_resolved_revision_string(self):
        with tempfile.TemporaryDirectory() as directory:
            root, revision = self._repository(directory)
            # Reaching the row at all proves `git worktree add` was given the
            # revision itself. Restoring `head.stdout.strip()` raises
            # AttributeError here, which is the defect the second commit fixed
            # and which the census could not previously see.
            runner, seen, count = self._run_family_capturing(root, revision, "FS03")
            self.assertEqual(count, 1)
            self.assertIn("env", seen, "no row ran, so `git worktree add` never succeeded")


if __name__ == "__main__":
    unittest.main()
