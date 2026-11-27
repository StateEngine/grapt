import copy
import hashlib
import importlib.util
import json
import unittest
from pathlib import Path


PROFILE_ROOT = Path(__file__).parent
REPO_ROOT = PROFILE_ROOT.parent
BASE = json.loads((PROFILE_ROOT / "secode-linux.json").read_text(encoding="utf-8"))
EVIDENCE = json.loads((PROFILE_ROOT / "secode-linux-evidence.json").read_text(encoding="utf-8"))
QCU = json.loads((PROFILE_ROOT / "qualification-custody.json").read_text(encoding="utf-8"))
EVIDENCE_TESTS = (
    "crates/grapt-event/tests/era_hashes.rs",
    "crates/grapt-event/tests/legacy_clone.rs",
    "crates/grapt-inbox/tests/clone_inbox.rs",
    "crates/grapt-inbox/tests/inbox_admission.rs",
    "crates/grapt-instruction/tests/clone_instructions.rs",
    "crates/grapt-permission/tests/clone_permissions.rs",
    "crates/grapt-permission/tests/permission_service.rs",
    "crates/grapt-session/tests/clone_messages.rs",
    "crates/grapt-session/tests/clone_session.rs",
    "crates/grapt-store/tests/clone_audit.rs",
    "crates/grapt-store/tests/clone_compaction_order.rs",
    "crates/grapt-store/tests/clone_export.rs",
    "crates/grapt-store/tests/clone_migrate.rs",
    "crates/grapt-store/tests/clone_replay.rs",
)
FIXTURE_CALLERS = (
    "crates/grapt-form/tests/form_service.rs",
    "crates/grapt-instruction/tests/f4a_corpus_rows.rs",
    "crates/grapt-permission/tests/permission_service.rs",
    "crates/grapt-provider/tests/d2_transcripts.rs",
    "crates/grapt-shell/tests/shell_operation.rs",
    "crates/grapt-subagent/tests/subagent_operation.rs",
)
FIXTURE_CALLER_FRAGMENTS = {
    "crates/grapt-subagent/tests/subagent_operation.rs": (
        "crates/grapt-subagent/tests/subagent_operation/lifecycle_claim.rs",
        "crates/grapt-subagent/tests/subagent_operation/delivery_persistence.rs",
        "crates/grapt-subagent/tests/subagent_operation/recovery_concurrency.rs",
    ),
}
FORMER_LINUX_FAILURES = (
    "executes_the_frozen_fixture_with_faithful_durable_data",
    "the_capped_fork_row_rebuilds_its_declared_ancestry_and_exclusions",
    "the_capped_fork_row_durable_log_matches_the_declared_boundary",
    "the_no_progress_row_reproduces_every_declared_outcome",
    "the_no_progress_row_commits_no_durable_fact",
    "executes_the_frozen_reject_cascade_fixture_with_literal_durable_data",
    "reasoning_text_usage_stop_maps_onto_the_alphabet",
    "usage_is_never_a_standalone_fragment_in_the_oracle",
    "malformed_fragment_yields_one_step_failure_where_the_oracle_recorded_none",
    "interrupt_midstream_settles_every_started_call",
    "straggler_asymmetry_is_ignored_evidence_not_a_second_settlement",
    "every_d2_scenario_declares_its_waivers",
    "a_real_session_still_reads_correctly_beside_a_same_identity_campaign_aggregate",
    "reproduces_the_frozen_shell_observed_state_from_preserved_values",
    "frozen_synthetic_delivery_is_inert_audit_input",
)


SHARED_STORE_MUTATION_INPUTS = (
    {"id": "ratified-manifest-schema",
     "local_path": "secode/fixtures/rust-reconstruction/manifest.schema.json",
     "remote_path": "fixtures/rust-reconstruction/manifest.schema.json",
     "bytes": 8257,
     "sha256": "372f9cbaf4dd785feb7fb484a728fc1e00bc1f6dc9ac5e6796c765b99da6ff79",
     "mode": "0444"},
    {"id": "ratified-oracle-receipt-schema",
     "local_path": "secode/fixtures/rust-reconstruction/oracle-receipt-manifest.schema.json",
     "remote_path": "fixtures/rust-reconstruction/oracle-receipt-manifest.schema.json",
     "bytes": 7907,
     "sha256": "6d855521e8107668025de7461cc0742654eac316ebcdba32a396c9ccb632f07f",
     "mode": "0444"},
    {"id": "ratified-law-pack",
     "local_path": "secode/fixtures/rust-reconstruction/law-pack.json",
     "remote_path": "fixtures/rust-reconstruction/law-pack.json",
     "bytes": 1983,
     "sha256": "d33d1592c27845f9a0f7ab9c4f721106b2437dbbbc1b725b45355c4ce99e08d8",
     "mode": "0444"},
    {"id": "campaign-replay-actions",
     "local_path": "secode/fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/actions.jsonl",
     "remote_path": "fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/actions.jsonl",
     "bytes": 2607,
     "sha256": "5c4f3eac0097883a660527159d9fb66bb2f009cccc4a527ee42e41b991264874",
     "mode": "0444"},
    {"id": "campaign-replay-durable",
     "local_path": "secode/fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/expected/durable.jsonl",
     "remote_path": "fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/expected/durable.jsonl",
     "bytes": 982,
     "sha256": "1cc8124b28d555f548a6ae332e6c7ed4f57543d1745cf8320266d9fafbeba40c",
     "mode": "0444"},
    {"id": "campaign-replay-state",
     "local_path": "secode/fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/expected/state.json",
     "remote_path": "fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/expected/state.json",
     "bytes": 2120,
     "sha256": "a1abcfad153e232ee0f9ed5fc35546d1e5c1968436ba8a840e3a1624b6015d40",
     "mode": "0444"},
    {"id": "campaign-replay-waivers",
     "local_path": "secode/fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/expected/waivers.json",
     "remote_path": "fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/expected/waivers.json",
     "bytes": 1641,
     "sha256": "d39f9802455593fab0fff3324a8ebb94184e061e49a59b51f4957527788a8c6c",
     "mode": "0444"},
    {"id": "campaign-replay-manifest",
     "local_path": "secode/fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/manifest.json",
     "remote_path": "fixtures/rust-reconstruction/campaign-node-reservation-spend-replay/manifest.json",
     "bytes": 6090,
     "sha256": "da4c20b4970831a3f19bbd0adbfc4bc8cd24cc85f04fcd34616ed00c4e361444",
     "mode": "0444"},
)


RECONNECT_INPUTS = (
    {"id": "reconnect-manifest",
     "local_path": "secode/fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/manifest.json",
     "remote_path": "fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/manifest.json",
     "bytes": 2344,
     "sha256": "6d1cc6e48724f8a1b0aedfa63b6f07af743f1aa6f50db8c936474bd093ae0372",
     "mode": "0444"},
    {"id": "reconnect-actions",
     "local_path": "secode/fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/actions.jsonl",
     "remote_path": "fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/actions.jsonl",
     "bytes": 1747,
     "sha256": "e0421d98c411050df04b7842be65f9b3ada5c3f28a61aa8c080a29880b0b23f5",
     "mode": "0444"},
    {"id": "reconnect-durable",
     "local_path": "secode/fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/expected/durable.jsonl",
     "remote_path": "fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/expected/durable.jsonl",
     "bytes": 1290,
     "sha256": "05c869656dc8ebe56d05c20ae8b5e4faf8b00c83eb03ea978d8430d5682147cd",
     "mode": "0444"},
    {"id": "reconnect-state",
     "local_path": "secode/fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/expected/state.json",
     "remote_path": "fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/expected/state.json",
     "bytes": 362,
     "sha256": "0b963f4de26f485a30c9dec6ff3e4b1fe5df7ad3dbb97a860455f1bb03f6b2b0",
     "mode": "0444"},
    {"id": "reconnect-waivers",
     "local_path": "secode/fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/expected/waivers.json",
     "remote_path": "fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/expected/waivers.json",
     "bytes": 546,
     "sha256": "9cc4685559b79241e14cb824e67342efd060307f6c4f8bf4555afd0ad111ce29",
     "mode": "0444"},
)


def inventory_mismatches(artifacts, ruled):
    """Name every ruled input row that is absent or altered in `artifacts`.

    Reported by id and field so a broken profile names the row instead of only
    moving an aggregate count.
    """
    present = {}
    for artifact in artifacts:
        present.setdefault(artifact["id"], artifact)
    mismatches = []
    for expected in ruled:
        actual = present.get(expected["id"])
        if actual is None:
            mismatches.append("%s: absent" % expected["id"])
            continue
        for field in ("local_path", "remote_path", "bytes", "sha256", "mode"):
            if actual.get(field) != expected[field]:
                mismatches.append(
                    "%s: %s is %r, ruled %r"
                    % (expected["id"], field, actual.get(field), expected[field])
                )
    return mismatches


def reconnect_mismatches(artifacts):
    """Name every absent or altered frozen reconnect corpus row.

    The three grapt-runtime fixture-twelve tests cannot run without this
    five-file corpus, and neither canonical profile staged it before this
    repair. Both profiles must carry all five rows identically.
    """
    return inventory_mismatches(artifacts, RECONNECT_INPUTS)


def shared_input_mismatches(artifacts):
    """Name every shared store-mutation input row that is absent or altered.

    Every governed profile able to execute crates/grapt-store/mutations/run.py
    must carry these eight rows with identical ids, local paths, remote paths,
    byte counts, SHA-256 pins, and mode. A missing or edited row is reported by
    id, so a broken profile names the row instead of only moving an aggregate.
    """
    return inventory_mismatches(artifacts, SHARED_STORE_MUTATION_INPUTS)


def expected_class_domains(name):
    # Coordinator scheduling pins these two campaigns to existing carriers;
    # neither creates a third cache root or permits arbitrary domain subsets.
    return {
        "retained-decision": ["domain0"],
        "retained-compatibility": ["domain1"],
        # 8807046ba pinned the desktop-recovery class to carrier0 so the DTV
        # family draws the governed Linux carrier once. desktop-history and
        # dogfood-cli-convergence (1b5a3084, 815da3de) route to the same class.
        "desktop-recovery": ["domain0"],
    }.get(name, ["domain0", "domain1"])


def stable_carrier_projection(profile, run_id):
    resources = profile.get("resources", {})
    if "admission" in json.dumps(resources, sort_keys=True) or "per-run" in json.dumps(resources, sort_keys=True):
        raise ValueError("per-run admission resurrected")
    domains = resources.get("domains")
    if not isinstance(domains, dict) or tuple(domains) != ("domain0", "domain1"):
        raise ValueError("exact two stable domains are required")
    if any(resource_class.get("domains") != expected_class_domains(name) for name, resource_class in resources["classes"].items()):
        raise ValueError("resource class escaped the stable domains")
    cache = profile["cache"]
    root = profile["target"]["remote_run_root"].rstrip("/") + "/cache"
    return tuple(
        {
            "run_id": run_id,
            "lease": (profile["profile_id"], domain_id),
            "root": f"{root}/{cache['namespace']}/{domain['cache_namespace']}/{cache['key']}",
        }
        for domain_id, domain in domains.items()
    )


class SeCodeRemoteProfileTests(unittest.TestCase):
    def test_release_collection_names_the_builders_produced_artifacts(self):
        spec = importlib.util.spec_from_file_location(
            "release_shipped", REPO_ROOT / "scripts/release/shipped.py")
        shipped = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(shipped)
        profile = json.loads((PROFILE_ROOT / "release-packaging-linux.json").read_text())
        artifacts = {row["id"]: row for row in profile["evidence"]["artifacts"]}
        for platform, directory, suffix in (
                ("linux", "artifacts", ""), ("windows", "artifacts-windows", ".exe")):
            with self.subTest(platform=platform):
                selected = profile["commands"][f"release-{platform}-artifacts"]["artifacts"]
                collected = {artifacts[key]["path"] for key in selected
                             if not artifacts[key]["path"].endswith(".json")}
                produced = {f"{directory}/{name}" for name in shipped.names(suffix)}
                self.assertEqual(collected, produced,
                                 "release collection must request the files the builder produces")

    def test_contract_target_cleanup_and_environment_are_exact(self):
        legal = {
            "contract", "runner_contract", "profile_id", "target", "environment",
            "toolchain", "cache", "resources", "commands", "inputs", "evidence", "cleanup",
        }
        for profile in (BASE, EVIDENCE):
            self.assertEqual(set(profile), legal)
            self.assertEqual(profile["contract"], "remote-compute-profile/v1")
            self.assertEqual(profile["runner_contract"], "remote-compute-runner/v1")
            self.assertEqual(profile["target"]["ssh_target"], "build-host")
            self.assertEqual(profile["cleanup"], {"policy": "always-remove-run-and-artifacts"})
            self.assertIn("/home/example/.cargo/bin", profile["environment"]["fixed"]["PATH"])

    def test_toolchain_and_cache_are_pinned(self):
        expected_rustfmt = "rustfmt 1.9.0-stable (8bab26f4f6 2026-07-14)"
        for profile in (BASE, EVIDENCE):
            self.assertEqual(profile["toolchain"]["bootstrap_policy"], "cache-keyed")
            self.assertEqual(profile["toolchain"]["checks"][-1]["expect"]["value"], expected_rustfmt)
            self.assertEqual(profile["cache"]["mode"], "shared-keyed")
            self.assertEqual(
                profile["cache"]["env"],
                {
                    "CARGO_HOME": "{{domain}}/cargo-home",
                    "RUSTUP_HOME": "{{domain}}/rustup-home",
                    "CARGO_TARGET_DIR": "{{domain}}/cargo-target",
                },
            )
            self.assertNotIn("CARGO_BUILD_JOBS", profile["environment"]["fixed"])

    def test_profiles_share_exact_two_work_conserving_cache_carriers(self):
        expected = {
            "domain0": {
                "cpu_policy": "host-shared",
                "memory": {"policy": "runner-unbounded"},
                "concurrency": 1,
                "cache_namespace": "carrier0",
            },
            "domain1": {
                "cpu_policy": "host-shared",
                "memory": {"policy": "runner-unbounded"},
                "concurrency": 1,
                "cache_namespace": "carrier1",
            },
        }
        self.assertEqual(BASE["profile_id"], EVIDENCE["profile_id"])
        for profile in (BASE, EVIDENCE):
            self.assertEqual(profile["resources"]["domains"], expected)
            encoded = json.dumps(profile["resources"], sort_keys=True)
            self.assertNotIn("per-run", encoded)
            self.assertNotIn("cpuset", encoded)
            self.assertNotIn("numa_node", encoded)
            self.assertNotIn("limit_bytes", encoded)
            self.assertNotIn("available_memory_floor_bytes", encoded)

    def test_every_route_uses_the_two_stable_domains(self):
        for profile in (BASE, EVIDENCE):
            for name, resource_class in profile["resources"]["classes"].items():
                self.assertEqual(resource_class["domains"], expected_class_domains(name))
                self.assertNotIn("exclusive_group", resource_class)
                self.assertNotIn("admission", resource_class)
            for command in profile["commands"].values():
                self.assertIn(command["resource_class"], profile["resources"]["classes"])

    def test_domain_token_is_confined_to_domain_cache_paths(self):
        for profile in (BASE, EVIDENCE):
            self.assertNotIn("{{domain}}", json.dumps(profile["environment"]["fixed"], sort_keys=True))
            for relative_path in profile["cache"]["env"].values():
                self.assertTrue(relative_path.startswith("{{domain}}/"), relative_path)
                self.assertNotIn("..", relative_path)

    def test_two_run_ids_and_both_profiles_resolve_only_two_common_roots_and_leases(self):
        projections = [
            stable_carrier_projection(profile, run_id)
            for profile in (BASE, EVIDENCE)
            for run_id in ("run-a", "run-b")
        ]
        roots = {row["root"] for projection in projections for row in projection}
        leases = {row["lease"] for projection in projections for row in projection}
        self.assertEqual(roots, {
            "/home/example/secode-remote-compute/runs/cache/secode-rust/carrier0/linux-x86_64-rust-1.97.1-v1",
            "/home/example/secode-remote-compute/runs/cache/secode-rust/carrier1/linux-x86_64-rust-1.97.1-v1",
        })
        self.assertEqual(leases, {
            ("secode-rust-linux", "domain0"),
            ("secode-rust-linux", "domain1"),
        })

    def test_per_run_resurrection_is_red_for_the_original_regression(self):
        mutant = copy.deepcopy(BASE)
        mutant["resources"] = {
            "classes": {
                "focused": {
                    "timeout_ms": 1200000,
                    "admission": {
                        "mode": "per-run",
                        "cpu_policy": "host-shared",
                        "memory": {"policy": "runner-unbounded"},
                    },
                },
            },
        }
        with self.assertRaisesRegex(ValueError, "per-run admission resurrected"):
            stable_carrier_projection(mutant, "regression-run")

    def test_profiles_split_input_transfer_by_need(self):
        self.assertEqual(
            set(BASE["commands"]),
            {
                "smoke", "mutation-battery", "provider-mutations",
                "config-mutations", "config-ledger-mutations", "config-all-mutations",
                "runtime-mutations", "store-mutations",
                "event-mutations",
                "platform-mutations", "service-mutations",
                "release-mutations", "permission-mutations", "product-mutations",
                "pcu-mutations", "cache-usage-benchmark", "r4ca-mutations",
                "retained-compatibility-tail", "retained-compatibility-mutations",
                "retained-decision-mutations", "retained-repin-mutations", "retained-fixture-compat", "retained-completion-compat", "retained-completion-mutations",
                "retained-shell-focused", "retained-product-readiness", "retained-tui-focused",
                # Accepted additions this inventory had not caught up with:
                # 8807046b (desktop-recovery), 815da3de (dogfood-cli-convergence),
                # 1b5a3084 (desktop-history). All three route to desktop-recovery.
                "desktop-recovery", "dogfood-cli-convergence", "desktop-history",
            },
        )
        base_artifacts = BASE["inputs"]["artifacts"]
        self.assertEqual(
            [artifact["id"] for artifact in base_artifacts],
            [
                "shell-fixture-manifest-json", "shell-fixture-actions-jsonl",
                "shell-fixture-expected-waivers-json", "shell-fixture-expected-durable-jsonl",
                "shell-fixture-expected-state-json", "form-suspension-durable",
                "ratified-manifest-schema",
                "ratified-oracle-receipt-schema",
                "ratified-law-pack",
                "campaign-replay-actions",
                "campaign-replay-durable",
                "campaign-replay-state",
                "campaign-replay-waivers",
                "campaign-replay-manifest",
                "reconnect-manifest",
                "reconnect-actions",
                "reconnect-durable",
                "reconnect-state",
                "reconnect-waivers",
            ],
        )
        self.assertEqual(
            set(EVIDENCE["commands"]),
            {
                "evidence-smoke", "release-evidence", "full-gate", "workspace-test-attribution",
                "final-composition-battery", "product-mutations", "retained-correction-checks",
                "retained-completion-gate-tail",
            },
        )
        self.assertEqual(
            EVIDENCE["commands"]["evidence-smoke"]["steps"][0]["argv"],
            ["cargo", "+1.97.1", "test", "-p", "grapt-event", "--test", "evidence_root", "--", "--include-ignored"],
        )
        artifacts = EVIDENCE["inputs"]["artifacts"]
        self.assertEqual(artifacts[:13], base_artifacts[6:])
        self.assertEqual(len(artifacts), 44)
        self.assertEqual(sum(artifact["bytes"] for artifact in artifacts), 426_428_083)
        self.assertEqual({artifact["mode"] for artifact in artifacts}, {"0444"})
        encoded = json.dumps(artifacts, sort_keys=True, separators=(",", ":")).encode()
        self.assertEqual(
            hashlib.sha256(encoded).hexdigest(),
            "45a04ce09e200cddd7a6e7ac81a456cd68ceb2be11c9a1e7009c0a3997e172bf",
        )
        database = next(artifact for artifact in artifacts if artifact["id"] == "sanitized-db-2026-08-08")
        self.assertEqual(
            database,
            {
                "id": "sanitized-db-2026-08-08",
                "local_path": "rust-reconstruction-evidence/opencode-next.clone-2026-08-08.sanitized.db",
                "remote_path": "opencode-next.clone-2026-08-08.sanitized.db",
                "bytes": 426_348_544,
                "sha256": "6d4cf224e3129d73babaafd75e0f1e635e4381119f29af99f3e43da9a4536d71",
                "mode": "0444",
            },
        )
        fixtures = [artifact for artifact in artifacts if artifact["id"] != "sanitized-db-2026-08-08"]
        self.assertTrue(
            all(artifact["local_path"].startswith("secode/fixtures/rust-reconstruction/") for artifact in fixtures)
        )
        self.assertTrue(
            all(artifact["remote_path"].startswith("fixtures/rust-reconstruction/") for artifact in fixtures)
        )
        self.assertEqual(shared_input_mismatches(base_artifacts), [])
        self.assertEqual(shared_input_mismatches(artifacts), [])
        self.assertEqual(reconnect_mismatches(base_artifacts), [])
        self.assertEqual(reconnect_mismatches(artifacts), [])
        self.assertEqual(
            [artifact["id"] for artifact in artifacts[:8]],
            [row["id"] for row in SHARED_STORE_MUTATION_INPUTS],
        )
        shared_ids = {row["id"] for row in SHARED_STORE_MUTATION_INPUTS}
        staged_in_both = shared_ids | {row["id"] for row in RECONNECT_INPUTS}
        evidence_only = [artifact["id"] for artifact in artifacts if artifact["id"] not in staged_in_both]
        self.assertEqual(len(evidence_only), 31)
        self.assertNotIn("sanitized-db-2026-08-08", shared_ids)
        self.assertIn("sanitized-db-2026-08-08", evidence_only)
        empty = {artifact["id"]: artifact for artifact in fixtures if artifact["bytes"] == 0}
        self.assertEqual(set(empty), {"no-progress-durable", "shell-durable"})
        self.assertEqual(
            {artifact["sha256"] for artifact in empty.values()},
            {"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"},
        )

    def test_deleting_or_altering_any_shared_store_mutation_input_is_red_and_names_the_row(self):
        for profile_name, profile in (("secode-linux", BASE), ("secode-linux-evidence", EVIDENCE)):
            artifacts = profile["inputs"]["artifacts"]
            self.assertEqual(shared_input_mismatches(artifacts), [], profile_name)
            for row in SHARED_STORE_MUTATION_INPUTS:
                deleted = [a for a in copy.deepcopy(artifacts) if a["id"] != row["id"]]
                self.assertEqual(
                    shared_input_mismatches(deleted),
                    ["%s: absent" % row["id"]],
                    "%s: deleting %s must be red and name it" % (profile_name, row["id"]),
                )
                for field, broken in (
                    ("sha256", "0" * 64),
                    ("bytes", row["bytes"] + 1),
                    ("mode", "0644"),
                    ("remote_path", "fixtures/rust-reconstruction/elsewhere"),
                    ("local_path", "secode/fixtures/rust-reconstruction/elsewhere"),
                ):
                    altered = copy.deepcopy(artifacts)
                    target = next(a for a in altered if a["id"] == row["id"])
                    target[field] = broken
                    mismatches = shared_input_mismatches(altered)
                    self.assertEqual(len(mismatches), 1, (profile_name, row["id"], field))
                    self.assertTrue(
                        mismatches[0].startswith("%s: %s is " % (row["id"], field)),
                        "%s: altering %s.%s must name the row and field, got %r"
                        % (profile_name, row["id"], field, mismatches[0]),
                    )

    def test_both_profiles_stage_the_identical_frozen_reconnect_corpus(self):
        base_rows = {a["id"]: a for a in BASE["inputs"]["artifacts"]}
        evidence_rows = {a["id"]: a for a in EVIDENCE["inputs"]["artifacts"]}
        self.assertEqual(len(RECONNECT_INPUTS), 5)
        self.assertEqual(sum(row["bytes"] for row in RECONNECT_INPUTS), 6_289)
        for row in RECONNECT_INPUTS:
            self.assertIn(row["id"], base_rows, row["id"])
            self.assertIn(row["id"], evidence_rows, row["id"])
            self.assertEqual(base_rows[row["id"]], row, row["id"])
            self.assertEqual(evidence_rows[row["id"]], row, row["id"])
            self.assertTrue(
                row["remote_path"].startswith(
                    "fixtures/rust-reconstruction/reconnect-cursor-convergence-replay/"
                ),
                row["id"],
            )

    def test_deleting_or_altering_any_reconnect_input_is_red_and_names_the_row(self):
        for profile_name, profile in (("secode-linux", BASE), ("secode-linux-evidence", EVIDENCE)):
            artifacts = profile["inputs"]["artifacts"]
            self.assertEqual(reconnect_mismatches(artifacts), [], profile_name)
            for row in RECONNECT_INPUTS:
                deleted = [a for a in copy.deepcopy(artifacts) if a["id"] != row["id"]]
                self.assertEqual(
                    reconnect_mismatches(deleted),
                    ["%s: absent" % row["id"]],
                    "%s: deleting %s must be red and name it" % (profile_name, row["id"]),
                )
                for field, broken in (
                    ("sha256", "0" * 64),
                    ("bytes", row["bytes"] + 1),
                    ("mode", "0644"),
                    ("remote_path", "fixtures/rust-reconstruction/elsewhere"),
                    ("local_path", "secode/fixtures/rust-reconstruction/elsewhere"),
                ):
                    altered = copy.deepcopy(artifacts)
                    target = next(a for a in altered if a["id"] == row["id"])
                    target[field] = broken
                    mismatches = reconnect_mismatches(altered)
                    self.assertEqual(len(mismatches), 1, (profile_name, row["id"], field))
                    self.assertTrue(
                        mismatches[0].startswith("%s: %s is " % (row["id"], field)),
                        "%s: altering %s.%s must name the row and field, got %r"
                        % (profile_name, row["id"], field, mismatches[0]),
                    )

    def test_the_five_campaign_inputs_are_identical_in_every_store_mutation_profile(self):
        base_rows = {a["id"]: a for a in BASE["inputs"]["artifacts"]}
        evidence_rows = {a["id"]: a for a in EVIDENCE["inputs"]["artifacts"]}
        campaign = [row for row in SHARED_STORE_MUTATION_INPUTS if row["id"].startswith("campaign-replay-")]
        self.assertEqual(len(campaign), 5)
        for row in campaign:
            self.assertIn(row["id"], base_rows)
            self.assertIn(row["id"], evidence_rows)
            self.assertEqual(base_rows[row["id"]], evidence_rows[row["id"]], row["id"])
            self.assertEqual(base_rows[row["id"]], row, row["id"])

    def test_full_gate_has_exact_required_surfaces(self):
        steps = {step["id"]: step for step in EVIDENCE["commands"]["full-gate"]["steps"]}
        self.assertEqual(
            set(steps),
            {
                "scoped-fmt", "workspace-clippy", "workspace-rustdoc", "workspace-test",
                "dependency-direction", "compile-fail", "store-evidence", "event-evidence",
                "release-evidence", "ambient-clock-negative", "ambient-randomness-negative",
            },
        )
        self.assertEqual(
            steps["workspace-test"]["argv"],
            ["cargo", "+1.97.1", "test", "--workspace", "--all-features", "--no-fail-fast"],
        )
        for name in ("ambient-clock-negative", "ambient-randomness-negative"):
            self.assertEqual(steps[name]["expected_exit"], 101)
            self.assertEqual(
                steps[name]["required_logs"],
                [{"stream": "merged", "match": "includes", "value": "clippy::disallowed_methods"}],
            )
        scoped_fmt = steps["scoped-fmt"]["argv"]
        for crate in (
            "grapt-platform", "grapt-product", "grapt-api", "grapt-service", "grapt-release"
        ):
            self.assertIn(crate, scoped_fmt)
        final_fmt = EVIDENCE["commands"]["final-composition-battery"]["steps"][0]["argv"]
        self.assertEqual(final_fmt, scoped_fmt)

    def test_workspace_test_attribution_is_serialized_and_exact(self):
        route = EVIDENCE["commands"]["workspace-test-attribution"]
        self.assertEqual(route["resource_class"], "full")
        self.assertEqual(route["timeout_ms"], 7_500_000)
        self.assertEqual(route["artifacts"], [])
        self.assertEqual(
            route["steps"],
            [
                {
                    "id": "workspace-test",
                    "argv": [
                        "cargo", "+1.97.1", "test", "--workspace",
                        "--all-features", "--no-fail-fast",
                    ],
                    "timeout_ms": 7_200_000,
                }
            ],
        )

    def test_current_mutation_subset_and_final_sequence_are_exact(self):
        expected_commands = [
            "provider-mutations", "config-mutations", "config-ledger-mutations",
            "runtime-mutations", "store-mutations", "event-mutations",
            "platform-mutations", "product-mutations",
        ]
        # A runner whose own gate-time behaviour is witnessed in python runs
        # that census immediately before its battery, in every route that
        # carries the battery. Reading them from one table is what keeps the
        # route, the battery and the final sequence from drifting apart.
        censused = {
            "provider-mutations": ["provider-mutation-census", "provider-mutations"],
            "product-mutations": ["product-mutation-census", "product-mutations"],
        }
        expected_steps = []
        for command in expected_commands:
            expected_steps.extend(censused.get(command, [command]))
        mutation = [step["id"] for step in BASE["commands"]["mutation-battery"]["steps"]]
        final = [step["id"] for step in EVIDENCE["commands"]["final-composition-battery"]["steps"]]
        full = [step["id"] for step in EVIDENCE["commands"]["full-gate"]["steps"]]
        self.assertEqual(mutation, expected_steps)
        for step_id in expected_commands[:-1]:
            route = BASE["commands"][step_id]
            self.assertEqual(
                [step["id"] for step in route["steps"]], censused.get(step_id, [step_id])
            )
            self.assertEqual(route["resource_class"], "mutation")
        self.assertEqual(final, full + expected_steps)

    def test_the_config_lane_runs_each_allocation_exactly_once_per_battery(self):
        """IGC is U1a's allocation and IGL is U1b's; a battery runs each once.

        The joined `config-all-mutations` route exists so a caller can ask for
        the whole config lane in one command. The batteries carry its two STEPS
        rather than calling it, which is what keeps a run from executing IGC
        twice - once for the inherited route and once inside the joined one.
        The IGC-only route is retained beside it for attribution: a red row
        should be nameable to the allocation that owns it.

        `config-ledger-mutations` is deliberately NOT in the `censused` table
        above: its runner's gate-time behaviour is witnessed by its own
        `--self-test`, which builds nothing and is not a battery step, so there
        is no census to run immediately before it.
        """
        joined = BASE["commands"]["config-all-mutations"]
        self.assertEqual(joined["resource_class"], "mutation")
        self.assertEqual(
            [step["argv"] for step in joined["steps"]],
            [
                ["python3", "crates/grapt-config/mutations/run.py"],
                ["python3", "crates/grapt-config/mutations/igl/run.py"],
            ],
        )
        self.assertEqual(
            BASE["commands"]["config-ledger-mutations"]["steps"][0]["argv"],
            ["python3", "crates/grapt-config/mutations/igl/run.py"],
        )
        self.assertEqual(
            BASE["commands"]["config-mutations"]["steps"][0]["argv"],
            ["python3", "crates/grapt-config/mutations/run.py"],
        )

        for battery in (
            BASE["commands"]["mutation-battery"],
            EVIDENCE["commands"]["final-composition-battery"],
        ):
            argvs = [step["argv"] for step in battery["steps"]]
            self.assertEqual(
                argvs.count(["python3", "crates/grapt-config/mutations/run.py"]),
                1,
                "IGC must be run exactly once per battery",
            )
            self.assertEqual(
                argvs.count(["python3", "crates/grapt-config/mutations/igl/run.py"]),
                1,
                "IGL must be run exactly once per battery",
            )
            self.assertNotIn(
                "config-all-mutations",
                [step["id"] for step in battery["steps"]],
                "a battery carries the joined route's steps, never the route itself",
            )

    def test_service_mutations_route_is_exact_and_standalone(self):
        route = BASE["commands"]["service-mutations"]
        self.assertEqual(route["resource_class"], "mutation")
        self.assertEqual(route["timeout_ms"], 14_400_000)
        self.assertEqual(route["artifacts"], [])
        self.assertEqual(
            route["steps"],
            [
                {
                    "id": "service-mutations",
                    "argv": ["python3", "crates/grapt-service/mutations/sv/run.py"],
                    "timeout_ms": 14_400_000,
                }
            ],
        )
        self.assertNotIn(
            "service-mutations",
            [step["id"] for step in BASE["commands"]["mutation-battery"]["steps"]],
        )
        self.assertNotIn(
            "service-mutations",
            [step["id"] for step in EVIDENCE["commands"]["final-composition-battery"]["steps"]],
        )

    def test_provider_mutations_route_runs_its_census_first(self):
        route = BASE["commands"]["provider-mutations"]
        self.assertEqual(route["resource_class"], "mutation")
        self.assertEqual(route["timeout_ms"], 14_400_000)
        self.assertEqual(route["artifacts"], [])
        self.assertEqual(
            route["steps"],
            [
                {
                    "id": "provider-mutation-census",
                    "argv": [
                        "python3", "-m", "unittest",
                        "crates/grapt-provider/mutations/test_run.py",
                    ],
                    "timeout_ms": 600_000,
                },
                {
                    "id": "provider-mutations",
                    "argv": ["python3", "crates/grapt-provider/mutations/run.py"],
                    "timeout_ms": 14_400_000,
                },
            ],
        )
        # The census is worth running only because the runner it censuses can
        # no longer inherit the ambient target this very profile exports.
        runner = (REPO_ROOT / "crates/grapt-provider/mutations/run.py").read_text(encoding="utf-8")
        self.assertIn("AMBIENT_TARGET_VARS", runner)
        self.assertIn("def run_cargo(", runner)
        self.assertIn("CARGO_TARGET_DIR", BASE["cache"]["env"])

    def test_product_mutations_and_fixture_inputs_are_declared(self):
        route = BASE["commands"]["product-mutations"]
        self.assertEqual(route["resource_class"], "mutation")
        self.assertEqual(route["timeout_ms"], 14_400_000)
        self.assertEqual(route["artifacts"], [])
        self.assertEqual(
            route["steps"],
            [
                {
                    "id": "product-mutation-census",
                    "argv": [
                        "python3", "-m", "unittest",
                        "crates/grapt-product/mutations/test_run.py",
                    ],
                    "timeout_ms": 600_000,
                },
                {
                    "id": "product-mutations",
                    "argv": ["python3", "crates/grapt-product/mutations/run.py"],
                    "timeout_ms": 14_400_000,
                }
            ],
        )
        pcu = BASE["commands"]["pcu-mutations"]
        self.assertEqual(pcu["resource_class"], "mutation")
        self.assertEqual(pcu["timeout_ms"], 14_400_000)
        self.assertEqual(pcu["artifacts"], [])
        self.assertEqual(
            pcu["steps"],
            [
                {
                    "id": "product-mutation-census",
                    "argv": [
                        "python3", "-m", "unittest",
                        "crates/grapt-product/mutations/test_run.py",
                    ],
                    "timeout_ms": 600_000,
                },
                {
                    "id": "pcu-mutations",
                    "argv": [
                        "python3", "crates/grapt-product/mutations/run.py",
                        "--prefix", "PCU",
                    ],
                    "timeout_ms": 14_400_000,
                },
            ],
        )
        benchmark = BASE["commands"]["cache-usage-benchmark"]
        self.assertEqual(benchmark["resource_class"], "focused")
        self.assertEqual(benchmark["timeout_ms"], 1_200_000)
        self.assertEqual(benchmark["artifacts"], [])
        self.assertEqual(
            benchmark["steps"],
            [
                {
                    "id": "cache-usage-benchmark-build",
                    "argv": [
                        "cargo", "+1.97.1", "build", "-p", "grapt-product",
                        "--bin", "cache-usage-benchmark",
                    ],
                    "timeout_ms": 1_200_000,
                },
                {
                    "id": "cache-usage-benchmark",
                    "argv": [
                        "cargo", "+1.97.1", "run", "--quiet", "-p",
                        "grapt-product", "--bin", "cache-usage-benchmark",
                    ],
                    "timeout_ms": 1_200_000,
                },
            ],
        )
        compose = (REPO_ROOT / "crates/grapt-product/src/compose.rs").read_text(encoding="utf-8")
        self.assertIn("SECODE_FIXTURES", compose)
        self.assertIn("REMOTE_COMPUTE_ARTIFACT_ROOT", compose)
        self.assertNotIn(r"C:\workspace\repo", compose)
        self.assertIn("required frozen fixtures are unavailable", compose)
        runner = (REPO_ROOT / "scripts/run-linux.ps1").read_text(encoding="utf-8")
        self.assertIn("--project secode-rust `", runner)
        self.assertNotIn("--project secode `", runner)
        self.assertIn("'platform-mutations'", runner)
        self.assertIn("'pcu-mutations'", runner)
        self.assertIn("'cache-usage-benchmark'", runner)

    def test_the_governed_product_step_draws_every_ruled_mutation_family(self):
        """FS must be unskippable from the governed route, not merely present.

        The profiles invoke the product runner with no argument. That used to
        select IPR only, so FS01-FS04 existed, were selectable by hand, and no
        governed route ever drew them: a full gate could pass green with the
        entire fixture-staging family unexecuted. The argv is deliberately
        UNCHANGED here - the guarantee lives in the runner's no-argument
        default, so no profile edit can express "skip a family" while still
        running the step at all.
        """
        specification = importlib.util.spec_from_file_location(
            "secode_product_mutations_reachability",
            REPO_ROOT / "crates/grapt-product/mutations/run.py",
        )
        product = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(product)

        # The no-argument default covers every ruled family, in a pinned order.
        self.assertEqual(sorted(product.RULED_ORDER), sorted(product.EXACT_INVENTORIES))
        self.assertEqual(
            product.RULED_ORDER,
            ("IPR", "DGH", "RMH", "FS", "LH", "DTV", "PC", "PU", "PCU", "RDT", "SCO", "DDR"),
        )
        self.assertEqual(len(product.EXACT_INVENTORIES["IPR"]), 22)
        self.assertEqual(len(product.EXACT_INVENTORIES["DGH"]), 7)
        self.assertEqual(len(product.EXACT_INVENTORIES["RMH"]), 2)
        self.assertEqual(len(product.EXACT_INVENTORIES["FS"]), 4)
        self.assertEqual(len(product.EXACT_INVENTORIES["LH"]), 21)
        self.assertEqual(len(product.EXACT_INVENTORIES["DTV"]), 13)
        self.assertEqual(len(product.EXACT_INVENTORIES["PC"]), 5)
        self.assertEqual(len(product.EXACT_INVENTORIES["PU"]), 10)
        self.assertEqual(len(product.EXACT_INVENTORIES["PCU"]), 19)
        self.assertEqual(len(product.EXACT_INVENTORIES["RDT"]), 7)
        # 4fa2d0ce added DDR with a letter-only prefix for the whole-token parser.
        # Pinning the count keeps DDR genuinely covered, not merely named.
        self.assertEqual(len(product.EXACT_INVENTORIES["DDR"]), 10)

        # Every governed step that runs the product runner uses that form.
        no_argument = ["python3", "crates/grapt-product/mutations/run.py"]
        seen = 0
        for profile in (BASE, EVIDENCE):
            for command in profile["commands"].values():
                for step in command["steps"]:
                    if step["id"] == "product-mutations":
                        self.assertEqual(step["argv"], no_argument, step["id"])
                        seen += 1
        self.assertEqual(seen, 4)

    def test_every_evidence_test_uses_the_shared_fail_loud_seam(self):
        for relative_path in EVIDENCE_TESTS:
            body = (REPO_ROOT / relative_path).read_text(encoding="utf-8")
            self.assertIn("mod evidence;", body, relative_path)
            self.assertIn("evidence::frozen_clone()", body, relative_path)
            self.assertIn("#[ignore", body, relative_path)
            self.assertNotIn("SKIP: frozen clone", body, relative_path)
        support = (REPO_ROOT / "test-support" / "evidence.rs").read_text(encoding="utf-8")
        self.assertIn("SECODE_EVIDENCE_ROOT", support)
        self.assertIn("REMOTE_COMPUTE_ARTIFACT_ROOT", support)

    def test_fixture_callers_share_the_portable_fail_loud_seam(self):
        joined = ""
        for relative_path in FIXTURE_CALLERS:
            body = (REPO_ROOT / relative_path).read_text(encoding="utf-8")
            declared_fragments = FIXTURE_CALLER_FRAGMENTS.get(relative_path, ())
            if relative_path in FIXTURE_CALLER_FRAGMENTS:
                facade_parent = (REPO_ROOT / relative_path).parent
                direct_fragments = tuple(
                    (facade_parent / line.removeprefix('include!("').removesuffix('");'))
                    .resolve(strict=True)
                    .relative_to(REPO_ROOT.resolve(strict=True))
                    .as_posix()
                    for line in body.splitlines()
                    if line.startswith('include!("') and line.endswith('");')
                )
                self.assertEqual(
                    declared_fragments,
                    direct_fragments,
                    f"{relative_path}: declared fragments must equal direct include inventory in exact order",
                )
            for fragment_path in declared_fragments:
                body += (REPO_ROOT / fragment_path).read_text(encoding="utf-8")
            joined += body
            self.assertIn("mod fixtures;", body, relative_path)
            self.assertIn("fixtures::fixture_path", body, relative_path)
            self.assertNotIn("C:/workspace/repo/fixtures/rust-reconstruction", body, relative_path)
        session = (REPO_ROOT / "crates/grapt-session/tests/cross_family_identity.rs").read_text(encoding="utf-8")
        self.assertNotIn('"C:/workspace"', session)
        self.assertIn('dir.join("workspace")', session)
        for test_name in FORMER_LINUX_FAILURES:
            self.assertIn(f"fn {test_name}", joined + session, test_name)
        support = (REPO_ROOT / "test-support" / "fixtures.rs").read_text(encoding="utf-8")
        self.assertIn("SECODE_FIXTURES", support)
        self.assertIn("REMOTE_COMPUTE_ARTIFACT_ROOT", support)
        self.assertIn("fixtures/rust-reconstruction", support)
        self.assertIn("required frozen fixture is absent", support)

    def test_qualification_custody_uses_carrier0_and_collects_a_verified_receipt(self):
        self.assertEqual(QCU["contract"], "remote-compute-profile/v1")
        self.assertEqual(QCU["runner_contract"], "remote-compute-runner/v1")
        self.assertEqual(tuple(QCU["resources"]["domains"]), ("domain0",))
        self.assertEqual(QCU["resources"]["domains"]["domain0"]["cache_namespace"], "carrier0")
        for resource_class in QCU["resources"]["classes"].values():
            self.assertEqual(resource_class["domains"], ["domain0"])
        self.assertEqual(QCU["environment"]["fixed"]["CARGO_BUILD_JOBS"], "2")
        self.assertEqual(QCU["inputs"]["artifacts"], [])
        smoke = QCU["commands"]["qcu-smoke"]
        self.assertEqual(
            [step["id"] for step in smoke["steps"]],
            ["plr-harness-import", "custody-controls", "qcu-resurrections"],
        )
        route = QCU["commands"]["qcu-plr-one"]
        self.assertEqual(
            [step["id"] for step in route["steps"]],
            ["plr-intended-red-restored-green", "verify-and-collect-custody"],
        )
        runner, verify = (step["argv"] for step in route["steps"])
        self.assertEqual(runner[2:4], ["--id", "PLR101"])
        self.assertEqual(verify[2:4], ["--receipt", "../qualification-custody-receipt.json"])
        self.assertEqual(runner[-1], "../qualification-custody-receipt.json")
        self.assertEqual(verify[-1], "artifacts/qualification-custody-receipt.json")
        # The expected diagnostic is judged from the manifest row the runner
        # executed (its default PLR manifest), never from the receipt.
        self.assertEqual(
            verify[verify.index("--manifest") + 1],
            "scripts/qualification/plugin-linux-reap/mutation-manifest.json",
        )
        self.assertEqual(route["artifacts"], ["qcu-plr-receipt"])
        self.assertEqual(QCU["evidence"]["artifacts"], [{
            "id": "qcu-plr-receipt", "path": "artifacts/qualification-custody-receipt.json",
            "required": True, "max_bytes": 4194304,
        }])


if __name__ == "__main__":
    unittest.main()
