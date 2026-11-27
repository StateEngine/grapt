//! Replays `r4-desktop-differential-contract` through the real public API and
//! compares every expected surface exhaustively.
//!
//! # What this owner does and does not do
//!
//! It reads `actions.jsonl`, drives each row through `grapt-service`'s public
//! constructors, **produces** the durable rows and the projection from the
//! model's own answers, and then compares them to the committed
//! `expected/durable.jsonl` and `expected/state.json`. Nothing in `expected/*`
//! is projected from the corpus input or hand-maintained alongside the model: a
//! change in behaviour moves the produced side and the comparison fails.
//!
//! A missing or unreadable expected file is a loud failure, never a skip. So is
//! a malformed action row — a reader that silently tolerated one would weaken
//! the comparison it exists to make.
//!
//! # It measures no engine
//!
//! Every owner receipt is a value built from its action row, every instant is
//! supplied by the row through an injected clock, and no socket is opened. The
//! recorded bound renderings are deliberately written as `unmeasured-*` tokens:
//! they exercise the recording law and are never parsed by the crate, and no
//! value in this corpus was read from a running host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use grapt_service::{
    AdmittedDifferential, BoundProvenance, DifferentialMatrix, DifferentialPlan,
    DifferentialRefusal, EngineSelection, HostBoundName, LongSessionBound, MatrixEvidence,
    Operation, OperationSubset, ProbeOutcome, RecordedBound, RecordedHostBounds, RestartBoundary,
    ResumeReadmission, SelectionReceipt, SelectorState,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceSlotId,
};
use serde_json::{json, Value};

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-desktop-differential-contract")
}

fn read(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) => panic!(
            "cannot read {}: {error}. A corpus surface that cannot be read is a failure, not a skip.",
            path.display()
        ),
    }
}

fn parse(body: &str, what: &str) -> Value {
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

fn require_i64(value: &Value, key: &str, what: &str) -> i64 {
    match value.get(key).and_then(Value::as_i64) {
        Some(number) => number,
        None => panic!("{what}: `{key}` is missing or not an integer"),
    }
}

fn require_u64(value: &Value, key: &str, what: &str) -> u64 {
    match value.get(key).and_then(Value::as_u64) {
        Some(number) => number,
        None => panic!("{what}: `{key}` is missing or not an unsigned integer"),
    }
}

fn owner_from(value: &Value, what: &str) -> OwnerReceipt {
    let parts = (
        ServiceSlotId::new(require_str(value, "slot", what)),
        ServiceInstanceId::new(require_str(value, "instance", what)),
        EnvironmentReceipt::new(require_str(value, "environment", what)),
        u32::try_from(require_u64(value, "pid", what))
            .ok()
            .and_then(ProcessId::new),
        Endpoint::new(require_str(value, "endpoint", what)),
    );
    match parts {
        (Some(slot), Some(instance), Some(environment), Some(pid), Some(endpoint)) => {
            OwnerReceipt::new(
                slot,
                instance,
                environment,
                ServiceGeneration::new(require_u64(value, "generation", what)),
                pid,
                endpoint,
            )
        }
        _ => panic!("{what}: one of slot/instance/environment/pid/endpoint is unrepresentable"),
    }
}

fn selection_from(value: &Value, key: &str, what: &str) -> EngineSelection {
    let label = require_str(value, key, what);
    match EngineSelection::from_label(label) {
        Some(selection) => selection,
        None => panic!("{what}: `{label}` is not an engine this build knows"),
    }
}

/// Admit a receipt from an action row's probe.
///
/// The `probe` key selects between an attributed answer and a health-only one,
/// which is how the corpus reaches R4D-1's `Unattributed` refusal through this
/// lane's projection.
fn receipt_from(value: &Value, what: &str) -> Result<SelectionReceipt, DifferentialRefusal> {
    let selection = selection_from(value, "selection", what);
    let owner = match value.get("owner") {
        Some(owner) => owner_from(owner, what),
        None => panic!("{what}: `owner` is missing"),
    };
    let probe = match value.get("probe").and_then(Value::as_str) {
        Some("attributed") | None => ProbeOutcome::Attributed(owner),
        Some("healthy-unattributed") => ProbeOutcome::HealthyUnattributed,
        Some(other) => panic!("{what}: `{other}` is not a probe outcome this build knows"),
    };
    let clock = FixedClock(require_i64(value, "selected_at_millis", what));
    Ok(SelectionReceipt::admit(selection, probe, &clock)?)
}

fn matrix_from(label: &str, what: &str) -> DifferentialMatrix {
    match DifferentialMatrix::from_label(label) {
        Some(matrix) => matrix,
        None => panic!("{what}: `{label}` is not a matrix this build knows"),
    }
}

fn operations_from(value: &Value, what: &str) -> Vec<Operation> {
    let listed = match value.get("operations").and_then(Value::as_array) {
        Some(listed) => listed,
        None => panic!("{what}: `operations` is missing or not an array"),
    };
    listed
        .iter()
        .map(
            |entry| match entry.as_str().and_then(Operation::from_operation_id) {
                Some(operation) => operation,
                None => panic!("{what}: `{entry}` is not an operation this contract owns"),
            },
        )
        .collect()
}

fn bounds_from(listed: &Value, what: &str) -> Vec<RecordedBound> {
    let rows = match listed.as_array() {
        Some(rows) => rows,
        None => panic!("{what}: `bounds` is not an array"),
    };
    rows.iter()
        .map(|row| {
            let triple = match row.as_array() {
                Some(triple) if triple.len() == 3 => triple,
                _ => panic!("{what}: every bound row is [name, rendering, provenance]"),
            };
            let name = match triple[0].as_str().and_then(HostBoundName::from_field_name) {
                Some(name) => name,
                None => panic!(
                    "{what}: `{}` is not a host bound this build knows",
                    triple[0]
                ),
            };
            let rendering = match triple[1].as_str() {
                Some(rendering) => rendering,
                None => panic!("{what}: a bound rendering must be a string"),
            };
            let provenance = match triple[2].as_str().and_then(BoundProvenance::from_label) {
                Some(provenance) => provenance,
                None => panic!(
                    "{what}: `{}` is not a provenance this build knows",
                    triple[2]
                ),
            };
            RecordedBound::new(name, rendering, provenance)
        })
        .collect()
}

fn evidence_from(value: &Value, what: &str) -> Result<MatrixEvidence, DifferentialRefusal> {
    let evidence = match value.get("evidence") {
        Some(evidence) => evidence,
        None => panic!("{what}: `evidence` is missing"),
    };
    match require_str(evidence, "kind", what) {
        "none" => Ok(MatrixEvidence::None),
        "restart" => {
            let before = match evidence.get("before") {
                Some(before) => owner_from(before, what),
                None => panic!("{what}: restart evidence needs `before`"),
            };
            let after = match evidence.get("after") {
                Some(after) => owner_from(after, what),
                None => panic!("{what}: restart evidence needs `after`"),
            };
            Ok(MatrixEvidence::Restart(RestartBoundary::observe(
                before, after,
            )?))
        }
        "resume" => {
            let before = match evidence.get("before_sleep") {
                Some(before) => receipt_from(before, what)?,
                None => panic!("{what}: resume evidence needs `before_sleep`"),
            };
            let after = match evidence.get("after_resume") {
                Some(after) => receipt_from(after, what)?,
                None => panic!("{what}: resume evidence needs `after_resume`"),
            };
            Ok(MatrixEvidence::Resume(ResumeReadmission::readmit(
                before, after,
            )?))
        }
        "long-session" => Ok(MatrixEvidence::LongSession(LongSessionBound::declare(
            match u32::try_from(require_u64(evidence, "max_reads", what)) {
                Ok(reads) => reads,
                Err(_) => panic!("{what}: `max_reads` does not fit a u32"),
            },
            require_u64(evidence, "max_bytes", what),
        )?)),
        other => panic!("{what}: `{other}` is not an evidence kind this build knows"),
    }
}

fn terminal_selector_from(value: &Value, left: &SelectionReceipt, what: &str) -> SelectorState {
    match require_str(value, "terminal_selector", what) {
        "retained" => SelectorState::Retained,
        "attached-to-rust" => SelectorState::attach(left.clone()),
        other => panic!("{what}: `{other}` is not a terminal selector this build knows"),
    }
}

/// Run one `plan` row through the model, returning the plan or the refusal.
fn run_plan(
    row: &Value,
    catalog: &[RecordedBound],
    what: &str,
) -> Result<DifferentialPlan, DifferentialRefusal> {
    let matrix = matrix_from(require_str(row, "matrix", what), what);
    // A subset is normally declared for the matrix it will be planned as. One
    // row declares it for another, which is the only way to reach the
    // subset-borrowed law.
    let declared_for = match row.get("subset_declared_for").and_then(Value::as_str) {
        Some(label) => matrix_from(label, what),
        None => matrix,
    };
    let subset = OperationSubset::declare(declared_for, operations_from(row, what))?;

    let left = match row.get("left") {
        Some(left) => receipt_from(left, what)?,
        None => panic!("{what}: `left` is missing"),
    };
    let right = match row.get("right") {
        Some(right) => receipt_from(right, what)?,
        None => panic!("{what}: `right` is missing"),
    };
    let terminal = terminal_selector_from(row, &left, what);
    let engines = AdmittedDifferential::admit(left, right)?;

    let recorded = match row.get("bounds") {
        Some(bounds) => bounds_from(bounds, what),
        None => catalog.to_vec(),
    };
    let bounds = RecordedHostBounds::record(recorded)?;

    let evidence = evidence_from(row, what)?;
    DifferentialPlan::admit(matrix, subset, engines, bounds, evidence, terminal)
}

fn receipt_projection(receipt: &SelectionReceipt) -> Value {
    let owner = receipt.owner();
    json!({
        "selection": receipt.selection().label(),
        "slot": owner.slot().as_str(),
        "instance": owner.instance().as_str(),
        "environment": owner.environment().as_str(),
        "generation": owner.generation().value(),
        "pid": owner.pid().value(),
        // Read through the owner, so a row cannot disagree with itself about
        // where it attached.
        "endpoint": receipt.endpoint().as_str(),
        "selected_at_millis": receipt.selected_at().as_millis(),
    })
}

fn evidence_projection(evidence: &MatrixEvidence) -> Value {
    match evidence {
        MatrixEvidence::None => json!({"kind": "none"}),
        MatrixEvidence::Restart(boundary) => json!({
            "kind": "restart",
            "slot": boundary.before().slot().as_str(),
            "before_instance": boundary.before().instance().as_str(),
            "after_instance": boundary.after().instance().as_str(),
            "before_pid": boundary.before().pid().value(),
            "after_pid": boundary.after().pid().value(),
        }),
        // The resume projection carries the OWNER, not only the two instants.
        // The R1 review found that recording instants alone left a resume for
        // the wrong engine with no trace in the corpus at all — the projection
        // could not witness the law even once the law existed.
        MatrixEvidence::Resume(readmission) => json!({
            "kind": "resume",
            "selection": readmission.before_sleep().selection().label(),
            "slot": readmission.before_sleep().owner().slot().as_str(),
            "instance": readmission.before_sleep().owner().instance().as_str(),
            "environment": readmission.before_sleep().owner().environment().as_str(),
            "after_slot": readmission.after_resume().owner().slot().as_str(),
            "after_instance": readmission.after_resume().owner().instance().as_str(),
            "after_environment": readmission.after_resume().owner().environment().as_str(),
            "before_selected_at_millis": readmission.before_sleep().selected_at().as_millis(),
            "after_selected_at_millis": readmission.after_resume().selected_at().as_millis(),
        }),
        MatrixEvidence::LongSession(bound) => json!({
            "kind": "long-session",
            "max_reads": bound.max_reads().get(),
            "max_bytes": bound.max_bytes().get(),
        }),
    }
}

fn durable_row(ordinal: u64, scenario: &str, plan: &DifferentialPlan) -> Value {
    json!({
        "ordinal": ordinal,
        "scenario": scenario,
        "matrix": plan.matrix().label(),
        "operations": plan
            .subset()
            .operations()
            .iter()
            .map(|operation| operation.operation_id())
            .collect::<Vec<_>>(),
        "left": receipt_projection(plan.engines().left()),
        "right": receipt_projection(plan.engines().right()),
        "recorded_bounds": plan
            .rust_bounds()
            .bounds()
            .iter()
            .map(|bound| json!({
                "name": bound.name().field_name(),
                "rendering": bound.reported_rendering(),
                "provenance": bound.provenance().label(),
            }))
            .collect::<Vec<_>>(),
        "evidence": evidence_projection(plan.evidence()),
        "terminal_selector": "retained",
    })
}

struct Replay {
    durable: Vec<Value>,
    state: Value,
}

fn replay() -> Replay {
    let actions = read(&corpus_dir().join("actions.jsonl"));
    let mut catalog: Vec<RecordedBound> = Vec::new();
    let mut durable = Vec::new();
    let mut refusals = Vec::new();
    let mut admitted_by_matrix: BTreeMap<&str, u64> = BTreeMap::new();
    let mut confirmations = Vec::new();
    let mut action_count = 0_u64;

    for (index, line) in actions.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let what = format!("actions.jsonl line {}", index + 1);
        let row = parse(line, &what);
        let ordinal = require_u64(&row, "ordinal", &what);
        let scenario = require_str(&row, "scenario", &what).to_owned();

        match require_str(&row, "action", &what) {
            "bounds-catalog" => {
                let bounds = match row.get("bounds") {
                    Some(bounds) => bounds,
                    None => panic!("{what}: the catalog row needs `bounds`"),
                };
                catalog = bounds_from(bounds, &what);
                assert!(
                    !catalog.is_empty(),
                    "{what}: an empty catalog would make every row that relies on it vacuous"
                );
            }
            "plan" => {
                action_count += 1;
                match run_plan(&row, &catalog, &what) {
                    Ok(plan) => {
                        *admitted_by_matrix.entry(plan.matrix().label()).or_insert(0) += 1;
                        durable.push(durable_row(ordinal, &scenario, &plan));
                    }
                    Err(refusal) => refusals.push(json!({
                        "ordinal": ordinal,
                        "scenario": scenario,
                        "label": refusal.label(),
                        "message": refusal.to_string(),
                    })),
                }
            }
            "confirm" => {
                action_count += 1;
                let recorded = match row.get("recorded") {
                    Some(recorded) => match receipt_from(recorded, &what) {
                        Ok(receipt) => receipt,
                        Err(refusal) => {
                            panic!("{what}: the recorded receipt must admit: {refusal}")
                        }
                    },
                    None => panic!("{what}: `recorded` is missing"),
                };
                let observed = match row.get("observed") {
                    Some(observed) => owner_from(observed, &what),
                    None => panic!("{what}: `observed` is missing"),
                };
                match recorded.still_current(&observed) {
                    Ok(()) => confirmations.push(json!({
                        "ordinal": ordinal,
                        "scenario": scenario,
                        "outcome": "still-the-recorded-owner",
                    })),
                    Err(refusal) => {
                        let refusal: DifferentialRefusal = refusal.into();
                        refusals.push(json!({
                            "ordinal": ordinal,
                            "scenario": scenario,
                            "label": refusal.label(),
                            "message": refusal.to_string(),
                        }));
                    }
                }
            }
            other => panic!("{what}: `{other}` is not an action this owner knows"),
        }
    }

    let mut labels: Vec<String> = refusals
        .iter()
        .filter_map(|refusal| refusal.get("label").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    labels.sort();
    labels.dedup();

    let state = json!({
        "actions": action_count,
        "admitted_plans": durable.len(),
        "refusals": refusals.len(),
        "confirmations": confirmations.len(),
        "admitted_by_matrix": admitted_by_matrix,
        "refusal_labels": labels,
        "ordered_refusals": refusals,
        "confirmed": confirmations,
    });

    Replay { durable, state }
}

fn canonical(value: &Value) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(text) => text,
        Err(error) => panic!("cannot render a produced surface: {error}"),
    }
}

#[test]
fn the_corpus_replays_through_the_model_and_matches_every_expected_surface() {
    let produced = replay();

    let expected_durable = read(&corpus_dir().join("expected/durable.jsonl"));
    let produced_durable: Vec<String> = produced.durable.iter().map(canonical).collect();
    let expected_rows: Vec<String> = expected_durable
        .replace("\r\n", "\n")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| canonical(&parse(line, "expected/durable.jsonl")))
        .collect();

    assert_eq!(
        produced_durable.len(),
        expected_rows.len(),
        "the model admitted {} plans but expected/durable.jsonl records {}",
        produced_durable.len(),
        expected_rows.len()
    );
    for (index, (produced_row, expected_row)) in produced_durable
        .iter()
        .zip(expected_rows.iter())
        .enumerate()
    {
        assert_eq!(
            produced_row,
            expected_row,
            "durable row {} differs between the model and the recorded corpus",
            index + 1
        );
    }

    let expected_state = parse(
        &read(&corpus_dir().join("expected/state.json")),
        "expected/state.json",
    );
    assert_eq!(
        canonical(&produced.state),
        canonical(&expected_state),
        "the projection differs between the model and the recorded corpus"
    );
}

#[test]
fn every_refusal_this_contract_can_produce_reaches_the_recorded_projection() {
    // The corpus is only evidence for the laws it actually exercises. If a law
    // is added without a corpus row, this fails rather than letting the
    // recorded projection quietly cover less than the vocabulary.
    let produced = replay();
    let recorded: Vec<&str> = produced
        .state
        .get("refusal_labels")
        .and_then(Value::as_array)
        .map(|labels| labels.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    // Read from the CARRIER on the enum, never from a literal of this test's
    // own. The R1 review falsified the previous shape: this test compared an
    // 18-entry hand list against the corpus, so a nineteenth variant with a
    // label and no corpus row left the suite green — the exact protection the
    // manifest claims this test provides. `DifferentialRefusal::ALL_LABELS` is
    // bound to the enum by an exhaustive `label_index`, so a new law is a
    // compile error first and an unrecorded label here second.
    let mut expected: Vec<&str> = DifferentialRefusal::ALL_LABELS.to_vec();
    expected.sort();
    assert_eq!(
        recorded, expected,
        "the recorded refusal vocabulary is not the complete one this contract can produce: \
         every label the enum can emit needs at least one corpus row"
    );
}

#[test]
fn the_waivers_file_states_its_exclusions_explicitly() {
    // An empty `waivers` array is a meaningful assertion, so the file must
    // exist and be readable rather than being absent by default.
    let waivers = parse(
        &read(&corpus_dir().join("expected/waivers.json")),
        "expected/waivers.json",
    );
    match waivers.get("waivers").and_then(Value::as_array) {
        Some(_) => {}
        None => panic!("expected/waivers.json must carry a `waivers` array, even when empty"),
    }
}
