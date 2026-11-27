//! The E1a acceptance corpus, executed.
//!
//! **Pattern A.** The scenario root is resolved from `env!("CARGO_MANIFEST_DIR")`
//! inside the candidate tree. This validator deliberately does **not** use
//! `test-support/fixtures.rs`: that is Pattern B, whose `fixture_path`
//! precedence runs `SECODE_FIXTURES` → `REMOTE_COMPUTE_ARTIFACT_ROOT` → a
//! hard-coded absolute path and then asserts. Importing it would import a
//! cross-repository read, a Linux failure mode, and an obligation to declare
//! fixture inputs in the mutation profile — for no benefit, because this
//! scenario lives in the same repository as the crate that owns it.
//!
//! That absence is load-bearing for the mutation battery: because no witness
//! here reaches a fixture path, a missing input can never be mistaken for a
//! fired witness.
//!
//! # Closed in both directions
//!
//! A row in `actions.jsonl` with no expected receipt is refused, and an
//! expected receipt with no action row is refused. A corpus that is closed in
//! only one direction lets a case be deleted from the side nobody checks.
//!
//! # Disclosed
//!
//! `manifest.json` declares `"$schema": "../oracle-receipt-manifest.schema.json"`
//! to match its sibling Pattern-A manifests. **That file does not exist on the
//! Rust side.** The pointer dangles and nothing validates the manifest against
//! it. This validator therefore does not claim the manifest is schema-valid; it
//! asserts the surfaces it actually reads.

#[path = "support/receipt_scratch.rs"]
mod receipt_scratch;

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scenario_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("rust-reconstruction")
        .join("engined-composition-root")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

fn rows(path: &Path) -> Vec<Value> {
    read(path)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap_or_else(|error| {
                panic!(
                    "{} carries a malformed row: {error}; line was {line}",
                    path.display()
                )
            })
        })
        .collect()
}

fn text<'a>(value: &'a Value, key: &str, context: &str) -> &'a str {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{context} must carry a string {key}; got {value}"))
}

/// The two slot paths a `status` row needs, neither of which can be a committed
/// file.
///
/// `service status` has to be driven against a slot that does not exist and a
/// slot holding undecodable bytes, and both must live OUTSIDE the repository: a
/// file written under a candidate tree while a battery runs voids the run, and
/// the direct-include inventory counts every entry it finds. Committing a
/// fixture would also move the inventory counters for a file whose only purpose
/// is to be unreadable.
///
/// So an action row names a placeholder and this function resolves it. Note
/// what is NOT being normalised: the substitution is on the row's INPUT, never
/// on the output, which is still compared as parsed values with nothing
/// rewritten.
fn resolve_placeholder(token: &str) -> Option<(receipt_scratch::tmp_guard::Dir, PathBuf)> {
    let scratch = receipt_scratch::outside_temp_dir();
    let root = scratch.join("grapt-engined-corpus-slots");
    match token {
        "{absent_registration}" => {
            let path = root.join("absent");
            let _ = std::fs::remove_dir_all(&path);
            Some((scratch, path.join("no-registration-here.json")))
        }
        "{unreadable_registration}" => {
            let path = root.join("unreadable");
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path)
                .unwrap_or_else(|error| panic!("the slot directory must be creatable: {error}"));
            let file = path.join("registration.json");
            std::fs::write(&file, b"not a decodable registration record")
                .unwrap_or_else(|error| panic!("the unreadable slot must be writable: {error}"));
            Some((scratch, file))
        }
        _ => None,
    }
}

/// Run the binary exactly as the action row describes.
fn invoke(argv: &[String]) -> (i32, String, String) {
    let scratch = receipt_scratch::outside_temp_dir();
    let receipt_root = scratch.join("grapt-engined-corpus");
    let output = Command::new(env!("CARGO_BIN_EXE_grapt-engined"))
        .args(argv)
        .env("GRAPT_RECEIPT_ROOT", &receipt_root)
        .output()
        .unwrap_or_else(|error| panic!("the binary under test must be runnable: {error}"));
    (
        output.status.code().unwrap_or_else(|| {
            panic!("the binary must exit with a code rather than a signal for {argv:?}")
        }),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Every action row produces exactly the expected receipt and exit code.
#[test]
fn every_scenario_row_produces_its_expected_receipt() {
    let root = scenario_root();
    let actions = rows(&root.join("actions.jsonl"));
    let expected = rows(&root.join("expected").join("receipts.jsonl"));

    assert!(
        !actions.is_empty(),
        "the corpus must carry at least one action"
    );

    let mut expected_by_case: BTreeMap<String, &Value> = BTreeMap::new();
    for row in &expected {
        let case = text(row, "case", "an expected receipt").to_owned();
        assert!(
            expected_by_case.insert(case.clone(), row).is_none(),
            "expected receipts carry one row per case; {case} appears twice"
        );
    }

    let mut action_cases: BTreeMap<String, &Value> = BTreeMap::new();
    for row in &actions {
        let case = text(row, "case", "an action").to_owned();
        assert!(
            action_cases.insert(case.clone(), row).is_none(),
            "actions carry one row per case; {case} appears twice"
        );
    }

    // Closed in both directions, asserted before anything is executed so a
    // missing pair is reported as a corpus defect rather than as a failing run.
    let action_names: Vec<&String> = action_cases.keys().collect();
    let expected_names: Vec<&String> = expected_by_case.keys().collect();
    assert_eq!(
        action_names, expected_names,
        "every action must have an expected receipt and every expected receipt \
         must have an action"
    );

    for (case, action) in &action_cases {
        let mut slots = Vec::new();
        let argv: Vec<String> = action
            .get("argv")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("action {case} must carry an argv array"))
            .iter()
            .map(|item| {
                let raw = item
                    .as_str()
                    .unwrap_or_else(|| panic!("action {case} argv entries are strings"));
                match resolve_placeholder(raw) {
                    Some((slot, path)) => {
                        slots.push(slot);
                        path.to_string_lossy().into_owned()
                    }
                    None => raw.to_owned(),
                }
            })
            .collect();

        let (code, stdout, stderr) = invoke(&argv);
        drop(slots);
        let row = expected_by_case[case];

        let wanted_code = row
            .get("exit_code")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("expected row {case} must carry an exit_code"));
        assert_eq!(
            i64::from(code),
            wanted_code,
            "case {case}: exit code differs; stdout was {stdout}"
        );

        let lines: Vec<&str> = stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "case {case}: stdout carries exactly one JSON object; got {lines:?}"
        );
        let observed: Value = serde_json::from_str(lines[0]).unwrap_or_else(|error| {
            panic!(
                "case {case}: stdout must be one parseable JSON object: {error}; got {}",
                lines[0]
            )
        });

        let wanted = row
            .get("receipt")
            .unwrap_or_else(|| panic!("expected row {case} must carry a receipt object"));
        assert_eq!(
            &observed, wanted,
            "case {case}: the receipt differs from the committed expectation"
        );

        assert!(
            !stderr.contains('{'),
            "case {case}: prose must stay off the receipt stream; stderr was {stderr}"
        );
    }
}

/// The waivers list is non-empty on purpose, and says why.
///
/// A green run of this corpus proves argument admission, platform refusal,
/// receipt custody and the single-object receipt. Naming every capability it
/// does not exercise is what stops a green run being read as end-to-end product
/// coverage by somebody who did not author it.
#[test]
fn the_waivers_name_every_capability_this_corpus_does_not_exercise() {
    let root = scenario_root();
    let waivers: Value = serde_json::from_str(&read(&root.join("expected").join("waivers.json")))
        .unwrap_or_else(|error| panic!("waivers.json must parse: {error}"));

    let listed = waivers
        .get("waivers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("waivers.json must carry a waivers array"));
    assert!(
        !listed.is_empty(),
        "the waivers list is deliberately NON-EMPTY; an empty one here would let a \
         green run read as end-to-end coverage"
    );

    // The capabilities the execution packet names explicitly.
    let required = [
        "session",
        "prompt-admission",
        "model-target",
        "provider",
        "tool",
        "credential",
        "permission",
        "form",
        "subagent",
        "checkpoint",
        "product-http-route",
    ];
    let named: Vec<&str> = listed
        .iter()
        .map(|entry| text(entry, "capability", "a waiver entry"))
        .collect();
    for capability in required {
        assert!(
            named.contains(&capability),
            "the waivers must name {capability}; got {named:?}"
        );
    }
    for entry in listed {
        let reason = text(entry, "not_exercised", "a waiver entry");
        assert!(
            reason.len() > 20,
            "each waiver states what is not exercised rather than only naming it; got {reason:?}"
        );
    }
}

/// The manifest declares the surfaces this validator actually reads.
#[test]
fn the_manifest_declares_the_surfaces_that_exist() {
    let root = scenario_root();
    let manifest: Value = serde_json::from_str(&read(&root.join("manifest.json")))
        .unwrap_or_else(|error| panic!("manifest.json must parse: {error}"));

    assert_eq!(
        manifest.get("id").and_then(Value::as_str),
        Some("engined-composition-root"),
        "the manifest names its own scenario"
    );

    let surfaces = manifest
        .get("surfaces")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the manifest must declare its surfaces"));
    assert!(
        !surfaces.is_empty(),
        "a scenario with no declared surface asserts nothing"
    );

    for surface in surfaces {
        let relative = surface
            .as_str()
            .unwrap_or_else(|| panic!("surfaces are strings; got {surface}"));
        let path = root.join(relative);
        assert!(
            path.is_file(),
            "declared surface {relative} must exist at {}",
            path.display()
        );
    }

    // The owner names the target that executes this corpus, so a scenario whose
    // owner is not a real test function in the named target is refused.
    let owner = manifest
        .get("owner")
        .unwrap_or_else(|| panic!("the manifest must name its owner"));
    assert_eq!(
        owner.get("crate").and_then(Value::as_str),
        Some("grapt-engined")
    );
    assert_eq!(
        owner.get("test_target").and_then(Value::as_str),
        Some("engined_acceptance_corpus"),
        "the owner names THIS target"
    );
}
