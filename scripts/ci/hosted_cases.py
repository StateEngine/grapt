"""Run named, existing tests without turning missing/ignored cases into passes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import threading
import xml.etree.ElementTree as ET
import hosted_manifest

_STARTUP_DEADLINE = None


def write_json(path, value):
    pending = path.with_suffix(path.suffix + ".tmp")
    pending.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    pending.replace(path)


def startup_record(stage, **fields):
    """Optional observations cannot hold up the owned process or relay its status."""
    if os.name != "nt" or __name__ != "__main__" or sys.argv[1:2] != ["phase"]:
        return
    limits = {"runtime-entered": 4096, "job-joined": 1024,
              "child-started": 2048, "child-exited": 1024}
    if stage not in limits:
        return
    try:
        # Inspect only our options, never the contained child's arguments.
        options = sys.argv[2:sys.argv.index("--phase-command")]
        output = Path(options[options.index("--output") + 1])
        if not output.is_absolute():
            return
        budget = 0.05 if _STARTUP_DEADLINE is None else min(0.05, max(0, _STARTUP_DEADLINE - time.time()))
        until = time.monotonic() + budget
        duplicate = None
        if "child_handle" in fields:
            import _winapi
            current = _winapi.GetCurrentProcess()
            duplicate = _winapi.DuplicateHandle(current, fields["child_handle"], current,
                                                0, False, _winapi.DUPLICATE_SAME_ACCESS)
            fields["child_handle"] = duplicate

        def observe():
            global _STARTUP_DEADLINE
            try:
                import datetime
                row = dict(stage=stage, pid=os.getpid(), parent_pid=os.getppid(),
                           observed_unix_ns=time.time_ns(),
                           **{key: value for key, value in fields.items() if key != "child_handle"})
                row["observed_process"] = {"status": "unavailable", "reason": "query not completed"}
                if time.monotonic() >= until:
                    return
                # Entry/PID/status survives even if a later optional query stalls.
                output.mkdir(parents=True, exist_ok=True)
                write_json(output / ("startup-" + stage + ".json"), row)
                # These are the two existing callers' receipt locations. No retry
                # or deadline of our own substitutes for an absent parent receipt.
                context = None
                row["deadline_sources"] = []
                for path in (output.parent / (output.name + ".phase.json"), output / "phase.json"):
                    if time.monotonic() >= until:
                        return
                    try:
                        with path.open("rb") as stream:
                            encoded = stream.read(8193)
                        if len(encoded) > 8192:
                            row["deadline_sources"].append({"status": "oversized"})
                            continue
                        value = json.loads(encoded.decode("utf-8-sig"))
                        deadline = datetime.datetime.fromisoformat(value["deadline_utc"].replace("Z", "+00:00")).timestamp()
                        if value.get("phase_owner_pid") in (os.getpid(), os.getppid()):
                            context = deadline
                            _STARTUP_DEADLINE = deadline
                            row["requested_owner_pid"] = value["phase_owner_pid"]
                            break
                        row["deadline_sources"].append({"status": "owner-mismatch"})
                    except (OSError, ValueError, KeyError, TypeError) as error:
                        row["deadline_sources"].append({"error_type": type(error).__name__})
                        continue
                remaining = 0 if context is None else context - time.time()
                row["query_budget"] = "parent deadline unavailable" if context is None else "remaining parent deadline"
                if remaining > 0 and time.monotonic() < until and stage in ("runtime-entered", "child-started"):
                    row["observed_process"] = startup_process(fields.get("child_handle"), until, context)
                else:
                    row["observed_process"] = {"status": "unavailable", "reason": "no remaining bounded query budget"}
                if stage == "runtime-entered":
                    row["runtime"] = {"executable": startup_path(sys.executable),
                                      "base_executable": startup_path(getattr(sys, "_base_executable", "")),
                                      "paths": startup_paths()}
                encoded = (json.dumps(row, indent=2) + "\n").encode("utf-8")
                if len(encoded) > limits[stage]:
                    row = dict(stage=stage, pid=os.getpid(), parent_pid=os.getppid(),
                               status="unavailable", reason="observation exceeds byte bound")
                    row.update({key: fields[key] for key in ("child_pid", "exit_code", "exit_code_signed", "exit_code_hex")
                                if key in fields})
                if time.monotonic() >= until or (context is not None and time.time() >= context):
                    return
                write_json(output / ("startup-" + stage + ".json"), row)
            except Exception:
                # Absence is unavailable, never a successful startup or identity.
                return
            finally:
                if duplicate is not None:
                    try:
                        _winapi.CloseHandle(duplicate)
                    except OSError:
                        pass

        # Native queries and file I/O have no cancellable timeout. Keep them off
        # the relay thread, with at most four daemon observations in this process.
        try:
            worker = threading.Thread(target=observe, daemon=True)
            worker.start()
        except Exception:
            if duplicate is not None:
                _winapi.CloseHandle(duplicate)
            raise
        remaining = max(0, until - time.monotonic())
        if _STARTUP_DEADLINE is not None:
            remaining = min(remaining, max(0, _STARTUP_DEADLINE - time.time()))
        elif stage == "child-exited":
            remaining = 0
        worker.join(remaining)
    except Exception:
        return


def startup_path(value):
    return value if len(value) <= 192 else {"status": "unavailable", "reason": "path exceeds byte projection"}


def startup_paths():
    import hashlib
    result = {name: {"present": name in os.environ, "value": startup_path(os.environ.get(name, ""))}
              for name in ("USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP")}
    for name in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV", "PATH"):
        value = os.environ.get(name)
        result[name] = {"present": value is not None}
        if value is not None:
            if len(value) <= 8192:
                result[name]["sha256"] = hashlib.sha256(value.encode("utf-8")).hexdigest()
            else:
                result[name].update(status="unavailable", reason="hash input exceeds projection bound")
    return result


def startup_process(handle, until, deadline):
    """Query only the captured process handle; never reopen a reusable PID."""
    import ctypes as C
    from ctypes import wintypes as W
    kernel = C.WinDLL("kernel32", use_last_error=True)
    advapi = C.WinDLL("advapi32", use_last_error=True)

    def call(library, name, result, arguments, *values):
        if time.monotonic() >= until or time.time() >= deadline:
            raise TimeoutError("Observation budget exhausted")
        function = getattr(library, name)
        function.restype, function.argtypes = result, arguments
        if not function(*values):
            raise C.WinError(C.get_last_error())

    if handle is None:
        current = kernel.GetCurrentProcess
        current.restype, current.argtypes = W.HANDLE, []
        handle = current()
    row = {"status": "observed"}
    try:
        size, image = W.DWORD(1024), C.create_unicode_buffer(1024)
        call(kernel, "QueryFullProcessImageNameW", W.BOOL,
             [W.HANDLE, W.DWORD, W.LPWSTR, C.POINTER(W.DWORD)], handle, 0, image, C.byref(size))
        row["image"] = startup_path(image.value)
        times = [W.FILETIME() for _ in range(4)]
        call(kernel, "GetProcessTimes", W.BOOL, [W.HANDLE] + [C.POINTER(W.FILETIME)] * 4,
             handle, *(C.byref(value) for value in times))
        row["birth_filetime"] = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
        token = W.HANDLE()
        call(advapi, "OpenProcessToken", W.BOOL, [W.HANDLE, W.DWORD, C.POINTER(W.HANDLE)],
             handle, 8, C.byref(token))
        try:
            facts = {}
            for name, kind in (("user_sid", 1), ("integrity_sid", 25), ("elevated", 20),
                               ("elevation_type", 18), ("session", 12)):
                buffer, needed = C.create_string_buffer(4096), W.DWORD()
                call(advapi, "GetTokenInformation", W.BOOL,
                     [W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.POINTER(W.DWORD)],
                     token, kind, buffer, len(buffer), C.byref(needed))
                if kind in (1, 25):
                    sid = C.cast(buffer, C.POINTER(C.c_void_p))[0]
                    text = W.LPWSTR()
                    call(advapi, "ConvertSidToStringSidW", W.BOOL,
                         [C.c_void_p, C.POINTER(W.LPWSTR)], sid, C.byref(text))
                    try:
                        facts[name] = text.value
                    finally:
                        free = kernel.LocalFree
                        free.restype, free.argtypes = C.c_void_p, [C.c_void_p]
                        free(C.cast(text, C.c_void_p))
                else:
                    facts[name] = C.cast(buffer, C.POINTER(W.DWORD))[0]
            row["token"] = facts
        finally:
            close = kernel.CloseHandle
            close.restype, close.argtypes = W.BOOL, [W.HANDLE]
            close(token)
    except Exception as error:
        row["status"] = "partial"
        row["unavailable"] = {"error_type": type(error).__name__, "winerror": getattr(error, "winerror", None)}
    return row


startup_record("runtime-entered")

# Reuse the native release worker's kernel-owned, kill-on-close tree.
owned = None
if os.name == "nt":
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "release"))
    import windows_version_worker as owned


LOGIN = "profile_security::tests::"
SECURITY = [
    ("grapt_login", LOGIN + "protected_storage_round_trips_binary_input_with_independent_plaintext_bound", "DPAPI round trip, plaintext bound and corrupt-envelope refusal"),
    ("grapt_login", LOGIN + "plaintext_input_remains_bounded_and_an_envelope_never_falls_back", "No damaged-envelope plaintext fallback"),
    ("grapt_login", LOGIN + "only_real_directories_can_be_protected", "Private directory verification and file-as-directory refusal"),
    ("grapt_login", LOGIN + "newly_created_private_record_gets_strict_user_authority", "Strict private ACL and selected-file identity"),
    ("grapt_login", "profile_security::private_read::tests::private_snapshot_refuses_exposure_links_and_overflow", "Public-read ACL, cross-thread ACL, hard-link and overflow refusals"),
    ("grapt_login", "chatgpt::tests::protection::save_and_load_keep_both_token_files_protected_on_the_atomic_path", "Protected atomic credential persistence and occupied-temp refusal"),
    ("grapt_login", "chatgpt::tests::protection::startup_migrates_legacy_tokens_before_any_provider_request", "Legacy token migration before provider dispatch"),
    ("grapt_login", "chatgpt::migration::tests::a_foreign_writable_parent_cannot_cause_a_partially_migrated_profile", "No partial credential migration through a foreign-writable parent"),
    ("grapt_sandbox", "tests::write_targets_stay_exact_and_protected_resources_cannot_be_granted", "Protected, missing and hard-link write-grant refusals"),
    ("grapt_sandbox", "tests::write_target_replacement_requires_new_approval", "Pinned Windows file/directory replacement refusal"),
    ("grapt_sandbox", "tests::malformed_requests_refuse_before_any_process_effect", "No effects from malformed or filesystem-root grants"),
    ("grapt_sandbox", "tests::native_child_preserves_exit_and_refuses_protected_writes", "Actual MXC containment, exit preservation and protected/missing-path denial"),
    ("grapt_sandbox", "tests::native_network_off_refuses_the_same_reachable_direct_ip", "Reachable network-on control and network-off access denial"),
    *[("grapt_product", "dogfood::tools::coding::tests::windows_paths::" + name, assertion) for name, assertion in [
        ("ordinary_dos_path_matches_canonical_workspace", "Ordinary DOS and canonical workspace identity"),
        ("equivalent_drive_and_unc_prefixes_keep_components", "Drive/UNC prefix component preservation"),
        ("windows_prefix_aliases_do_not_expand_authority", "Cross-root, parent, ADS and malformed-prefix refusal"),
    ]],
    ("grapt_product", "dogfood::tools::coding::tests::profile_protection::profile_files_refuse_reads_and_writes_through_public_tool_inputs", "Public tool read/write refusal preserves protected credential bytes"),
]
CREDENTIALS = [
    ("grapt_login", "chatgpt::migration::tests::a_foreign_writable_parent_cannot_cause_a_partially_migrated_profile", "No partial credential migration through a foreign-writable parent"),
    ("grapt_connections", "chatgpt::tests::a_migrated_sign_in_updates_the_managed_account_and_retains_user_configuration", "Compiled sign-in migration"),
    ("grapt_platform", "credential::renewal::tests::vaulted_credentials_keep_their_binding_on_initial_and_renewed_capabilities", "Compiled initial and renewed platform vault capabilities"),
    ("grapt_platform", "credential::connection_use::environment_tests::retained_environment_grant_ends_at_managed_disconnect_without_reading_the_value", "Compiled managed disconnect revokes retained platform authority"),
    ("grapt_platform", "profile_configuration::command_trust_tests::explicit_connection_changes_preserve_frozen_configuration_and_reject_unrelated_edits", "Compiled protected connection configuration remains pinned"),
]
PRODUCT = [("dogfood_headless", name, "Existing public Product journey assertions") for name in [
    "headless_real_process_closes_every_journey_law",
    "apply_patch::native_text_patch_edits_files_and_inspect_cannot_run_it",
    "prompt_cache::consecutive_requests_extend_the_previous_request_byte_for_byte",
    "cli_continuity::a_crashed_session_resumes_through_the_shipped_cli_and_refuses_named_mismatches",
]]
WATCHERS = [("grapt_product", "dogfood::skills_watch::tests::" + name, "Existing native watcher registration and invalidation assertions") for name in [
    "new_edited_and_recreated_skill_folders_invalidate_their_location",
    "directories_created_during_registration_remain_watched_after_setup",
]]


def output_snapshot(path):
    """Record a finite raw-byte prefix, even if failed cleanup leaves a writer."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        remaining = size = os.fstat(stream.fileno()).st_size
        while remaining:
            chunk = stream.read(min(remaining, 65536))
            if not chunk:
                raise ValueError("Test output shrank during capture")
            digest.update(chunk)
            remaining -= len(chunk)
        complete = os.fstat(stream.fileno()).st_size == size
    return {"path": path.name, "bytes": size, "sha256": digest.hexdigest(), "complete": complete}


def credential_report(output, phase_outcome):
    """Finalize partial worker evidence after it exits, even on build/phase failure."""
    evidence = output / "standard-credentials" / "results.json"
    report = {}
    evidence_error = None
    try:
        report = json.loads(evidence.read_text(encoding="utf-8-sig"))
        if not isinstance(report, dict) or not isinstance(report.get("cases"), list):
            raise ValueError("Malformed case report")
    except (OSError, ValueError) as error:
        evidence_error = type(error).__name__
        report = {}
    suite = ET.Element("testsuite", name="standard-credentials")
    table = f"Credential phase outcome: **{phase_outcome}**.\n\n"
    if evidence_error:
        table += f"Worker evidence unavailable ({evidence_error}); no case inferred as passing.\n\n"
    table += "| Exact case | Worker verdict | Evidence result |\n|---|---|---|\n"
    failures = errors = 0
    rows = report.get("cases", [])
    cases = CREDENTIALS
    captured_path = output / "build" / "manifest.json"
    selection_error = None
    try:
        captured = json.loads(captured_path.read_text(encoding="utf-8-sig"))
        if not isinstance(captured, dict):
            raise ValueError("Captured selection is not an object")
        cases = captured.get("credential_cases", CREDENTIALS)
        if "case_manifest_sha256" in report or "case_manifest_sha256" in captured:
            cases = captured["credential_cases"]
            if not isinstance(captured.get("case_manifest_sha256"), str) or not re.fullmatch(
                    r"[0-9a-f]{64}", captured["case_manifest_sha256"]):
                raise ValueError("Captured case-manifest digest is missing or malformed")
        if not isinstance(cases, list) or not cases or any(
                not isinstance(case, (list, tuple)) or len(case) != 3 or
                any(not isinstance(field, str) or not field for field in case) for case in cases):
            raise ValueError("Captured exact case selection is malformed")
        if len({(case[0], case[1]) for case in cases}) != len(cases):
            raise ValueError("Captured exact case selection contains duplicates")
        if (report.get("source") != captured.get("source") or
                report.get("case_manifest_sha256") != captured.get("case_manifest_sha256")):
            raise ValueError("Worker source or case-manifest binding differs")
    except FileNotFoundError:
        if "case_manifest_sha256" in report:
            selection_error = "Captured case selection is missing"
    except (OSError, ValueError, KeyError) as error:
        selection_error = f"Captured case selection unavailable ({type(error).__name__})"
    if selection_error:
        # Without the expected selection, retain observed failures and cleanup
        # evidence, but never infer that these rows cover the requested cases.
        cases = list(dict.fromkeys((row["binary"], row["name"], "Unverified observed case")
                     for row in rows if isinstance(row, dict) and
                     isinstance(row.get("binary"), str) and isinstance(row.get("name"), str)))
        errors += 1
        capture = ET.SubElement(suite, "testcase", classname="hosted-runner", name="captured-selection")
        ET.SubElement(capture, "error", message=selection_error).text = (
            "Expected case coverage is unknown; inspect build/manifest.json and worker results.json")
        table += f"{selection_error}; expected coverage is unknown.\n\n"
    for index, (binary, case, _) in enumerate(cases):
        matches = [row for row in rows if isinstance(row, dict) and
                   (row.get("binary"), row.get("name")) == (binary, case)]
        row = matches[0] if len(matches) == 1 else {}
        status = row.get("status", "unmeasured")
        cleanup = row.get("cleanup", {})
        if not isinstance(cleanup, dict):
            cleanup = {}
        result = ET.SubElement(suite, "testcase", classname=binary, name=case)
        problems = []
        if selection_error:
            problems.append(("error", "Observed case cannot establish requested coverage"))
            errors += 1
        if status == "fail" and row.get("completed") is True:
            problems.append(("failure", "Named test failed"))
            failures += 1
        elif status != "pass" or row.get("completed") is not True:
            problems.append(("error", "No completed passing verdict for this exact case"))
            errors += 1
        if cleanup.get("children_clean") is False or cleanup.get("survivors_after_root_exit", 0) != 0:
            problems.append(("error", "Owned process cleanup failed"))
            errors += 1
        elif cleanup.get("children_clean") is not True or "survivors_after_root_exit" not in cleanup:
            problems.append(("error", "Owned process cleanup was not measured"))
            errors += 1
        if row.get("output_complete") is False:
            problems.append(("error", "Test output capture was incomplete"))
            errors += 1
        for kind, message in problems:
            ET.SubElement(result, kind, message=message).text = (
                f"{message}; inspect standard-credentials/case-{index + 1}.log, "
                f"case-{index + 1}.stderr.log and results.json")
        detail = "; ".join(message for _, message in problems) or "pass"
        table += f"| `{case}` | {status if status in ('pass', 'fail', 'unmeasured') else 'invalid'} | {detail} |\n"
    if phase_outcome != "success":
        errors += 1
        message = "Credential phase did not succeed"
        try:
            phase_report = json.loads((output / "standard-credentials.phase.json").read_text(encoding="utf-8-sig"))
            if isinstance(phase_report, dict) and phase_report.get("timed_out") is True:
                message = "Credential phase timed out"
        except (OSError, ValueError):
            pass  # The failed step remains an error when its phase record is absent.
        table += f"\n{message}; inspect standard-credentials.phase.json.\n"
        phase = ET.SubElement(suite, "testcase", classname="hosted-runner", name="phase-completion")
        ET.SubElement(phase, "error", message=message).text = (
            "Inspect build logs, standard-credentials.phase.json, phase-error.json, "
            "standard-credentials.stderr.log and account-cleanup.json")
    suite.set("tests", str(len(cases) + int(phase_outcome != "success") + int(selection_error is not None)))
    suite.set("failures", str(failures))
    suite.set("errors", str(errors))
    # The administrator creates these siblings of the worker output. Fixtures
    # never live below this root, including when the worker is forcibly stopped.
    ET.ElementTree(suite).write(output / "credentials-junit.xml", encoding="utf-8", xml_declaration=True)
    (output / "credentials-results.md").write_text(table, encoding="utf-8")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as stream:
            stream.write(table)
    return int(bool(failures or errors))


def build(output, windows, *, credentials=False, case_manifest=None, build_targets=None):
    plan = digest = None
    if case_manifest:
        plan, digest = hosted_manifest.load(case_manifest)
    specs = [("grapt-product", "test", ["--lib"])]
    if credentials:
        specs = [(package, "test", ["--lib"]) for package in
                 ("grapt-login", "grapt-connections", "grapt-platform")]
    elif windows:
        specs = [("grapt-login", "test", ["--lib"]), ("grapt-sandbox", "test", ["--lib"]),
                 ("grapt-product", "test", ["--lib", "--test", "dogfood_headless",
                                             "--features", "loopback-provider-testkit"]),
                 ("grapt-cli", "build", ["--bin", "grapt", "--features", "loopback-testkit"])]
    manifest = {"source": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
                "builds": [], "binaries": {}, "shipped": {}, "artifacts": {}, "product_binaries": {},
                "target": os.environ.get("CARGO_BUILD_TARGET", "host"),
                "target_directory": os.environ.get("CARGO_TARGET_DIR", "target")}
    cases = CREDENTIALS if credentials else SECURITY + PRODUCT if windows else WATCHERS
    if plan:
        cases = hosted_manifest.selection(plan)
        manifest.update(case_manifest=plan, case_manifest_sha256=digest)
        specs = [(variant["package"], "test", variant) for variant in plan["variants"]]
        # Capture the executables each public consumer actually invokes.
        companions = hosted_manifest.companions(plan)
        products = set(companions)
        if any("cli" in names for names in companions.values()):
            specs.insert(0, ("grapt-cli", "build", {
                "id": "@shipped-cli", "package": "grapt-cli", "kind": "bin", "target": "grapt",
                "features": ["loopback-testkit"], "default_features": True}))
    else:
        products = set()
    targets = None
    if build_targets:
        if not plan:
            raise ValueError("Build target map requires an exact case manifest")
        targets = hosted_manifest.build_targets(build_targets,
            [selection["id"] for _, _, selection in specs],
            Path(os.environ.get("CARGO_TARGET_DIR", "target")))
        configurations = {}
        for _, verb, variant in specs:
            if verb == "test":
                directory = Path(targets[variant["id"]]).resolve()
                configuration = (variant["default_features"], tuple(sorted(variant["features"])))
                if directory in configurations and configurations[directory] != configuration:
                    raise ValueError("Different feature configurations require separate Cargo targets")
                configurations[directory] = configuration
        manifest["build_targets"] = targets
    elif plan and any(variant["target"] == "integration" and variant["id"] in products
                      for variant in plan["variants"]) and len({
            (variant["default_features"], tuple(sorted(variant["features"])))
            for variant in plan["variants"]}) > 1:
        raise ValueError("Mixed embedded-server configurations require an explicit build target map")
    if credentials:
        manifest["credential_cases"] = cases
    write_json(output / "selected-cases.json", cases)
    write_json(output / "manifest.json", manifest)
    for package, verb, selection in specs:
        variant = selection if plan else None
        command = (hosted_manifest.command(variant, verb) if variant else
                   ["cargo", verb, "--locked", "-p", package, *selection, "--message-format=json"])
        if verb == "test" and not variant:
            command.append("--no-run")
        start = time.monotonic()
        build_id = variant["id"] if variant else package
        messages = output / (build_id + ".jsonl")
        options = {"env": dict(os.environ, CARGO_TARGET_DIR=targets[build_id])} if targets else {}
        with messages.open("w", encoding="utf-8") as stdout, (output / (build_id + "-build.log")).open("w") as stderr:
            result = subprocess.run(command, stdout=stdout, stderr=stderr,
                                    timeout=1200 if credentials else 5400, **options)
        features = set()
        product = None
        with messages.open(encoding="utf-8") as stream:
            for line in stream:
                row = json.loads(line)
                if row.get("reason") == "compiler-artifact" and row.get("executable"):
                    if (build_id in products and row["target"]["name"] == "grapt-server" and
                            "bin" in row["target"]["kind"] and not row["profile"]["test"]):
                        product = row
                    if variant and (row["target"]["name"] != variant["target"] or
                                    variant["kind"] not in row["target"]["kind"] or
                                    bool(row["profile"]["test"]) != (verb == "test")):
                        continue
                    group = "binaries" if row["profile"]["test"] else "shipped"
                    binary = variant["id"] if variant else row["target"]["name"]
                    manifest[group][binary] = row["executable"]
                    manifest["artifacts"][binary] = {
                        "sha256": hosted_manifest.file_digest(Path(row["executable"])),
                        "package": package, "target": row["target"], "features": row.get("features", [])}
                    features.update(row.get("features", []))
        manifest["builds"].append({"id": build_id, "package": package, "command": command, "features": sorted(features),
                                    "seconds": time.monotonic() - start, "exit": result.returncode})
        group = "binaries" if verb == "test" else "shipped"
        if variant and variant["id"] not in manifest[group]:
            manifest["builds"][-1]["exit"] = result.returncode or 1
            manifest["builds"][-1]["error"] = "Selected test target did not produce an executable"
        if build_id == "@shipped-cli" and manifest["builds"][-1]["exit"] == 0:
            # Later CLI test variants can replace Cargo's ordinary binary.
            # Preserve the captured loopback build before invoking Cargo again.
            original = Path(manifest["shipped"][build_id])
            stage = output / "shipped" / build_id
            stage.mkdir(parents=True, exist_ok=True)
            destination = stage / original.name
            shutil.copy2(original, destination)
            manifest["shipped"][build_id] = str(destination.resolve())
        if build_id in products and manifest["builds"][-1]["exit"] == 0:
            cli = manifest["shipped"].get("@shipped-cli")
            required = companions[build_id]
            if not product or ("cli" in required and not cli):
                manifest["builds"][-1].update(exit=1, error="Compiled case omitted a required companion executable")
            else:
                stage = output / "shipped" / build_id
                stage.mkdir(parents=True, exist_ok=True)
                staged = {}
                originals = {"product": product["executable"], "cli": cli}
                for name in required:
                    original = originals[name]
                    # Cargo embeds integration's ordinary server path in the
                    # harness; bind that original rather than a staged copy.
                    embedded = name == "product" and variant["target"] == "integration"
                    destination = Path(original) if embedded else stage / Path(original).name
                    if not embedded:
                        shutil.copy2(original, destination)
                    staged[name] = str(destination.resolve())
                    staged[name + "_sha256"] = hosted_manifest.file_digest(destination)
                staged["product_features"] = product.get("features", [])
                if "cli" in required:
                    staged["cli_features"] = manifest["artifacts"]["@shipped-cli"]["features"]
                manifest["product_binaries"][build_id] = staged
        write_json(output / "manifest.json", manifest)
        if credentials and manifest["builds"][-1]["exit"]:
            break
    if windows and not credentials:
        product = Path(manifest["shipped"].get("grapt-server", ""))
        cli = Path(manifest["shipped"].get("grapt", ""))
        manifest["shipped_cli_ready"] = (product.is_file() and cli.is_file()
                                         and product.parent == cli.parent)
        write_json(output / "manifest.json", manifest)
    return int(any(row["exit"] for row in manifest["builds"]) or
               (windows and not credentials and not manifest["shipped_cli_ready"]))


def run_cases(output, manifest_path, suite, identity):
    manifest = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
    cases = {"security": SECURITY, "product": PRODUCT, "watchers": WATCHERS,
             "credentials": CREDENTIALS}[suite]
    if suite == "credentials":
        cases = manifest.get("credential_cases", cases)
    report = {"source": manifest["source"], "builds": manifest["builds"], "identity": identity,
              "suite": suite, "cases": [], "selected": len(cases)}
    if "case_manifest_sha256" in manifest:
        report["case_manifest_sha256"] = manifest["case_manifest_sha256"]
    report["artifacts"] = manifest.get("artifacts", {})
    report["product_binaries"] = manifest.get("product_binaries", {})
    companions = hosted_manifest.companions(manifest["case_manifest"]) if "case_manifest" in manifest else {}
    products = set(companions)
    identity_file = output / "identity.json"
    if identity_file.is_file():
        report["token"] = json.loads(identity_file.read_text(encoding="utf-8-sig"))
        report["token_scope"] = "Measured worker token; tests inherit it through ordinary non-impersonating process creation"
    batch_start = time.monotonic()
    for index, (binary, case, assertion) in enumerate(cases):
        row = {"binary": binary, "name": case, "assertions": assertion, "status": "unmeasured", "completed": False}
        report["cases"].append(row)
        executable = manifest["binaries"].get(binary)
        log = output / f"case-{index + 1}.log"
        stderr_log = output / f"case-{index + 1}.stderr.log"
        row["log"] = log.name
        if executable and Path(executable).is_file():
            product = manifest.get("product_binaries", {}).get(binary)
            required = companions.get(binary, ("product", "cli"))
            observed = {}
            if product:
                try:
                    for name in required:
                        path = product.get(name)
                        if not isinstance(path, str) or not Path(path).is_file():
                            break
                        observed[name] = {"path": path, "sha256": hosted_manifest.file_digest(Path(path)),
                                          "features": product.get(name + "_features", [])}
                except OSError:
                    observed = {}
            if (binary in products and not product) or (product and (
                    set(observed) != set(required) or any(
                    observed[name]["sha256"] != product.get(name + "_sha256") for name in required))):
                row["measurement_error"] = "Required companion executable missing or changed"
                write_json(output / "results.json", report)
                continue
            if product:
                row["companions"] = observed
            expected = manifest.get("artifacts", {}).get(binary, {}).get("sha256")
            if (("case_manifest" in manifest and not expected) or
                    (expected and hosted_manifest.file_digest(Path(executable)) != expected)):
                row["measurement_error"] = "Captured executable digest missing or changed"
                write_json(output / "results.json", report)
                continue
            command = [executable, case, "--exact", "--nocapture", "--test-threads=1"]
            row["command"] = command
            start = time.monotonic()
            environment = os.environ.copy()
            if product:
                environment["GRAPT_HEADLESS_PRODUCT"] = product["product"]
            try:
                with log.open("wb") as stdout, stderr_log.open("wb") as stderr:
                    child = subprocess.Popen(command, stdout=stdout, stderr=stderr, env=environment)
                    row["root_pid"] = child.pid
                    try:
                        code = child.wait(timeout=300)
                    except subprocess.TimeoutExpired:
                        if owned:
                            owned.stop_descendants(timeout=30)
                        else:
                            child.kill()
                        code = child.wait(timeout=30)
                        row["timed_out"] = True
                row["seconds"] = time.monotonic() - start
                row["exit"] = code
                text = log.read_text(encoding="utf-8", errors="replace")
                # --nocapture can put arbitrary output between the case name and
                # its verdict. The final libtest summary records completed cases.
                selected = re.search(r"^test " + re.escape(case) + r" \.\.\.", text, re.MULTILINE)
                summaries = re.findall(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;",
                                       text, re.MULTILINE)
                result = summaries[0] if selected and len(summaries) == 1 else None
                if result == ("ok", "1", "0", "0") and code == 0 and not row.get("timed_out"):
                    row["status"] = "pass"
                    row["completed"] = True
                elif result == ("FAILED", "0", "1", "0"):
                    row["status"] = "fail"
                    row["completed"] = not row.get("timed_out", False)
                elif code != 0 or row.get("timed_out"):
                    row["status"] = "fail"
                    row["measurement_error"] = "Test process did not complete one named case"
                else:
                    row["measurement_error"] = "Expected exactly one executed, non-ignored named case"
                # Persist the completed test outcome before attempting cleanup.
                write_json(output / "results.json", report)
                if owned:
                    try:
                        membership = owned.process_ids()
                        # wait() joined this root. Popen retains its Windows process
                        # handle, so a lingering job entry is that same exited root.
                        survivors = [pid for pid in membership if pid not in (os.getpid(), child.pid)]
                        row["cleanup"] = {"raw_descendants_after_root_exit": len(membership) - 1,
                                          "job_member_pids_after_root_exit": membership,
                                          "joined_root_listed": child.pid in membership,
                                          "survivor_pids": survivors,
                                          "survivors_after_root_exit": len(survivors)}
                        write_json(output / "results.json", report)
                        owned.stop_descendants(timeout=30)
                        row["cleanup"]["children_clean"] = owned.children_clean()
                    except (OSError, RuntimeError) as error:
                        row.setdefault("cleanup", {}).update(children_clean=False, error=type(error).__name__)
            finally:
                for name, path in (("stdout", log), ("stderr", stderr_log)):
                    try:
                        row[name] = output_snapshot(path)
                    except (OSError, ValueError) as error:
                        row[name] = {"path": path.name, "error": type(error).__name__}
                row["output_complete"] = all(row[name].get("complete") is True
                                             for name in ("stdout", "stderr"))
                write_json(output / "results.json", report)
        write_json(output / "results.json", report)
    report["execution_seconds"] = sum(row.get("seconds", 0) for row in report["cases"])
    report["batch_seconds"] = time.monotonic() - batch_start
    completed = sum(row["completed"] for row in report["cases"])
    report["completed_cases_per_minute"] = completed * 60 / max(report["execution_seconds"], .001)
    write_json(output / "results.json", report)
    table = "| Identity | Exact case | Result | Seconds | Cleanup |\n|---|---|---|---:|---|\n"
    for row in report["cases"]:
        cleanup = row.get("cleanup", {})
        state = ("joined; survivors=" + str(cleanup.get("survivors_after_root_exit", 0))
                 if cleanup.get("children_clean") else "unmeasured")
        if cleanup.get("children_clean") is False:
            state = "FAILED: " + cleanup.get("error", "owned cleanup incomplete")
        table += f"| {identity} | `{row['name']}` | {row['status']} | {row.get('seconds', 0):.3f} | {state} |\n"
    table += f"\nBuild time is separate: {sum(x['seconds'] for x in report['builds']):.3f}s; execution {report['execution_seconds']:.3f}s; batch including cleanup {report['batch_seconds']:.3f}s.\n"
    (output / "results.md").write_text(table, encoding="utf-8")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as stream:
            stream.write(table)
    return int(any(row["status"] != "pass" or row.get("output_complete") is False or
                   (suite == "credentials" and (row.get("cleanup", {}).get("children_clean") is not True or
                                                row.get("cleanup", {}).get("survivors_after_root_exit") != 0)) or
                   row.get("cleanup", {}).get("children_clean") is False or
                   row.get("cleanup", {}).get("survivors_after_root_exit", 0) != 0
                   for row in report["cases"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["build-windows", "build-credentials", "watchers",
                                        "security", "product", "credentials", "credential-report", "phase"])
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--case-manifest", type=Path)
    parser.add_argument("--build-targets", type=Path)
    parser.add_argument("--identity", default="macos-native")
    parser.add_argument("--phase-outcome", default="unknown")
    parser.add_argument("--phase-command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    if args.mode == "phase":
        if not owned or not args.phase_command or not Path(args.phase_command[0]).is_absolute():
            parser.error("A Windows phase requires an absolute executable in its owned process job")
        # main runs only after contain(); the same finally joins descendants.
        # Killing this phase owner closes its job and terminates the whole tree.
        with subprocess.Popen(args.phase_command, stdin=subprocess.DEVNULL) as child:
            try:
                startup_record("child-started", child_pid=child.pid, child_handle=child._handle,
                               requested_executable=startup_path(args.phase_command[0]))
                code = child.wait()
                startup_record("child-exited", child_pid=child.pid, exit_code=code,
                               exit_code_signed=code if code < 0x80000000 else code - 0x100000000,
                               exit_code_hex=f"0x{code & 0xffffffff:08x}")
                return code
            except BaseException:
                child.kill()
                # Popen.__exit__ joins and closes, as subprocess.call does.
                raise
    if args.mode == "credential-report":
        return credential_report(args.output, args.phase_outcome)
    if args.mode == "build-windows":
        return build(args.output, True)
    if args.mode == "build-credentials":
        return build(args.output, True, credentials=True, case_manifest=args.case_manifest,
                     build_targets=args.build_targets)
    if args.mode == "watchers":
        if build(args.output, False):
            return 1
        args.manifest = args.output / "manifest.json"
    if not args.manifest:
        parser.error("Windows cases require the captured build manifest")
    return run_cases(args.output, args.manifest, args.mode, args.identity)


if __name__ == "__main__":
    if owned:
        owned.contain(limit=64)
    try:
        if owned:
            startup_record("job-joined")
        raise SystemExit(main())
    finally:
        if owned:
            owned.stop_descendants(timeout=30)
