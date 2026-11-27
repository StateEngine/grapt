"""Small execution checks for the hosted measurement adapter."""
import base64
import ctypes
import hashlib
from ctypes import wintypes
import importlib.util
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch


class HostedCases(unittest.TestCase):
    def test_companion_binding_refuses_changes_and_preserves_root_cli_requirements(self):
        path = Path(__file__).with_name("hosted_cases.py")
        spec = importlib.util.spec_from_file_location("companion_binding_test", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            script, marker, server, cli = [root / name for name in
                                           ("selected.py", "executed", "server", "cli")]
            script.write_text('import sys\nfrom pathlib import Path\n'
                              f'Path({str(marker)!r}).touch()\n'
                              'print("test " + sys.argv[0] + " ... ok")\n'
                              'print("test result: ok. 1 passed; 0 failed; 0 ignored;")\n')
            for target, change, with_cli, expected in [
                ("integration", False, False, 0), ("integration", True, False, 1),
                ("product_root", False, False, 1), ("product_root", False, True, 0),
                ("dogfood_headless", False, False, 0), ("integration", "missing", False, 1),
            ]:
                with self.subTest(target=target, change=change, with_cli=with_cli):
                    marker.unlink(missing_ok=True)
                    server.write_bytes(b"captured server")
                    cli.write_bytes(b"captured CLI")
                    companions = {"product": str(server), "product_sha256":
                                  module.hosted_manifest.file_digest(server), "product_features": []}
                    if with_cli:
                        companions.update(cli=str(cli), cli_sha256=module.hosted_manifest.file_digest(cli))
                    variant = dict(id="fixture", package="grapt-product", kind="test", target=target)
                    if target == "dogfood_headless":
                        variant["companions"] = ["product"]
                    if change == "missing":
                        server.unlink()
                    elif change:
                        server.write_bytes(b"changed server")
                    manifest = root / "manifest.json"
                    manifest.write_text(json.dumps(dict(source="fixture", builds=[],
                        binaries=dict(fixture=sys.executable), case_manifest=dict(variants=[variant]),
                        artifacts=dict(fixture=dict(sha256=module.hosted_manifest.file_digest(Path(sys.executable)))),
                        product_binaries=dict(fixture=companions))))
                    with patch.object(module, "WATCHERS", [("fixture", str(script), "fixture")]), \
                            patch.object(module, "owned", None):
                        self.assertEqual(module.run_cases(root, manifest, "watchers", "fixture"), expected)
                    self.assertEqual(marker.exists(), expected == 0)
                    row = json.loads((root / "results.json").read_text())["cases"][0]
                    if expected == 0:
                        self.assertEqual(row["companions"]["product"]["sha256"], companions["product_sha256"])
                        self.assertEqual("cli" in row["companions"], with_cli)

    def test_joined_root_is_retained_in_raw_membership_without_hiding_another_member(self):
        path = Path(__file__).with_name("hosted_cases.py")
        spec = importlib.util.spec_from_file_location("joined_root_test", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        popen = subprocess.Popen
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            executable = root / "selected.py"
            executable.write_text('import sys\nprint("test " + sys.argv[0] + " ... ok")\n'
                                  'print("test result: ok. 1 passed; 0 failed; 0 ignored;")\n')
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps(dict(source="fixture", builds=[],
                                                binaries=dict(fixture=sys.executable))))
            for extra in (False, True):
                with self.subTest(extra_owned_member=extra):
                    children = []
                    def spawn(*args, **kwargs):
                        child = popen(*args, **kwargs)
                        children.append(child)
                        return child
                    def membership():
                        self.assertIsNotNone(children[0].returncode,
                                             "Membership must be classified after the root is joined")
                        return [os.getpid(), children[0].pid] + ([2147483647] if extra else [])
                    with patch.object(module.subprocess, "Popen", side_effect=spawn), \
                            patch.object(module, "CREDENTIALS", [("fixture", str(executable), "fixture")]), \
                            patch.object(module, "owned") as job:
                        job.process_ids.side_effect = membership
                        job.children_clean.return_value = True
                        job.describe_descendants.return_value = []
                        self.assertEqual(module.run_cases(root, manifest, "credentials", "fixture"), int(extra))
                        job.stop_descendants.assert_called_once()
                    row = json.loads((root / "results.json").read_text())["cases"][0]
                    self.assertEqual((row["status"], row["completed"]), ("pass", True))
                    cleanup = row["cleanup"]
                    self.assertTrue(cleanup["joined_root_listed"])
                    self.assertEqual(cleanup["job_member_pids_after_root_exit"], membership())
                    self.assertEqual(cleanup["raw_descendants_after_root_exit"], 1 + int(extra))
                    self.assertEqual(cleanup["survivor_pids"], [2147483647] if extra else [])
                    self.assertEqual(cleanup["survivors_after_root_exit"], int(extra))

    def test_raw_streams_remain_separate_and_only_stdout_can_judge_a_case(self):
        spec = importlib.util.spec_from_file_location(
            "raw_stream_test", Path(__file__).with_name("hosted_cases.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            executable = root / "selected.py"
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps(dict(source="fixture", builds=[],
                                                binaries=dict(fixture=sys.executable))))
            for channel in (1, 2):
                with self.subTest(verdict_channel=channel):
                    executable.write_text(
                        'import os, sys\n'
                        'os.write(1, b"stdout \\xff\\r\\n")\n'
                        'os.write(2, b"stderr \\xfe\\r\\n")\n'
                        f'os.write({channel}, ("test " + sys.argv[0] + " ... ok\\n" '
                        '+ "test result: ok. 1 passed; 0 failed; 0 ignored;\\n").encode())\n')
                    with patch.object(module, "WATCHERS", [("fixture", str(executable), "fixture")]), \
                            patch.object(module, "owned", None):
                        self.assertEqual(module.run_cases(root, manifest, "watchers", "fixture"),
                                         int(channel != 1))
                    row = json.loads((root / "results.json").read_text())["cases"][0]
                    self.assertEqual(row["completed"], channel == 1)
                    self.assertTrue(row["output_complete"])
                    for name, prefix in (("stdout", b"stdout \xff\r\n"),
                                         ("stderr", b"stderr \xfe\r\n")):
                        stream = row[name]
                        data = (root / stream["path"]).read_bytes()
                        self.assertTrue(data.startswith(prefix))
                        self.assertNotIn(b"stderr" if name == "stdout" else b"stdout", data)
                        self.assertEqual(stream["bytes"], len(data))
                        self.assertEqual(stream["sha256"], hashlib.sha256(data).hexdigest())
                        self.assertTrue(stream["complete"])

    def test_product_variant_keeps_the_captured_cli_after_another_variant_rebuilds_it(self):
        spec = importlib.util.spec_from_file_location(
            "product_cli_binding_test", Path(__file__).with_name("hosted_cases.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            output = root / "build"
            output.mkdir()
            cli, server, harness = [root / name for name in ("grapt", "grapt-server", "harness")]
            harness.write_bytes(b"compiled harness")
            variants = [dict(id=identity, package=package, kind="test", target=target,
                             features=[], default_features=True,
                             cases=[dict(name="selected", assertions="existing case")])
                        for identity, package, target in [
                            ("cli-default", "grapt-cli", "contracts"),
                            ("product", "grapt-product", "product_root")]]
            plan = root / "cases.json"
            plan.write_text(json.dumps(dict(schema_version=1, variants=variants)))

            def artifact(path, name, kind, testing, features):
                return dict(reason="compiler-artifact", executable=str(path),
                            target=dict(name=name, kind=[kind]),
                            profile=dict(test=testing), features=features)

            def cargo(command, *, stdout, stderr, timeout):
                if command[1] == "build":
                    cli.write_bytes(b"captured loopback CLI")
                    rows = [artifact(cli, "grapt", "bin", False, ["loopback-testkit"])]
                else:
                    target = command[command.index("--test") + 1]
                    rows = [artifact(harness, target, "test", True, [])]
                    if target == "contracts":
                        cli.write_bytes(b"later ordinary CLI")
                    else:
                        server.write_bytes(b"real server")
                        rows += [artifact(server, "grapt-server", "bin", False, [])]
                stdout.write("".join(json.dumps(row) + "\n" for row in rows))
                return subprocess.CompletedProcess(command, 0)

            with patch.object(module.subprocess, "check_output", return_value="fixture"), \
                    patch.object(module.subprocess, "run", side_effect=cargo):
                code = module.build(output, True, credentials=True, case_manifest=plan)
            if code == 0:
                captured = json.loads((output / "manifest.json").read_text())
                staged = Path(captured["product_binaries"]["product"]["cli"])
                self.assertEqual(staged.read_bytes(), b"captured loopback CLI",
                                 "A successful Product build must retain its captured CLI variant")

    def test_report_refuses_manifest_bound_results_without_the_captured_selection(self):
        path = Path(__file__).with_name("hosted_cases.py")
        spec = importlib.util.spec_from_file_location("credential_manifest_test", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            output = Path(raw)
            evidence = output / "standard-credentials"
            evidence.mkdir()
            cases = [{"binary": "fixture", "name": name, "status": status,
                      "completed": True, "cleanup": {
                          "children_clean": True, "survivors_after_root_exit": 0}}
                     for name, status in (("default", "pass"), ("extra", "fail"))]
            (evidence / "results.json").write_text(json.dumps({
                "source": "b" * 40, "case_manifest_sha256": "a" * 64,
                "selected": len(cases), "cases": cases,
            }))
            with patch.object(module, "CREDENTIALS", [("fixture", "default", "fixture")]):
                self.assertEqual(module.credential_report(output, "success"), 1,
                                 "Missing captured selection must not hide the extra failed case")

    def test_report_preserves_failed_verdict_and_unverified_cleanup(self):
        import xml.etree.ElementTree as ET
        path = Path(__file__).with_name("hosted_cases.py")
        spec = importlib.util.spec_from_file_location("credential_cleanup_test", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            output = Path(raw)
            evidence = output / "standard-credentials"
            evidence.mkdir()
            (evidence / "results.json").write_text(json.dumps({"cases": [{
                "binary": "fixture", "name": "selected", "status": "fail",
                "completed": True, "cleanup": {"children_clean": False},
            }]}))
            with patch.object(module, "CREDENTIALS", [("fixture", "selected", "fixture")]):
                self.assertEqual(module.credential_report(output, "failure"), 1)
            case = ET.parse(output / "credentials-junit.xml").find("testcase")
            self.assertIsNotNone(case.find("failure"), "The failed test verdict must survive")
            cleanup = case.find("error")
            self.assertIsNotNone(cleanup, "Unverified cleanup must also be reported")
            self.assertIn("cleanup", cleanup.get("message", "").lower())

    def test_nocapture_output_preserves_completed_verdicts_and_missing_controls(self):
        path = Path(__file__).with_name("hosted_cases.py")
        spec = importlib.util.spec_from_file_location("hosted_verdict_test", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            executable = root / "selected.py"
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"source": "fixture", "builds": [],
                                            "binaries": {"fixture": sys.executable}}))
            for verdict, counts, code, status, completed in [
                ("ok", "1 passed; 0 failed; 0 ignored;", 0, "pass", True),
                ("FAILED", "0 passed; 1 failed; 0 ignored;", 101, "fail", True),
                ("ok", "0 passed; 0 failed; 1 ignored;", 0, "unmeasured", False),
                ("ok", "0 passed; 0 failed; 0 ignored;", 0, "unmeasured", False),
                (None, None, 2, "fail", False),
            ]:
                with self.subTest(verdict=verdict, counts=counts):
                    lines = ['import sys', 'print("test " + sys.argv[0] + " ... diagnostic")']
                    if verdict:
                        lines += [f'print("{verdict}")', f'print("test result: {verdict}. {counts}")']
                    lines += [f'raise SystemExit({code})']
                    executable.write_text("\n".join(lines) + "\n")
                    with patch.object(module, "WATCHERS", [("fixture", str(executable), "fixture")]), \
                            patch.object(module, "owned", None):
                        self.assertEqual(module.run_cases(root, manifest, "watchers", "fixture"),
                                         int(status != "pass"))
                    result = json.loads((root / "results.json").read_text())["cases"][0]
                    self.assertEqual((result["status"], result["completed"]), (status, completed))

    @unittest.skipUnless(os.name == "nt", "requires native Windows job objects")
    def test_job_joins_orphans_after_root_exit_timeout_and_worker_interruption(self):
        sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "release"))
        import windows_version_worker as custody
        custody.contain(limit=64)
        self.addCleanup(custody.stop_descendants, timeout=30)
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel.WaitForSingleObject.restype = wintypes.DWORD
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        program = """
import subprocess, sys
from pathlib import Path
sys.path.insert(0,str(Path('scripts/release').resolve()))
import windows_version_worker as job
job.contain(limit=8)
orphan = "import subprocess,sys; p=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);print(p.pid,flush=True)"
root = subprocess.Popen([sys.executable,'-c',orphan],stdout=subprocess.PIPE,text=True)
pid = int(root.stdout.readline())
root.wait(timeout=5)
print(pid,flush=True)
if '--phase-payload' in sys.argv:
    import time; time.sleep(60)
mode = sys.stdin.readline().strip()
if mode == 'timeout':
    waiting = subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
    try:
        waiting.wait(timeout=.05)
        raise RuntimeError('Timeout control unexpectedly completed')
    except subprocess.TimeoutExpired:
        job.stop_descendants(timeout=5)
    waiting.wait(timeout=5)
    assert job.children_clean()
"""
        def supervision_command(payload, phase_root):
            phase_log, phase_receipt = phase_root / "stdout.log", phase_root / "phase.json"
            quote = lambda value: "'" + str(value).replace("'", "''") + "'"
            phase_command = [str(Path(__file__).with_name("hosted_cases.py").resolve()), "phase",
                             "--output", str(phase_root), "--phase-command", *payload]
            supervision = f"""$ErrorActionPreference='Stop'
. {quote(Path(__file__).with_name('windows_hosted_phase.ps1').resolve())}
$options=@{{FilePath={quote(sys.executable)};ArgumentList={quote(subprocess.list2cmdline(phase_command))};PassThru=$true
RedirectStandardOutput={quote(phase_log)};RedirectStandardError={quote(phase_root/'stderr.log')}}}
$result=Invoke-HostedPhase -Options $options -LimitMilliseconds 10000 -Output {quote(phase_root)} -Receipt {quote(phase_receipt)}
if($result.status -ne 'completed'){{exit 1}}
exit 0
"""
            command = [str(Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe"),
                       "-NoProfile", "-NonInteractive", "-EncodedCommand",
                       base64.b64encode(supervision.encode("utf-16le")).decode()]
            return command, phase_log, phase_receipt

        for mode in ("exit", "timeout", "interrupt", "phase-timeout"):
            with self.subTest(mode=mode):
                phase_output = tempfile.TemporaryDirectory() if mode == "phase-timeout" else None
                command = [sys.executable, "-c", program]
                if phase_output:
                    command, phase_log, phase_receipt = supervision_command(
                        [*command, "--phase-payload"], Path(phase_output.name))
                worker = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
                handle = owner_handle = None
                ready = queue.Queue(maxsize=1)
                def read_ready():
                    if not phase_output:
                        ready.put(worker.stdout.readline())
                        return
                    deadline = time.monotonic() + 10
                    while time.monotonic() < deadline:
                        try:
                            lines = phase_log.read_text(encoding="utf-8-sig").splitlines()
                            phase = json.loads(phase_receipt.read_text(encoding="utf-8-sig"))
                            if lines:
                                ready.put((int(lines[0]), phase["phase_owner_pid"]))
                                return
                        except (OSError, ValueError, KeyError):
                            pass
                        time.sleep(.01)
                    ready.put("")
                reader = threading.Thread(target=read_ready, daemon=True)
                reader.start()
                try:
                    observation = ready.get(timeout=10)
                    if phase_output:
                        pid, owner_pid = observation
                        owner_handle = kernel.OpenProcess(0x100000, False, owner_pid)
                        self.assertTrue(owner_handle)
                        self.assertEqual(kernel.WaitForSingleObject(owner_handle, 0), 258)
                    else:
                        pid = int(observation)
                    # Observation only: retain this handle while the contained
                    # worker still owns the living orphan; never signal a PID.
                    handle = kernel.OpenProcess(0x100000, False, pid)
                    self.assertTrue(handle)
                    self.assertEqual(kernel.WaitForSingleObject(handle, 0), 258)
                    if mode == "interrupt":
                        worker.kill()
                    elif mode != "phase-timeout":
                        worker.stdin.write(mode + "\n")
                        worker.stdin.flush()
                    code = worker.wait(timeout=20 if phase_output else 10)
                    if mode not in ("interrupt", "phase-timeout"):
                        self.assertEqual(code, 0)
                    self.assertEqual(kernel.WaitForSingleObject(handle, 10000), 0)
                    if phase_output:
                        phase = json.loads(phase_receipt.read_text(encoding="utf-8-sig"))
                        self.assertEqual(code, 1)
                        self.assertEqual((phase["status"], phase["timed_out"], phase["phase_process_joined"]),
                                         ("timed-out", True, True))
                        self.assertIsInstance(phase["exit_code"], int)
                        self.assertNotEqual(phase["exit_code"], 0)
                        self.assertEqual(phase["phase_owner_pid"], owner_pid)
                        self.assertNotEqual(owner_pid, worker.pid)
                        self.assertEqual(kernel.WaitForSingleObject(owner_handle, 10000), 0)
                        self.assertEqual(phase["phase_owner_executable"], sys.executable)
                        print("phase-timeout-control=" + json.dumps(dict(phase, descendant_pid=pid, descendant_exited=True)))
                finally:
                    if worker.poll() is None:
                        worker.kill()
                    worker.wait(timeout=10)
                    reader.join(timeout=5)
                    self.assertFalse(reader.is_alive(), "owned worker stdout did not close")
                    if handle:
                        kernel.CloseHandle(handle)
                    if owner_handle:
                        kernel.CloseHandle(owner_handle)
                    worker.stdin.close()
                    worker.stdout.close()
                    if phase_output:
                        phase_output.cleanup()
        with tempfile.TemporaryDirectory() as raw:
            command, _, receipt = supervision_command([sys.executable, "-c", "raise SystemExit(23)"], Path(raw))
            result = subprocess.run(command, capture_output=True, timeout=20)
            phase = json.loads(receipt.read_text(encoding="utf-8-sig"))
            self.assertEqual((result.returncode, phase["status"], phase["exit_code"], phase["phase_process_joined"]),
                             (1, "failed", 23, True))
            self.assertFalse(phase["timed_out"])
            print("phase-failure-control=" + json.dumps(phase))

    @unittest.skipIf(os.name == "nt", "portable executable fixture uses a shebang")
    def test_completed_failure_survives_cleanup_refusal(self):
        path = Path(__file__).with_name("hosted_cases.py")
        spec = importlib.util.spec_from_file_location("hosted_cases_test", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            executable = root / "failed-case"
            executable.write_text('#!/usr/bin/env python3\nprint("test selected ... FAILED")\n'
                                  'print("test result: FAILED. 0 passed; 1 failed; 0 ignored;")\nraise SystemExit(1)\n')
            executable.chmod(0o755)
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"source": "fixture", "builds": [], "binaries": {"fixture": str(executable)}}))
            with patch.object(module, "WATCHERS", [("fixture", "selected", "failure")]), \
                    patch.object(module, "owned") as job:
                job.process_ids.return_value = [os.getpid()]
                job.stop_descendants.side_effect = RuntimeError("fixture cleanup refusal")
                self.assertEqual(module.run_cases(root, manifest, "watchers", "fixture"), 1)
            result = json.loads((root / "results.json").read_text())["cases"][0]
            self.assertEqual(result["status"], "fail")
            self.assertTrue(result["completed"])
            self.assertFalse(result["cleanup"]["children_clean"])
            self.assertTrue(result["output_complete"])
            for name in ("stdout", "stderr"):
                captured = result[name]
                data = (root / captured["path"]).read_bytes()
                self.assertEqual(captured["bytes"], len(data))
                self.assertEqual(captured["sha256"], hashlib.sha256(data).hexdigest())


if __name__ == "__main__":
    unittest.main()
