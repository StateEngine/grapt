//! The executable owner of the `r4-desktop-attachment` corpus.
//!
//! It replays `actions.jsonl` through the real public API and **produces**
//! `expected/durable.jsonl` and `expected/state.json` from the model, then
//! compares both exhaustively — every durable row and every projection key.
//! Nothing in `expected/*` is hand-projected; if the model changes, this
//! comparison is what goes red.
//!
//! Missing evidence fails loudly. A corpus file that is absent is a panic, not
//! a skipped case: verification code may never self-skip.
//!
//! What this proves and what it does not: it proves the receipt model and its
//! reconstruction from an ordered durable representation. It does **not** prove
//! filesystem persistence and it does not assert that any service was running.
//! Nothing here opens a socket, starts an engine or reads a clock of its own.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use grapt_service::{
    AdmittedDifferential, AttachmentRefusal, EngineSelection, ProbeOutcome, SelectionReceipt,
    SelectorState,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceSlotId,
};
use serde_json::{json, Map, Value};

// NOTE ON `$schema` — it is DELIBERATELY not pinned anywhere in this file.
//
// It was, and that is precisely how the R1 defect survived: this owner pinned
// the same wrong literal the manifest declared and asserted they were equal, so
// the two agreed with each other while both diverged from the ratified
// contract. Restating a schema's own rule inside the thing it governs cannot
// detect that. `tests/schema_conformance.rs` now validates this manifest against
// the ACTUAL ratified schema file resolved through the frozen-fixture seam, and
// that file is the only oracle for this field.
//
// Written as a plain comment, not a doc comment: it documents an absence, and as
// `///` it bound itself to whichever constant happened to follow, which was
// `CORPUS_ID` — an unrelated item it appeared to describe.

const CORPUS_ID: &str = "r4-desktop-attachment";
const LAW_PACK_REVISION: u64 = 9;
const LAW_PACK_SOURCES: usize = 9;
const REQUIRED_WAIVER: &str = "no-typescript-oracle-for-engine-selection-receipt";

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-desktop-attachment")
}

/// Read a required corpus file, or fail loudly naming it.
fn read_required(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) => panic!(
            "the corpus file {} is required and could not be read: {error}",
            path.display()
        ),
    }
}

fn parse_json(body: &str, what: &str) -> Value {
    match serde_json::from_str(body) {
        Ok(value) => value,
        Err(error) => panic!("{what} is not valid JSON: {error}"),
    }
}

fn require_str<'a>(value: &'a Value, key: &str, what: &str) -> &'a str {
    match value.get(key).and_then(Value::as_str) {
        Some(text) => text,
        None => panic!("{what}: `{key}` is missing or not a string"),
    }
}

/// The injected clock. Every `selected_at` in the corpus arrives through one of
/// these with the exact millisecond value the action row names.
#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

fn owner_from(value: &Value, what: &str) -> OwnerReceipt {
    let slot = require_str(value, "slot", what);
    let instance = require_str(value, "instance", what);
    let environment = require_str(value, "environment", what);
    let endpoint = require_str(value, "endpoint", what);
    let generation = match value.get("generation").and_then(Value::as_u64) {
        Some(generation) => generation,
        None => panic!("{what}: `generation` is missing or not an integer"),
    };
    let pid = match value.get("pid").and_then(Value::as_u64) {
        Some(pid) => pid,
        None => panic!("{what}: `pid` is missing or not an integer"),
    };

    let parts = (
        ServiceSlotId::new(slot),
        ServiceInstanceId::new(instance),
        EnvironmentReceipt::new(environment),
        u32::try_from(pid).ok().and_then(ProcessId::new),
        Endpoint::new(endpoint),
    );
    match parts {
        (Some(slot), Some(instance), Some(environment), Some(pid), Some(endpoint)) => {
            OwnerReceipt::new(
                slot,
                instance,
                environment,
                ServiceGeneration::new(generation),
                pid,
                endpoint,
            )
        }
        _ => panic!("{what}: one of slot/instance/environment/pid/endpoint is unrepresentable"),
    }
}

/// One durable row, exactly as it is recorded.
fn durable_row(ordinal: u64, scenario: &str, receipt: &SelectionReceipt) -> Value {
    let owner = receipt.owner();
    json!({
        "ordinal": ordinal,
        "scenario": scenario,
        "selection": receipt.selection().label(),
        "slot": owner.slot().as_str(),
        "instance": owner.instance().as_str(),
        "environment": owner.environment().as_str(),
        "generation": owner.generation().value(),
        "pid": owner.pid().value(),
        // Read through the owner. There is exactly one carrier for the endpoint,
        // so a row cannot disagree with itself about where it attached.
        "endpoint": receipt.endpoint().as_str(),
        "selected_at_millis": receipt.selected_at().as_millis(),
    })
}

#[derive(Default)]
struct ScenarioState {
    admitted: Vec<SelectionReceipt>,
    selector: Option<SelectorState>,
}

struct Replay {
    durable: Vec<Value>,
    refusals: Vec<Value>,
    confirmations: Vec<Value>,
    differentials: Vec<Value>,
    selector_state: BTreeMap<String, String>,
    engines: BTreeMap<String, u64>,
}

/// Replay every action through the real API. This is the model; the expected
/// surfaces below are its output, not a parallel description of it.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn replay(actions: &str) -> Replay {
    let mut scenarios: BTreeMap<String, ScenarioState> = BTreeMap::new();
    let mut durable = Vec::new();
    let mut refusals = Vec::new();
    let mut confirmations = Vec::new();
    let mut differentials = Vec::new();
    let mut engines: BTreeMap<String, u64> = BTreeMap::new();

    for (index, line) in actions.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let what = format!("actions line {}", index + 1);
        let row = parse_json(line, &what);
        let ordinal = match row.get("ordinal").and_then(Value::as_u64) {
            Some(ordinal) => ordinal,
            None => panic!("{what}: `ordinal` is missing or not an integer"),
        };
        let scenario = require_str(&row, "scenario", &what).to_owned();
        let action = require_str(&row, "action", &what);
        let state = scenarios.entry(scenario.clone()).or_default();

        match action {
            "admit" => {
                let selection =
                    match EngineSelection::from_label(require_str(&row, "selection", &what)) {
                        Some(selection) => selection,
                        None => panic!("{what}: `selection` does not name an admitted engine"),
                    };
                let clock = match row.get("clock_millis").and_then(Value::as_i64) {
                    Some(millis) => FixedClock(millis),
                    None => panic!("{what}: `clock_millis` is missing or not an integer"),
                };
                let probe = match row.get("probe") {
                    Some(probe) => probe,
                    None => panic!("{what}: `probe` is missing"),
                };
                let outcome = match require_str(probe, "kind", &what) {
                    "healthy-unattributed" => ProbeOutcome::HealthyUnattributed,
                    "attributed" => {
                        let owner = match probe.get("owner") {
                            Some(owner) => owner,
                            None => panic!("{what}: an attributed probe must carry `owner`"),
                        };
                        ProbeOutcome::Attributed(owner_from(owner, &what))
                    }
                    other => panic!("{what}: unknown probe kind {other:?}"),
                };

                match SelectionReceipt::admit(selection, outcome, &clock) {
                    Ok(receipt) => {
                        durable.push(durable_row(ordinal, &scenario, &receipt));
                        *engines.entry(selection.label().to_owned()).or_default() += 1;
                        state.admitted.push(receipt.clone());
                        state.selector = Some(SelectorState::attach(receipt));
                    }
                    Err(refusal) => refusals.push(json!({
                        "ordinal": ordinal,
                        "scenario": scenario,
                        "action": "admit",
                        "refusal": refusal.label(),
                    })),
                }
            }
            "still-current" => {
                let against = match row.get("against") {
                    Some(against) => owner_from(against, &what),
                    None => panic!("{what}: `against` is missing"),
                };
                let recorded = match state.admitted.last() {
                    Some(receipt) => receipt,
                    None => panic!("{what}: this scenario has admitted no receipt to check"),
                };
                match recorded.still_current(&against) {
                    Ok(()) => confirmations.push(json!({
                        "ordinal": ordinal,
                        "scenario": scenario,
                        "outcome": "still-current",
                    })),
                    Err(refusal) => refusals.push(json!({
                        "ordinal": ordinal,
                        "scenario": scenario,
                        "action": "still-current",
                        "refusal": refusal.label(),
                    })),
                }
            }
            "differential" => {
                let (left, right) = match state.admitted.split_first() {
                    Some((left, rest)) => match rest.first() {
                        Some(right) => (left.clone(), right.clone()),
                        None => panic!("{what}: a differential needs two admitted receipts"),
                    },
                    None => panic!("{what}: a differential needs two admitted receipts"),
                };
                match AdmittedDifferential::admit(left, right) {
                    Ok(pair) => differentials.push(json!({
                        "ordinal": ordinal,
                        "scenario": scenario,
                        "left": pair.left().selection().label(),
                        "right": pair.right().selection().label(),
                    })),
                    Err(refusal) => refusals.push(json!({
                        "ordinal": ordinal,
                        "scenario": scenario,
                        "action": "differential",
                        "refusal": refusal.label(),
                    })),
                }
            }
            "revert" => {
                let current = state.selector.take().unwrap_or(SelectorState::Retained);
                state.selector = Some(current.revert());
            }
            other => panic!("{what}: unknown action {other:?}"),
        }
    }

    let selector_state = scenarios
        .iter()
        .map(|(name, state)| {
            let label = match &state.selector {
                None | Some(SelectorState::Retained) => "retained".to_owned(),
                Some(SelectorState::Attached(receipt)) => {
                    format!("attached:{}", receipt.selection().label())
                }
            };
            (name.clone(), label)
        })
        .collect();

    Replay {
        durable,
        refusals,
        confirmations,
        differentials,
        selector_state,
        engines,
    }
}

/// The projection, folded from the durable rows and the refusals.
fn projection(replay: &Replay) -> Value {
    let mut engines = Map::new();
    for (engine, count) in &replay.engines {
        engines.insert(engine.clone(), json!(count));
    }
    let mut selector = Map::new();
    for (scenario, state) in &replay.selector_state {
        selector.insert(scenario.clone(), json!(state));
    }
    json!({
        "admitted_receipts": replay.durable.len(),
        "engines_admitted": Value::Object(engines),
        "confirmations": replay.confirmations,
        "differentials_admitted": replay.differentials,
        "refusals": replay.refusals,
        "selector_state": Value::Object(selector),
    })
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn validate_manifest(manifest: &Value) {
    let what = "manifest";
    // `$schema` is checked by tests/schema_conformance.rs against the ratified
    // file itself, not restated here — see the `$schema` note at the top of this
    // file.
    assert_eq!(require_str(manifest, "id", what), CORPUS_ID);
    assert_eq!(
        manifest.get("schema_version").and_then(Value::as_u64),
        Some(1),
        "schema_version must be the number 1, not a string"
    );

    // The absent-oracle branch. `oracle` must be OMITTED, not null: a null
    // would still be a declared property, and the schema's own contract is that
    // a scenario with nothing to capture names no oracle at all.
    assert!(
        manifest.get("oracle").is_none(),
        "this corpus has no TypeScript oracle, so it must carry no `oracle` property at all"
    );

    let conformance = match manifest.get("law_conformance") {
        Some(value) => value,
        None => panic!("{what}: `law_conformance` is required on the absent-oracle branch"),
    };
    assert_eq!(
        require_str(conformance, "basis", "law_conformance"),
        "ratified-law-pack"
    );
    assert_eq!(
        require_str(conformance, "oracle_status", "law_conformance"),
        "not-implemented-in-frozen-typescript-engine"
    );
    assert!(
        require_str(conformance, "note", "law_conformance").contains("defaultServerUrl"),
        "the note must name the exact absence, which is the bare URL the frozen desktop persists"
    );

    let law_pack = match manifest.get("law_pack") {
        Some(value) => value,
        None => panic!("{what}: `law_pack` is required"),
    };
    assert_eq!(
        law_pack.get("revision").and_then(Value::as_u64),
        Some(LAW_PACK_REVISION)
    );
    let sources = match law_pack.get("sources").and_then(Value::as_array) {
        Some(sources) => sources,
        None => panic!("law_pack: `sources` must be an array"),
    };
    assert_eq!(
        sources.len(),
        LAW_PACK_SOURCES,
        "the ordered law pack has exactly nine sources"
    );
    for (index, source) in sources.iter().enumerate() {
        let sha = require_str(source, "sha256", "law_pack source");
        assert_eq!(
            sha.len(),
            64,
            "law_pack source {index} must pin a full sha256"
        );
        assert!(
            sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "law_pack source {index} sha256 must be hex"
        );
    }
    assert!(
        law_pack.get("matrix").is_some(),
        "law_pack must pin the supersession matrix"
    );

    let paths = match manifest.get("paths") {
        Some(value) => value,
        None => panic!("{what}: `paths` is required"),
    };
    assert_eq!(require_str(paths, "actions", "paths"), "actions.jsonl");
    let expected = match paths.get("expected") {
        Some(value) => value,
        None => panic!("paths: `expected` is required"),
    };
    assert_eq!(
        require_str(expected, "durable", "paths.expected"),
        "expected/durable.jsonl"
    );
    assert_eq!(
        require_str(expected, "state", "paths.expected"),
        "expected/state.json"
    );
    assert_eq!(
        require_str(expected, "waivers", "paths.expected"),
        "expected/waivers.json"
    );

    let contracts = match manifest.get("record_contracts") {
        Some(value) => value,
        None => panic!("{what}: `record_contracts` is required"),
    };
    for key in ["actions", "durable", "state"] {
        let text = require_str(contracts, key, "record_contracts");
        assert!(
            !text.is_empty(),
            "record_contracts.{key} must state what the surface is"
        );
    }
    // Ruling: the contract must say plainly what this lane does NOT prove.
    let durable_contract = require_str(contracts, "durable", "record_contracts");
    assert!(
        durable_contract.contains("not a filesystem record"),
        "the durable contract must state that this is the receipt model, not filesystem persistence"
    );

    let evidence = match manifest.get("evidence").and_then(Value::as_array) {
        Some(evidence) => evidence,
        None => panic!("{what}: `evidence` must be a non-empty array"),
    };
    assert!(!evidence.is_empty(), "evidence must not be empty");
    for row in evidence {
        for key in ["path", "test", "claim"] {
            assert!(
                !require_str(row, key, "evidence row").is_empty(),
                "evidence row `{key}` must not be empty"
            );
        }
    }
}

fn validate_waivers(waivers: &Value) {
    let rows = match waivers.get("waivers").and_then(Value::as_array) {
        Some(rows) => rows,
        None => panic!("waivers: `waivers` must be an array"),
    };
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| require_str(row, "id", "waiver row"))
        .collect();
    assert!(
        ids.contains(&REQUIRED_WAIVER),
        "the absent-oracle waiver {REQUIRED_WAIVER:?} must be present, found {ids:?}"
    );
    for row in rows {
        let reason = require_str(row, "reason", "waiver row");
        assert!(
            reason.contains("defaultServerUrl"),
            "the waiver must name the exact absence rather than gesturing at it"
        );
    }
}

#[test]
fn the_corpus_replays_through_the_model_and_matches_every_expected_surface() {
    let dir = corpus_dir();
    let manifest = parse_json(&read_required(&dir.join("manifest.json")), "manifest.json");
    validate_manifest(&manifest);
    validate_waivers(&parse_json(
        &read_required(&dir.join("expected/waivers.json")),
        "expected/waivers.json",
    ));

    let actions = read_required(&dir.join("actions.jsonl"));
    let replayed = replay(&actions);

    // Durable: every row, in order, compared exactly.
    let produced_durable: Vec<String> = replayed
        .durable
        .iter()
        .map(serde_json::Value::to_string)
        .collect();
    let recorded_durable: Vec<String> = read_required(&dir.join("expected/durable.jsonl"))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| parse_json(line, "expected/durable.jsonl").to_string())
        .collect();

    assert_eq!(
        produced_durable.len(),
        recorded_durable.len(),
        "expected/durable.jsonl has {} rows but the model produced {}\nmodel output:\n{}",
        recorded_durable.len(),
        produced_durable.len(),
        produced_durable.join("\n")
    );
    for (index, (produced, recorded)) in produced_durable
        .iter()
        .zip(recorded_durable.iter())
        .enumerate()
    {
        assert_eq!(
            produced,
            recorded,
            "expected/durable.jsonl row {} does not match the model",
            index + 1
        );
    }

    // State: the whole projection, compared as one value so an added or
    // dropped key fails rather than being ignored.
    let produced_state = projection(&replayed);
    let recorded_state = parse_json(
        &read_required(&dir.join("expected/state.json")),
        "expected/state.json",
    );
    assert_eq!(
        produced_state,
        recorded_state,
        "expected/state.json does not match the model\nmodel output:\n{}",
        match serde_json::to_string_pretty(&produced_state) {
            Ok(text) => text,
            Err(error) => format!("<unserialisable: {error}>"),
        }
    );
}

#[test]
fn every_recorded_refusal_word_is_the_models_own_label() {
    // The refusal vocabulary, checked as a SET and reported by name.
    //
    // Separate from the whole-projection comparison on purpose. That comparison
    // fails identically whichever label is changed, so two label laws resting on
    // it would share one record and one of them would not be independently
    // witnessed — which is exactly what this lane's duplicate-evidence guard
    // refused. Here the failure names the differing word, so each label has a
    // record of its own.
    let dir = corpus_dir();
    let state = parse_json(
        &read_required(&dir.join("expected/state.json")),
        "expected/state.json",
    );
    let Some(refusals) = state.get("refusals").and_then(Value::as_array) else {
        panic!("expected/state.json carries no `refusals` array");
    };

    let recorded: BTreeSet<String> = refusals
        .iter()
        .map(|row| require_str(row, "refusal", "recorded refusal").to_owned())
        .collect();

    // Every variant the model can produce, by its own label. All four must reach
    // the recorded projection: a word carried by nothing is a vocabulary claim
    // with no carrier.
    let model: BTreeSet<String> = [
        AttachmentRefusal::Unattributed,
        AttachmentRefusal::NotTheRecordedOwner,
        AttachmentRefusal::SameOwnerOnBothSides,
        AttachmentRefusal::SameSelectionOnBothSides,
    ]
    .iter()
    .map(|refusal| refusal.label().to_owned())
    .collect();

    // The difference is put IN the message, not left to assert_eq!'s separate
    // left/right lines: the duplicate-evidence guard compares the panic line, so
    // a message that does not name the word would give every label mutation one
    // shared record — which is the very sharing this witness exists to end.
    let unrecorded: Vec<&str> = model.difference(&recorded).map(String::as_str).collect();
    let unproduced: Vec<&str> = recorded.difference(&model).map(String::as_str).collect();
    assert!(
        unrecorded.is_empty() && unproduced.is_empty(),
        "refusal vocabulary drift: the model produces {unrecorded:?}, which no recorded refusal          carries, and expected/state.json records {unproduced:?}, which the model no longer          produces; every one of the four labels must reach the projection"
    );
}
