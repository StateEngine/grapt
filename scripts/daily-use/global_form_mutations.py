"""Focused DUB form and timing mutants in an exclusively owned author tree."""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCHEMA_UPGRADE = """            let tx = store.governed_transaction()?;
            tx.execute_batch(
                "CREATE TABLE form_request_global_upgrade (
                    id TEXT PRIMARY KEY, session_id TEXT, title TEXT NOT NULL,
                    status TEXT NOT NULL, suspended_reason TEXT, answer TEXT,
                    global_binding TEXT,
                    CHECK ((session_id IS NOT NULL AND global_binding IS NULL)
                        OR (session_id IS NULL AND global_binding IS NOT NULL))
                );
                INSERT INTO form_request_global_upgrade
                    (id, session_id, title, status, suspended_reason, answer)
                    SELECT id, session_id, title, status, suspended_reason, answer FROM form_request;
                DROP TABLE form_request;
                ALTER TABLE form_request_global_upgrade RENAME TO form_request;"
            )?;
            tx.commit()?.discard();"""

ROWS = [
    ("DUB-SW01", "crates/grapt-form/src/service.rs",
     SCHEMA_UPGRADE, SCHEMA_UPGRADE.replace("store.governed_transaction()", "store.connection().unchecked_transaction()").replace("tx.commit()?.discard();", "tx.commit()?;"),
     "grapt-store", ["--test", "write_chokepoint"], "only_the_store_may_open_a_transaction"),
    ("DUB-PR04", "crates/grapt-product/src/compose.rs",
     "let secondary_credentials = parts.take_secondary_credentials();",
     "let secondary_credentials = { let _ = parts.take_secondary_credentials(); Vec::new() };",
     "grapt-product", ["--features", "loopback-provider-testkit", "--test", "dogfood_headless"],
     "selected_secondary_account_crosses_production_composition_and_restart"),
    ("DUB-PR05", "crates/grapt-product/src/dogfood/execution.rs",
     "                targets.protocol_family(),", "                self.targets.0.protocol_family(),",
     "grapt-product", ["--features", "loopback-provider-testkit", "--test", "dogfood_headless"],
     "selected_secondary_account_crosses_production_composition_and_restart"),
    ("DUB-PR06", "crates/grapt-product/src/dogfood/execution.rs",
     ".get(first_target.provider())", ".get(&self.configured_provider)",
     "grapt-product", ["--features", "loopback-provider-testkit", "--test", "dogfood_headless"],
     "selected_secondary_account_crosses_production_composition_and_restart"),
    ("DUB-PR01", "crates/grapt-product/src/dogfood/execution/providers.rs",
     "for (provider, credential) in secondary {", "for (provider, credential) in secondary.into_iter().take(0) {",
     "grapt-product", ["--lib"],
     "configured_secondary_opener_binds_its_own_route_and_credential"),
    ("DUB-PR02", "crates/grapt-product/src/dogfood/production.rs",
     "ProtocolFamily::OpenAiCompatible => {", "ProtocolFamily::OpenAiCompatible if false => {",
     "grapt-product", ["--lib"],
     "compatible_selection_builds_chat_messages_and_reissues_on_retry"),
    ("DUB-PR03", "crates/grapt-provider/src/routes/body.rs",
     '"tool_call_id":call.as_str()', '"tool_call_id":"different-call"',
     "grapt-provider", ["--lib"],
     "compatible_history_preserves_call_identity_arguments_and_output"),
    ("DUB-GF10", "crates/grapt-api/src/u3a/sse.rs",
     "    location: ScopedLocation<'a>,", "    #[serde(skip)]\n    location: ScopedLocation<'a>,",
     "grapt-api", ["--lib"],
     "scoped_events_do_not_fill_another_locations_queue"),
    ("DUB-GF09", "crates/grapt-api/src/u3a/routes_location_headers.rs",
     "if directory.is_absent() {", "if false && directory.is_absent() {",
     "grapt-api", ["--lib"],
     "global_form_routes_consume_retained_headers_with_query_precedence"),
    ("DUB-GF01", "crates/grapt-form/src/service.rs",
     "if expected != Some(&decoded) {", "if false {",
     "grapt-form", ["--test", "integration"],
     "global_forms::wrong_binding_and_invalid_answer_leave_the_global_form_pending"),
    ("DUB-GF02", "crates/grapt-product/src/dogfood/mcp/call.rs",
     "() = held.retired() =>", "() = std::future::pending::<()>() =>",
     "grapt-product", ["--lib"],
     "registry_call_consumes_the_form_reply_once_and_retirement_cannot_resume_it"),
    ("DUB-GF03", "crates/grapt-product/src/dogfood/mcp/channel.rs",
     "let limit = self.bounds.max_frame_bytes();",
     "self.partial_frame.clear();\n        let limit = self.bounds.max_frame_bytes();",
     "grapt-product", ["--lib"],
     "a_cancelled_partial_read_preserves_every_byte_for_the_next_read"),
    ("DUB-GF04", "crates/grapt-product/src/dogfood/elicitation.rs",
     "if let Err(error) = self.suspend() {",
     "if let Err(error) = Ok::<(), String>(()) {",
     "grapt-product", ["--lib"],
     "dropping_the_live_call_suspends_without_inventing_a_user_cancel"),
    ("DUB-GF05", "crates/grapt-form/src/global.rs",
     "if pending >= 128 {", "if false {",
     "grapt-form", ["--test", "integration"],
     "global_forms::global_capacity_refuses_the_next_form_and_recovers_after_settlement"),
    ("DUB-GF06", "crates/grapt-product/src/dogfood/elicitation.rs",
     "if record.state == FormState::Pending {", "if false {",
     "grapt-product", ["--lib"],
     "startup_suspends_an_orphaned_durable_form_without_recreating_its_wait"),
    ("DUB-GF07", "crates/grapt-api/src/u3a/sse.rs",
     "if !interested(&subscriber.interest, location) {", "if false {",
     "grapt-api", ["--lib"],
     "scoped_events_do_not_fill_another_locations_queue"),
    ("DUB-GF08", "crates/grapt-product/src/dogfood/form.rs",
     "let state = match state {",
     'if matches!(state, FormState::Answered(_)) { return decode(json!({"data":{"status":"answered","answer":{}}})).map_err(|error| FormLookupApplicationError::InvalidRequest(invalid(error))); }\n    let state = match state {',
     "grapt-product", ["--lib"],
     "application_forms_validate_location_and_resume_the_originating_request"),
    ("DUB-LT01", "crates/grapt-provider/src/transport/adapter.rs",
     "observer.dispatched(&attempt);", "let _ = (observer, attempt);",
     "grapt-provider", ["--lib"],
     "the_wire_arm_sends_no_byte_until_the_reader_is_first_polled"),
    ("DUB-LT02", "crates/grapt-provider/src/transport/dispatch.rs",
     "if self.sender.try_send(sample).is_err() {",
     "if self.sender.try_send(sample).is_err() && false {",
     "grapt-provider", ["--lib"],
     "a_saturated_diagnostic_reports_loss_and_preserves_zero_time"),
    ("DUB-LT03", "crates/grapt-provider/src/transport/adapter.rs",
     ".map(|observer| (observer.clone(), input.attempt.clone())),",
     ".map(|observer| { observer.dispatched(&input.attempt); (observer.clone(), input.attempt.clone()) }),",
     "grapt-provider", ["--lib"],
     "the_wire_arm_sends_no_byte_until_the_reader_is_first_polled"),
]


def run(path, crate, target, witness):
    command = ["cargo", "test", "-p", crate, *target, witness,
               "--no-fail-fast", "-j", "2"]
    with path.open("wb") as output:
        code = subprocess.run(command, cwd=ROOT, stdout=output,
                              stderr=subprocess.STDOUT, timeout=180).returncode
    return code, path.read_text(encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--only", choices=[row[0] for row in ROWS])
    args = parser.parse_args()
    rows = [row for row in ROWS if args.only is None or row[0] == args.only]
    args.output.mkdir(parents=True, exist_ok=False)
    originals = {path: (ROOT / path).read_bytes() for _, path, *_ in rows}
    sources = {path: data.decode("utf-8").replace("\r\n", "\n")
               for path, data in originals.items()}
    for name, path, anchor, *_ in rows:
        if sources[path].count(anchor) != 1:
            raise RuntimeError(f"{name}: expected exactly one anchor")
    receipt = {"source_sha256": {path: hashlib.sha256(data).hexdigest()
                                 for path, data in originals.items()}, "rows": []}
    for name, path, anchor, replacement, crate, target, witness in rows:
        source = ROOT / path
        try:
            source.write_bytes(sources[path].replace(anchor, replacement).encode("utf-8"))
            code, log = run(args.output / f"{name}-red.log", crate, target, witness)
            if code != 101 or f"{witness} ... FAILED" not in log:
                raise RuntimeError(f"{name}: named witness was not red; exit={code}")
        finally:
            source.write_bytes(originals[path])
            if source.read_bytes() != originals[path]:
                raise RuntimeError(f"{name}: byte restoration failed")
        code, log = run(args.output / f"{name}-green.log", crate, target, witness)
        if code != 0 or "1 passed; 0 failed" not in log:
            raise RuntimeError(f"{name}: restored witness did not pass")
        receipt["rows"].append({"id": name, "witness": witness, "red": True, "restored_green": True})
        print(f"{name}: named RED, restored GREEN", flush=True)
    (args.output / "receipt.json").write_text(json.dumps(receipt, indent=2)+"\n", encoding="utf-8")


if __name__ == "__main__":
    main()
