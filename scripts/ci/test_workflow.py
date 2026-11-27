"""Exercise routing, final status, and actual nextest archive consumers."""
import importlib.util
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import xml.etree.ElementTree as ET
from unittest.mock import patch

import changes
import hosted_coverage
import frozen_fixtures
import run as runner

spec = importlib.util.spec_from_file_location("ci_status", Path(__file__).with_name("check.py"))
status = importlib.util.module_from_spec(spec)
spec.loader.exec_module(status)


class Workflow(unittest.TestCase):
    def batch_outputs(self, final_path):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)

            def git(*args):
                return subprocess.check_output(
                    ["git", "-c", "gc.auto=0", "-c", "commit.gpgsign=false", *args],
                    cwd=root).decode().strip()

            git("init", "-q", "-b", "main")
            git("config", "user.name", "CI fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers=["crates/leaf","crates/other"]\nresolver="2"\n')
            for name in ("leaf", "other"):
                crate = root / "crates" / name
                (crate / "src").mkdir(parents=True)
                (crate / "src/lib.rs").write_text("pub fn value() -> u32 { 1 }\n")
                (crate / "Cargo.toml").write_text(
                    f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n')
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root,
                           check=True, capture_output=True)
            git("add", "--", "Cargo.toml", "Cargo.lock", "crates")
            git("commit", "-qm", "fixture")
            base = git("rev-parse", "HEAD")
            git("update-ref", "refs/remotes/origin/main", base)
            git("checkout", "-qb", "batch/fixture")
            for path in ("crates/leaf/src/lib.rs", final_path):
                target = root / path
                target.parent.mkdir(parents=True, exist_ok=True)
                with target.open("a") as output:
                    output.write("\n")
                before = git("rev-parse", "HEAD")
                git("add", "--", path)
                git("commit", "-qm", "fixture change")
            event, output = root / "event.json", root / "output"
            event.write_text(json.dumps({"before": before, "after": git("rev-parse", "HEAD")}))
            with patch.object(changes, "ROOT", root), patch.dict(os.environ,
                    GITHUB_EVENT_PATH=str(event), GITHUB_OUTPUT=str(output),
                    GITHUB_EVENT_NAME="push", GITHUB_REF="refs/heads/batch/fixture"):
                changes.main()
            return base, dict(line.split("=", 1) for line in output.read_text().splitlines())


    def test_hosted_check_requires_every_hosted_job_to_succeed(self):
        jobs = {key: {"result": "success"} for key in sorted(status.EVERY_JOB["--hosted"])}
        self.assertTrue(status.accepted(jobs, mode="--hosted"))
        for key in jobs:
            for verdict in ["failure", "cancelled", "skipped"]:
                revised = {**jobs, key: {"result": verdict}}
                self.assertFalse(status.accepted(revised, mode="--hosted"), (key, verdict))
            missing = {name: job for name, job in jobs.items() if name != key}
            self.assertFalse(status.accepted(missing, mode="--hosted"), key)
    def test_ordinary_batch_gates_all_commits_as_affected_without_precheck(self):
        base, values = self.batch_outputs("crates/other/src/lib.rs")
        self.assertEqual(values["mode"], "affected")
        self.assertEqual(values["precheck"], "false")
        self.assertEqual(values["base"], base)
        self.assertEqual(values["rust"], "true")
        self.assertEqual(set(json.loads(values["packages"])),
                         {"leaf", "other", "grapt-types", "grapt-store"})

    def test_build_tooling_batch_keeps_the_full_landing_gate(self):
        base, values = self.batch_outputs("scripts/build/fixture.py")
        self.assertEqual(values["mode"], "full")
        self.assertEqual(values["precheck"], "false")
        self.assertEqual(values["base"], base)
        self.assertEqual(json.loads(values["packages"]), ["*"])

    def test_affected_helpers_leave_the_build_suite_to_the_full_gate(self):
        import gate
        for mode, expected in [
            ("affected", ["scripts/ci", "scripts/daily-use", "scripts/stateengine-native"]),
            ("full", ["scripts/ci", "scripts/build", "scripts/daily-use", "scripts/stateengine-native"]),
        ]:
            with self.subTest(mode=mode), patch.dict(os.environ,
                    GITHUB_EVENT_NAME="push", CI_NIGHTLY="false", CI_GATE_MODE=mode,
                    CI_BASE_SHA="deadbeef"), \
                    patch.object(gate, "execute") as execute:
                runner.run_helpers()
            commands = [call.args[0] for call in execute.call_args_list]
            self.assertEqual(commands[:-2], [
                [sys.executable, "-m", "unittest", "discover", "-s", directory,
                 "-p", "test_*.py", "-v"] for directory in expected
            ])
            self.assertEqual(commands[-2], [
                sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", "scripts/release"
            ])
            self.assertEqual(commands[-1], [
                sys.executable, "scripts/build/ratchet_monotonic.py", "deadbeef"
            ])

    def test_archive_consumer_fetches_registry_before_running_offline_probes(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            results = root / "ci-results"
            results.mkdir()
            (results / "archive-paths.json").write_text(json.dumps(
                {"workspace": str(root), "target": str(root / "target")}))
            (results / "test-allocation.json").write_text(json.dumps([
                {"binary": "fixture", "name": "offline_probe", "route": "hosted"}]))
            (results / "shard-plan.json").write_text(json.dumps(
                {"schema": 1, "mode": "fallback", "total": 1,
                 "expression": "all()", "shards": {}}))
            commands = []
            with patch.object(runner, "ROOT", root), \
                    patch.dict(os.environ, CI_PACKAGES='["fixture"]', CI_NIGHTLY="false"), \
                    patch.object(runner, "run", side_effect=lambda command: commands.append(command)), \
                    patch.object(sys, "argv", ["run.py", "test"]):
                runner.main()
            self.assertEqual(commands[0], ["cargo", "fetch", "--locked"])
            self.assertEqual(commands[1][:3], ["cargo", "nextest", "run"])

    def test_ordinary_guards_are_retained_on_windows_and_executed_on_linux(self):
        expected = {"ordinary_binary_ignores_the_fixture_crash_selector",
                    "lifecycle::witnesses::ordinary_binary_ignores_the_fixture_lifecycle_selectors"}
        for platform in ("windows", "linux"):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as raw:
                root = Path(raw)
                commands = []
                with patch.object(runner, "ROOT", root), \
                        patch.dict(os.environ, CI_PACKAGES='["grapt-product"]', CI_NIGHTLY="false"), \
                        patch.object(runner, "run", side_effect=lambda command: commands.append(command)), \
                        patch.object(sys, "argv", ["run.py", "doctest", "--platform", platform]):
                    runner.main()
                rows = json.loads((root / "ci-results/ordinary-allocation.json").read_text())
                self.assertEqual({row["name"] for row in rows}, expected)
                self.assertEqual({row["route"] for row in rows},
                                 {"vm" if platform == "windows" else "hosted"})
                self.assertIn("--doc", commands[0])
                self.assertEqual(len(commands), 1 if platform == "windows" else 2)
                if platform == "linux":
                    self.assertIn("ordinary_binary_ignores_the_fixture", commands[1])

    def test_real_diff_selects_direct_consumers_and_unembedded_user_prose(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root).decode().strip()
            git("init", "-q")
            git("config", "user.name", "CI fixture")
            git("config", "user.email", "ci@example.invalid")
            (root / "Cargo.toml").write_text('[workspace]\nmembers=["crates/leaf","crates/middle","crates/upper"]\nresolver="2"\n')
            for name, dependency in [("leaf", None), ("middle", "leaf"), ("upper", "middle")]:
                crate = root / "crates" / name
                (crate / "src").mkdir(parents=True)
                (crate / "src/lib.rs").write_text("pub fn value() -> u32 { 1 }\n")
                text = f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n'
                if dependency:
                    text += f'[dependencies]\n{dependency}={{path="../{dependency}"}}\n'
                (crate / "Cargo.toml").write_text(text)
            (root / "docs/user").mkdir(parents=True)
            (root / "docs/user/guide.md").write_text("Guide\n")
            (root / "docs/guide.md").write_text("Guide\n")
            (root / "notices/90-sections").mkdir(parents=True)
            (root / "notices/90-sections/component.md").write_text("Component notice\n")
            (root / "notices/90-sections/embedded.md").write_text("Embedded notice\n")
            with (root / "crates/leaf/src/lib.rs").open("a") as output:
                output.write('pub const NOTICE: &str = include_str!("../../../notices/90-sections/embedded.md");\n')
            (root / "crates/grapt-types/src").mkdir(parents=True)
            (root / "crates/grapt-types/src/id.rs").write_text("pub struct Identity;\n")
            (root / "crates/grapt-types/src/lib.rs").write_text("pub mod id;\n")
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True,
                           capture_output=True)
            git("add", "--", ".")
            git("commit", "-qm", "fixture")
            with patch.object(changes, "ROOT", root):
                for sources, expected, addition in [
                        (["crates/leaf/src/lib.rs"], ["grapt-store", "grapt-types", "leaf", "middle"], "\n"),
                        (["docs/user/guide.md"], [], "\n"),
                        (["docs/guide.md"], [], "\n"),
                        (["notices/90-sections/component.md"], [], "\n"),
                        (["notices/90-sections/embedded.md"], ["*"], "\n"),
                        (["docs/guide.md", "crates/leaf/src/lib.rs"],
                         ["grapt-store", "grapt-types", "leaf", "middle"], "\n"),
                        (["notices/90-sections/embedded.md", "crates/leaf/src/lib.rs"],
                         ["*"], "\n"),
                        (["docs/guide.md", "notices/90-sections/embedded.md",
                          "crates/leaf/src/lib.rs"], ["*"], "\n"),
                        (["crates/grapt-types/src/id.rs"], ["*"], "\n"),
                        (["crates/grapt-types/src/id.rs"], ["*"], '#[cfg(windows)]\npub struct Native;\n')]:
                    base = git("rev-parse", "HEAD")
                    for source in sources:
                        with (root / source).open("a") as output:
                            output.write(addition)
                    git("add", "--", *sources)
                    git("commit", "-qm", "fixture change")
                    event = {"pull_request": {"base": {"sha": base}, "head": {"sha": git("rev-parse", "HEAD")}}}
                    self.assertEqual(changes.scope(event, "pull_request"), expected)
                    self.assertEqual(changes.scope(event, "pull_request_target"), expected)

    def test_routing_fails_open_and_requires_explicit_skip_permission(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            event, output = root / "event.json", root / "output"
            event.write_text('{"pull_request": {"labels": [{"name": "ci:macos"}]}}')
            with patch.dict(os.environ, GITHUB_EVENT_PATH=str(event), GITHUB_EVENT_NAME="pull_request_target",
                            GITHUB_OUTPUT=str(output)):
                changes.main()
            values = dict(line.split("=", 1) for line in output.read_text().splitlines())
            self.assertEqual(json.loads(values["packages"]), ["*"])
            self.assertEqual(values["mode"], "full")
            self.assertEqual(json.loads(values["matrix"])["include"], [])
            self.assertEqual(values["windows"], "false")
            self.assertEqual(values["hosted"], "false")
            event.write_text("{")
            with patch.dict(os.environ, GITHUB_EVENT_PATH=str(event), GITHUB_EVENT_NAME="workflow_dispatch",
                            GITHUB_OUTPUT=str(output)):
                with self.assertRaises(ValueError):
                    changes.main()
        jobs = {key: {"result": "success"} for key in ["visibility", "changes", "linux", "clippy", "platform"]}
        jobs["changes"]["outputs"] = {"rust": "true", "windows": "true", "hosted": "true"}
        self.assertTrue(status.accepted(jobs))
        for key in jobs:
            for verdict in ["failure", "cancelled", "skipped"]:
                revised = {k: dict(v) for k, v in jobs.items()}
                revised[key]["result"] = verdict
                self.assertFalse(status.accepted(revised))
        for rust in ("false", "true"):
            jobs["changes"]["outputs"] = {"rust": rust, "windows": "false", "hosted": "false"}
            for key in ("clippy", "platform"):
                jobs[key]["result"] = "skipped"
            self.assertTrue(status.accepted(jobs))
            jobs["linux"]["result"] = "skipped"
            self.assertFalse(status.accepted(jobs))
            jobs["linux"]["result"] = "success"
        jobs["changes"]["outputs"]["hosted"] = "true"
        self.assertFalse(status.accepted(jobs))
        jobs["platform"]["result"] = "success"
        self.assertTrue(status.accepted(jobs))
        jobs["changes"]["outputs"]["windows"] = "true"
        jobs["changes"]["outputs"]["hosted"] = "false"
        jobs["clippy"]["result"] = "success"
        jobs["platform"]["result"] = "skipped"
        self.assertFalse(status.accepted(jobs))
        requested = {"inputs": {"windows": True, "macos": True},
                     "pull_request": {"labels": [{"name": "ci:macos"}]}}
        for kind in ("push", "pull_request", "pull_request_target"):
            self.assertEqual(changes.matrix(requested, kind)["include"], [])
        self.assertEqual(changes.matrix(requested, "schedule")["include"],
                         [{"os": "macos-15", "platform": "macos"}])
        self.assertEqual(len(changes.matrix(requested, "workflow_dispatch")["include"]), 2)

    def test_main_push_skips_only_a_commit_its_batch_already_passed(self):
        sha = "a" * 40
        runs = [{"head_sha": sha, "conclusion": "success", "path": changes.WORKFLOW,
                 "head_branch": "main", "html_url": "main-run"},
                {"head_sha": sha, "conclusion": "success", "path": ".github/workflows/other.yml",
                 "head_branch": "batch/x", "html_url": "other-workflow"},
                {"head_sha": "b" * 40, "conclusion": "success", "path": changes.WORKFLOW,
                 "head_branch": "batch/y", "html_url": "other-commit"},
                {"head_sha": sha, "conclusion": "failure", "path": changes.WORKFLOW,
                 "head_branch": "batch/z", "html_url": "red-batch"}]

        def respond(listed):
            body = io.BytesIO(json.dumps({"workflow_runs": listed}).encode())
            return patch.object(changes.urllib.request, "urlopen", return_value=body)

        env = dict(GITHUB_REF="refs/heads/main", GITHUB_SHA=sha, GITHUB_REPOSITORY="o/r", GH_TOKEN="t")
        with patch.dict(os.environ, env):
            with respond(runs):
                self.assertEqual(changes.proven_batch("push"), "")
            with respond(runs + [{"head_sha": sha, "conclusion": "success", "path": changes.WORKFLOW,
                                  "head_branch": "batch/33", "html_url": "green-batch"}]):
                self.assertEqual(changes.proven_batch("push"), "green-batch")
                self.assertEqual(changes.proven_batch("schedule"), "")
            with patch.dict(os.environ, GITHUB_REF="refs/heads/batch/34"):
                self.assertEqual(changes.proven_batch("push"), "")
        with tempfile.TemporaryDirectory() as raw:
            output, event = Path(raw) / "out", Path(raw) / "event.json"
            event.write_text("{}")
            with patch.dict(os.environ, GITHUB_EVENT_NAME="push", GITHUB_EVENT_PATH=str(event),
                            GITHUB_OUTPUT=str(output)), \
                    patch.object(changes, "proven_batch", side_effect=OSError("offline")), \
                    patch.object(changes, "scope", return_value=["*"]):
                changes.main()
            self.assertIn("proven=false", output.read_text().splitlines())
        jobs = {key: {"result": "skipped"} for key in ["linux", "clippy", "platform"]}
        jobs["visibility"] = {"result": "success"}
        jobs["changes"] = {"result": "success", "outputs": {"proven": "true"}}
        self.assertTrue(status.accepted(jobs))
        for key in ("linux", "clippy", "platform"):
            for verdict in ("success", "failure", "cancelled"):
                revised = {k: dict(v) for k, v in jobs.items()}
                revised[key]["result"] = verdict
                self.assertFalse(status.accepted(revised))
        jobs["changes"]["result"] = "failure"
        self.assertFalse(status.accepted(jobs))

    def test_windows_routes_every_withheld_case_to_a_named_owner(self):
        packages = [{"name": "grapt-login", "dependencies": []},
                    {"name": "runtime", "dependencies": [{"name": "grapt-login"}]},
                    {"name": "consumer", "dependencies": [{"name": "runtime"}]}]
        suites = {"runtime": {"package-name": "consumer", "testcases": {"case": {"ignored": False}}},
                  "sandbox": {"package-name": "grapt-sandbox", "testcases": {
                      name: {"ignored": False} for name in ["safe", *hosted_coverage.MXC]}}}
        rows = hosted_coverage.allocate({"rust-suites": suites}, packages, "windows", False)
        self.assertEqual(sum(row["route"] == "vm" for row in rows), 3)
        self.assertEqual([row["name"] for row in rows if row["route"] == "hosted"], ["safe"])
        self.assertIn("binary_id(=runtime)", hosted_coverage.expression(rows))
        rows += [{"binary": "grapt-product::bin/grapt-server", "name": "case_" + "bounded_" * 15 + str(i),
                  "route": "vm"} for i in range(1600)]
        rows += [{"binary": "grapt-product::bin/grapt-server", "name": name,
                  "route": "hosted"} for name in hosted_coverage.CODING]
        self.assertLess(len(hosted_coverage.expression(rows)), 4096)

    def test_pinned_fixture_input_overrides_inherited_paths_and_refuses_corruption(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            archive, output = root / "input.tar", root / "environment"
            pin = "a" * 40
            with tarfile.open(archive, "w") as bundle:
                for name, data in [(".source-commit", (pin + "\n").encode()), ("corpus.json", b'{"fixture":true}')]:
                    info = tarfile.TarInfo(name)
                    info.size = len(data)
                    bundle.addfile(info, io.BytesIO(data))
            config = {"commit": pin, "sha256": hashlib.sha256(archive.read_bytes()).hexdigest()}
            destination = root / "corpus"
            with patch.object(frozen_fixtures, "configuration", return_value=config), \
                    patch.dict(os.environ, SECODE_FIXTURES=str(root / "absent"), GITHUB_ENV=str(output)), \
                    patch.object(sys, "argv", ["fixtures.py", "--archive", str(archive), "--output", str(destination)]):
                frozen_fixtures.main()
            environment = dict(os.environ, SECODE_FIXTURES=str(root / "absent"))
            environment.update(line.split("=", 1) for line in output.read_text().splitlines())
            probe = 'import os,pathlib,json;assert json.loads((pathlib.Path(os.environ["SECODE_FIXTURES"])/"corpus.json").read_text())["fixture"]'
            subprocess.run([sys.executable, "-c", probe], env=environment, check=True)
            archive.write_bytes(archive.read_bytes() + b"changed")
            with self.assertRaisesRegex(ValueError, "checksum"):
                frozen_fixtures.install(archive, root / "changed", config)

    @unittest.skipUnless(shutil.which("cargo-nextest"), "requires the pinned nextest executable")
    def test_shards_execute_the_archived_build_after_the_original_target_is_removed(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "src").mkdir()
            (root / ".config").mkdir()
            (root / "Cargo.toml").write_text('[package]\nname="archive-fixture"\nversion="0.1.0"\nedition="2021"\n')
            (root / "src/lib.rs").write_text('''/// ```
/// assert_eq!(archive_fixture::value(), 1);
/// ```
pub fn value() -> u32 { 1 }
#[test] fn first() { assert_eq!(value(), 1); }
#[test] fn second() { assert_eq!(value(), 1); }
#[test] #[ignore] fn requires_evidence() { panic!("missing evidence"); }
''' + "".join(f'#[test] #[ignore] fn extra_retained_{i}() {{ panic!("retained input"); }}\n' for i in range(8)))
            shutil.copy(runner.ROOT / ".config/nextest.toml", root / ".config/nextest.toml")
            environment = dict(os.environ, CARGO_TARGET_DIR=str(root / "target"),
                               CI_PACKAGES='["archive-fixture"]', CI_NIGHTLY="false",
                               GH_TOKEN="")  # no token: the flow is offline
            subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True,
                           env=environment, capture_output=True)
            (root / "target/debug").mkdir(parents=True)
            (root / "target/debug/grapt").write_bytes(b"fixture CLI asset")
            timing = root / "scripts" / "ci"
            timing.mkdir(parents=True)
            (timing / "test-times.json").write_text(json.dumps({
                "schema": 1, "source": "fixture-run",
                "binaries": {"archive-fixture": {
                    "total": 10.0, "tests": {"first": 9.0, "second": 1.0}}}}),
                encoding="utf-8")
            with patch.object(runner, "ROOT", root), patch.dict(os.environ, environment, clear=True):
                executed = {}
                for action in ["archive", "test", "test", "doctest"]:
                    shard = 2 if action == "test" and (root / "target").exists() else 1
                    with patch.object(sys, "argv", ["run.py", action, "--shard", str(shard)]):
                        runner.main()
                    if action == "archive":
                        plan = json.loads((root / "ci-results/shard-plan.json").read_text())
                        self.assertEqual(plan["mode"], "time")
                        shutil.rmtree(root / "target")
                    elif action == "test":
                        self.assertEqual((root / "target/debug/grapt").read_bytes(), b"fixture CLI asset")
                        report = ET.parse(root / "target/nextest/gate/junit.xml")
                        executed[shard] = {case.get("name") for case in report.findall(".//testcase")
                                           if case.find("skipped") is None}
                        self.assertEqual(executed[shard],
                                         {name for _binary, name in plan["shards"][str(shard)]["expected"]})
                self.assertFalse(executed[1] & executed[2])
                self.assertEqual(executed[1] | executed[2], {"first", "second"})
            rows = json.loads((root / "ci-results/test-allocation.json").read_text())
            self.assertEqual(sum(row["route"] == "hosted" for row in rows), 2)
            self.assertEqual(sum(row["route"] == "ignored" for row in rows), 9)

    def test_the_aggregate_requires_a_successful_visibility_job(self):
        # The current five-job shape passes; the pre-visibility four-job shape
        # fails; and a visibility job that failed or was skipped fails, because
        # check cannot have judged a run whose gate decided not to publish.
        current = {key: {"result": "success"} for key in
                   ["visibility", "changes", "linux", "clippy", "platform"]}
        current["changes"]["outputs"] = {"rust": "true", "windows": "true", "hosted": "true"}
        self.assertTrue(status.accepted(current))
        for verdict in ("failure", "cancelled", "skipped"):
            revised = {k: dict(v) for k, v in current.items()}
            revised["visibility"]["result"] = verdict
            self.assertFalse(status.accepted(revised), verdict)
        without = {k: v for k, v in current.items() if k != "visibility"}
        self.assertFalse(status.accepted(without), "the old four-job shape passed")


if __name__ == "__main__":
    unittest.main()
