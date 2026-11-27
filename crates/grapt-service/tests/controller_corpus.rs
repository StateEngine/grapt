//! R4D-2C acceptance corpus: `fixtures/rust-reconstruction/r4-service-controller`.
//!
//! `actions.jsonl` is replayed through this crate's **real public API** — the
//! same `ServiceController`, `RegistrationRecord` and store a caller would use
//! — and the two expected surfaces are **produced** from the model's own
//! answers, then compared against the committed files exhaustively.
//!
//! Producing rather than projecting is what stops the expectation being
//! self-derived. Nothing in `expected/` is hand-written, and nothing is written
//! back during a run: the committed bytes are the fixed point, so a model that
//! changed its mind about any outcome, label, generation or registration would
//! fail here rather than quietly re-blessing itself.
//!
//! # What the corpus deliberately does not contain
//!
//! No service, no socket, no process and no port. Every owner receipt is built
//! from its action row, every instant is supplied by the row through an
//! injected `ClockAuthority`, and the store is in-memory. The **real** file
//! lock, the atomic write and the crash cuts are witnessed against a real
//! filesystem in `registration_durability.rs`; this corpus is the transition
//! ledger, not a durability claim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use grapt_service::{
    ControlRefusal, ElectedControllerAuthority, EnsureOutcome, Observation, ProbeOutcome,
    RegistrationRecord, ReplacementReason, Retirement, ServiceController, SlotState, StopOutcome,
    UpgradeTransitionRefusal,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ReplacementOutcome, ServiceGeneration,
    ServiceInstanceId, ServiceRegistrationVersion, ServiceSlotId,
};
use serde_json::{json, Map, Value};

type R = Result<(), Box<dyn std::error::Error>>;
type Fail = Box<dyn std::error::Error>;

const SLOT: &str = "slot-secode";
const ENVIRONMENT: &str = "env-alpha";

/// A clock whose instant each action row supplies. Interior mutability is not
/// needed: the controller borrows the clock, so a new one is built per row.
#[derive(Debug)]
struct RowClock(i64);

impl ClockAuthority for RowClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r4-service-controller")
}

/// Read a declared corpus path, failing loudly. A missing expected file is a
/// failure, never a skip.
fn read(path: &Path) -> Result<Vec<u8>, Fail> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()).into())
}

/// Read a corpus file as text with its line endings normalised.
///
/// This repository runs with `core.autocrlf=true` and had no `.gitattributes`
/// reaching this corpus until this lane added one, so a fresh default-config
/// clone would otherwise receive CRLF here. The comparison below is
/// deliberately byte-exact against what the model PRODUCES, which is LF, so a
/// CRLF checkout would fail the replay for a reason that has nothing to do with
/// the controller — a false red that reads exactly like a real one.
///
/// Normalising here rather than relying on the attribute alone is the point:
/// `grapt-api`'s own `.gitattributes` header records a gate that reddened on a
/// fresh clone precisely because a checkout contract was its only defence. The
/// attribute pins what a reviewer's clone holds; this stops the gate depending
/// on it.
///
/// It does not weaken the comparison. Every other byte still has to match, and
/// the committed files are LF — the CR census in this lane's receipt measures
/// the index rather than the worktree, so normalising here cannot hide a CRLF
/// file that was actually committed.
fn read_text(path: &Path) -> Result<String, Fail> {
    Ok(String::from_utf8(read(path)?)?.replace("\r\n", "\n"))
}

fn field<'a>(row: &'a Value, name: &str) -> Result<&'a Value, Fail> {
    row.get(name)
        .ok_or_else(|| format!("action row has no `{name}` field: {row}").into())
}

fn text<'a>(row: &'a Value, name: &str) -> Result<&'a str, Fail> {
    field(row, name)?
        .as_str()
        .ok_or_else(|| format!("`{name}` is not a string in {row}").into())
}

fn integer(row: &Value, name: &str) -> Result<i64, Fail> {
    field(row, name)?
        .as_i64()
        .ok_or_else(|| format!("`{name}` is not an integer in {row}").into())
}

/// Build an owner receipt from a corpus coordinate object.
///
/// `slot` and `environment` default to this corpus's, so a row that overrides
/// one is visibly exercising exactly that law.
fn owner_of(value: &Value) -> Result<OwnerReceipt, Fail> {
    let instance = text(value, "instance")?;
    let generation = integer(value, "generation")?;
    let pid = integer(value, "pid")?;
    let port = integer(value, "port")?;
    let slot = value.get("slot").and_then(Value::as_str).unwrap_or(SLOT);
    let environment = value
        .get("environment")
        .and_then(Value::as_str)
        .unwrap_or(ENVIRONMENT);
    let generation = u64::try_from(generation)
        .map_err(|_| format!("generation {generation} is not representable"))?;
    let pid = u32::try_from(pid).map_err(|_| format!("pid {pid} is not representable"))?;
    Ok(OwnerReceipt::new(
        ServiceSlotId::new(slot).ok_or("slot id")?,
        ServiceInstanceId::new(instance).ok_or("instance id")?,
        EnvironmentReceipt::new(environment).ok_or("environment receipt")?,
        ServiceGeneration::new(generation),
        ProcessId::new(pid).ok_or("process id")?,
        Endpoint::new(format!("http://127.0.0.1:{port}")).ok_or("endpoint")?,
    ))
}

/// Turn a row's `probe` field into a probe outcome against `declared`.
fn probe_of(row: &Value, declared: &OwnerReceipt) -> Result<ProbeOutcome, Fail> {
    match field(row, "probe")? {
        Value::String(kind) if kind == "self" => Ok(ProbeOutcome::Attributed(declared.clone())),
        Value::String(kind) if kind == "unattributed" => Ok(ProbeOutcome::HealthyUnattributed),
        Value::String(kind) => Err(format!("unknown probe kind {kind:?}").into()),
        Value::Object(_) => {
            let attributed = owner_of(field(row, "probe")?)?;
            Ok(ProbeOutcome::Attributed(attributed))
        }
        other => Err(format!("`probe` is neither a kind nor a coordinate object: {other}").into()),
    }
}

fn retirement_of(row: &Value) -> Result<Retirement, Fail> {
    match text(row, "retirement")? {
        "no-incumbent" => Ok(Retirement::NoIncumbent),
        "retired" => Ok(Retirement::Retired),
        "timed-out" => Ok(Retirement::TimedOut),
        "failed" => Ok(Retirement::Failed {
            reason: "the incumbent did not exit".to_owned(),
        }),
        other => Err(format!("unknown retirement {other:?}").into()),
    }
}

fn outcome_label(outcome: &ReplacementOutcome) -> &'static str {
    match outcome {
        ReplacementOutcome::Reused => "reused",
        ReplacementOutcome::Replaced => "replaced",
        ReplacementOutcome::ReplacedRetirePending => "replaced-retire-pending",
        ReplacementOutcome::RolledBack => "rolled-back",
        ReplacementOutcome::LostRace => "lost-race",
        ReplacementOutcome::FailedAfterCommit => "failed-after-commit",
    }
}

/// An owner rendered for the projection: the three identity coordinates first,
/// then the attributes, so a reader can see which is which.
fn render_owner(owner: &OwnerReceipt) -> Value {
    json!({
        "slot": owner.slot().as_str(),
        "instance": owner.instance().as_str(),
        "environment": owner.environment().as_str(),
        "generation": owner.generation().value(),
        "pid": owner.pid().value(),
        "endpoint": owner.endpoint().as_str(),
    })
}

fn render_slot(store: &ElectedControllerAuthority) -> Result<Value, Fail> {
    match store.read_registration()? {
        None => Ok(json!({"state": "empty"})),
        Some(bytes) => match RegistrationRecord::decode(&bytes) {
            Ok(record) => Ok(json!({
                "state": "registered",
                "owner": render_owner(record.owner()),
                "version": record.version().as_str(),
                "registered_at": record.registered_at().as_millis(),
            })),
            Err(error) => Ok(json!({"state": "unreadable", "because": error.to_string()})),
        },
    }
}

fn reason_label(reason: &ReplacementReason) -> &'static str {
    match reason {
        ReplacementReason::SlotEmpty => "slot-empty",
        ReplacementReason::UnreadableRegistration(_) => "unreadable-registration",
        ReplacementReason::StaleRegistration { .. } => "stale-registration",
        ReplacementReason::IncompatibleVersion { .. } => "incompatible-version",
        ReplacementReason::ObservedVersionMismatch { .. } => "observed-version-mismatch",
    }
}

fn stop_label(outcome: &StopOutcome) -> &'static str {
    match outcome {
        StopOutcome::Released { .. } => "released",
        StopOutcome::SlotAlreadyEmpty => "slot-already-empty",
        StopOutcome::NotTheOwner { .. } => "not-the-owner",
        StopOutcome::LostRace { .. } => "lost-race",
    }
}

/// The replay: every durable row, every refusal, in action order.
#[derive(Debug, Default)]
struct Replay {
    durable: Vec<Value>,
    refusals: Vec<Value>,
}

impl Replay {
    fn refuse(&mut self, ordinal: i64, refusal: &ControlRefusal) {
        self.refusals.push(json!({
            "ordinal": ordinal,
            "label": refusal.label(),
            "message": refusal.to_string(),
        }));
    }

    fn refuse_upgrade(&mut self, ordinal: i64, refusal: &UpgradeTransitionRefusal) {
        self.refusals.push(json!({
            "ordinal": ordinal,
            "label": refusal.label(),
            "message": refusal.to_string(),
        }));
    }
}

fn action_version(row: &Value) -> Result<ServiceRegistrationVersion, Fail> {
    let raw = row
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("0.1.0");
    ServiceRegistrationVersion::new(raw)
        .ok_or_else(|| format!("action version {raw:?} is not storable").into())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn replay(actions: &str) -> Result<Replay, Fail> {
    let store = ElectedControllerAuthority::isolated();
    let mut out = Replay::default();

    for (index, line) in actions.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(line)
            .map_err(|error| format!("action line {} is not JSON: {error}", index + 1))?;
        let ordinal = integer(&row, "ordinal")?;
        let expected_ordinal = i64::try_from(index + 1).unwrap_or(-1);
        if ordinal != expected_ordinal {
            return Err(format!(
                "action ordinals must be unique and ascending from one: line {} declares {ordinal}",
                index + 1
            )
            .into());
        }
        let action = text(&row, "action")?.to_owned();
        // The clock is per-row and comes from the row, so no ambient time is
        // read anywhere in this replay.
        let clock = RowClock(row.get("now").and_then(Value::as_i64).unwrap_or(0));
        let controller = ServiceController::new(
            ServiceSlotId::new(SLOT).ok_or("slot")?,
            EnvironmentReceipt::new(ENVIRONMENT).ok_or("environment")?,
            &store,
            &clock,
        );

        match action.as_str() {
            "seed" => {
                let bytes = match row.get("raw").and_then(Value::as_str) {
                    Some(raw) => Some(raw.as_bytes().to_vec()),
                    None => match row.get("slot_state").and_then(Value::as_str) {
                        Some("empty") => None,
                        other => {
                            return Err(
                                format!("seed row declares no usable state: {other:?}").into()
                            )
                        }
                    },
                };
                store.seed_isolated(bytes)?;
                out.durable.push(json!({
                    "ordinal": ordinal,
                    "transition": "seed",
                    "slot": render_slot(&store)?,
                }));
            }
            "ensure" => {
                let required = ServiceRegistrationVersion::new(text(&row, "required_version")?)
                    .ok_or("required version")?;
                let discovered = controller.discover()?;
                // A row may probe, or deliberately not probe at all.
                let observation = match field(&row, "probe")? {
                    Value::Null => None,
                    _ => {
                        let registered = discovered.registered_owner().cloned();
                        let declared = registered.ok_or(
                            "an ensure row that probes needs a registered owner to probe for",
                        )?;
                        let probe = probe_of(&row, &declared)?;
                        let observed_version = match row.get("observed_version") {
                            Some(value) => ServiceRegistrationVersion::new(
                                value.as_str().ok_or("observed_version is not a string")?,
                            )
                            .ok_or("observed version")?,
                            None => match discovered.state() {
                                SlotState::Registered(record) => record.version().clone(),
                                SlotState::Empty | SlotState::Unreadable(_) => {
                                    return Err(
                                        "an attributed ensure needs a readable record".into()
                                    )
                                }
                            },
                        };
                        Some(Observation::new(declared, probe, observed_version))
                    }
                };
                let outcome = controller.ensure(&discovered, observation.as_ref(), &required);
                let rendered = match &outcome {
                    EnsureOutcome::ReusedIncumbent { owner } => json!({
                        "decision": "reused-incumbent",
                        "outcome": outcome.reused().as_ref().map(outcome_label),
                        "owner": render_owner(owner),
                    }),
                    EnsureOutcome::ReplacementRequired { reason } => json!({
                        "decision": "replacement-required",
                        "reason": reason_label(reason),
                    }),
                };
                out.durable.push(json!({
                    "ordinal": ordinal,
                    "transition": "ensure",
                    "result": rendered,
                }));
            }
            "switch" | "reclaim" => {
                let declared = owner_of(field(&row, "candidate")?)?;
                let probe = probe_of(&row, &declared)?;
                let candidate = Observation::new(declared, probe, action_version(&row)?);
                let discovered = controller.discover()?;
                let prepared = if action == "switch" {
                    controller.prepare(&discovered, &candidate)
                } else {
                    controller.reclaim(&discovered, &candidate)
                };
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(refusal) => {
                        out.refuse(ordinal, &refusal);
                        continue;
                    }
                };
                let receipt = match controller.commit(prepared, retirement_of(&row)?) {
                    Ok(receipt) => receipt,
                    Err(refusal) => {
                        out.refuse(ordinal, &refusal);
                        continue;
                    }
                };
                let receipt = match row.get("post_commit").and_then(Value::as_str) {
                    Some("candidate-failed") => receipt.candidate_failed_after_commit()?,
                    Some(other) => {
                        return Err(format!("unknown post_commit {other:?}").into());
                    }
                    None => receipt,
                };
                out.durable.push(json!({
                    "ordinal": ordinal,
                    "transition": action,
                    "outcome": outcome_label(&receipt.outcome()),
                    "committed_owner": receipt.committed_owner().map(render_owner),
                    "switched_at": receipt.switched_at().as_millis(),
                    "slot": render_slot(&store)?,
                }));
            }
            "upgrade" => {
                let declared = owner_of(field(&row, "candidate")?)?;
                let probe = probe_of(&row, &declared)?;
                let candidate = Observation::new(declared, probe, action_version(&row)?);
                let discovered = controller.discover()?;
                let prepared = match controller.prepare_upgrade(&discovered, &candidate) {
                    Ok(prepared) => prepared,
                    Err(refusal) => {
                        out.refuse_upgrade(ordinal, &refusal);
                        continue;
                    }
                };
                let receipt = match controller.commit_upgrade(prepared, retirement_of(&row)?) {
                    Ok(receipt) => receipt,
                    Err(refusal) => {
                        out.refuse(ordinal, &refusal);
                        continue;
                    }
                };
                out.durable.push(json!({
                    "ordinal": ordinal,
                    "transition": "upgrade",
                    "outcome": outcome_label(&receipt.outcome()),
                    "incumbent_version": receipt.incumbent_version().as_str(),
                    "candidate_version": receipt.candidate_version().as_str(),
                    "committed_owner": receipt.committed_owner().map(render_owner),
                    "switched_at": receipt.switch_receipt().switched_at().as_millis(),
                    "slot": render_slot(&store)?,
                }));
            }
            "abandon" => {
                let declared = owner_of(field(&row, "candidate")?)?;
                let probe = probe_of(&row, &declared)?;
                let candidate = Observation::new(
                    declared,
                    probe,
                    ServiceRegistrationVersion::new("0.1.0").ok_or("version")?,
                );
                let discovered = controller.discover()?;
                let before = store.read_registration()?;
                let prepared = match controller.prepare(&discovered, &candidate) {
                    Ok(prepared) => prepared,
                    Err(refusal) => {
                        out.refuse(ordinal, &refusal);
                        continue;
                    }
                };
                let receipt = prepared.rollback();
                let after = store.read_registration()?;
                out.durable.push(json!({
                    "ordinal": ordinal,
                    "transition": "abandon",
                    "outcome": outcome_label(&receipt.outcome()),
                    "abandoned": render_owner(receipt.abandoned()),
                    "incumbent": receipt.incumbent().map(render_owner),
                    // The law this row exists to record: a pre-commit rollback
                    // writes nothing at all.
                    "slot_unchanged": before == after,
                    "slot": render_slot(&store)?,
                }));
            }
            "stop" => {
                let owner = owner_of(field(&row, "owner")?)?;
                let discovered = controller.discover()?;
                let outcome = match controller.stop(&discovered, &owner) {
                    Ok(outcome) => outcome,
                    Err(refusal) => {
                        out.refuse(ordinal, &refusal);
                        continue;
                    }
                };
                out.durable.push(json!({
                    "ordinal": ordinal,
                    "transition": "stop",
                    "result": stop_label(&outcome),
                    "slot": render_slot(&store)?,
                }));
            }
            other => return Err(format!("unknown action kind {other:?}").into()),
        }
    }
    Ok(out)
}

/// Fold the replay into the projection.
fn project(replay: &Replay, actions_run: usize) -> Result<Value, Fail> {
    let mut outcomes: BTreeMap<String, u64> = BTreeMap::new();
    for row in &replay.durable {
        if let Some(outcome) = row.get("outcome").and_then(Value::as_str) {
            *outcomes.entry(outcome.to_owned()).or_default() += 1;
        }
    }
    let mut labels: Vec<String> = replay
        .refusals
        .iter()
        .filter_map(|row| row.get("label").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    labels.sort();
    labels.dedup();

    let mut outcome_map = Map::new();
    for (key, count) in outcomes {
        outcome_map.insert(key, json!(count));
    }
    Ok(json!({
        "actions_run": actions_run,
        "durable_rows": replay.durable.len(),
        "refused": replay.refusals.len(),
        "outcomes": Value::Object(outcome_map),
        "refusals": replay.refusals,
        "refusal_labels": labels,
    }))
}

fn canonical(value: &Value) -> Result<String, Fail> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(text)
}

#[test]
fn the_corpus_replays_through_the_model_and_matches_every_expected_surface() -> R {
    let dir = corpus_dir();
    let actions = read_text(&dir.join("actions.jsonl"))?;
    let actions_run = actions.lines().filter(|line| !line.is_empty()).count();
    let replay = replay(&actions)?;

    let mut produced_durable = String::new();
    for row in &replay.durable {
        produced_durable.push_str(&serde_json::to_string(row)?);
        produced_durable.push('\n');
    }
    let produced_state = canonical(&project(&replay, actions_run)?)?;

    let expected_durable = read_text(&dir.join("expected/durable.jsonl"))?;
    let expected_state = read_text(&dir.join("expected/state.json"))?;

    if produced_durable != expected_durable {
        return Err(format!(
            "expected/durable.jsonl does not match what the model produced.\n\
             ---- produced ----\n{produced_durable}---- end ----"
        )
        .into());
    }
    if produced_state != expected_state {
        return Err(format!(
            "expected/state.json does not match what the model produced.\n\
             ---- produced ----\n{produced_state}---- end ----"
        )
        .into());
    }
    Ok(())
}

#[test]
fn every_refusal_the_corpus_records_is_a_label_this_lane_declares() -> R {
    // A projection may never carry a word the lane does not own. The other
    // direction — that every declared label maps from a distinct variant — is
    // held in `controller_contract.rs`, where the exotic variants can be built.
    let dir = corpus_dir();
    let state: Value = serde_json::from_slice(&read(&dir.join("expected/state.json"))?)?;
    let labels = state
        .get("refusal_labels")
        .and_then(Value::as_array)
        .ok_or("expected/state.json has no refusal_labels array")?;
    assert!(
        !labels.is_empty(),
        "a corpus that recorded no refusal at all would prove none of the refusal laws"
    );
    for label in labels {
        let label = label.as_str().ok_or("a refusal label is not a string")?;
        let declared = ControlRefusal::OWN_LABELS.contains(&label);
        let landed = [
            "unattributed",
            "not-the-recorded-owner",
            "same-owner-on-both-sides",
            "same-selection-on-both-sides",
        ]
        .contains(&label);
        let upgrade = [
            "upgrade-no-incumbent",
            "upgrade-incumbent-version-unorderable",
            "upgrade-candidate-version-unorderable",
            "upgrade-same-precedence",
            "upgrade-downgrade",
        ]
        .contains(&label);
        assert!(
            declared || landed || upgrade,
            "the corpus records refusal label {label:?}, which is neither one this lane declares \
             nor one R4D-1 landed"
        );
    }
    Ok(())
}

#[test]
fn the_corpus_reaches_every_slot_state_the_controller_can_observe() -> R {
    // `SlotState` has three variants and collapsing any two would be a real
    // defect, so the corpus must actually visit all three rather than proving
    // two of them twice.
    let dir = corpus_dir();
    let actions = read_text(&dir.join("actions.jsonl"))?;
    let store = ElectedControllerAuthority::isolated();
    let clock = RowClock(0);
    let controller = ServiceController::new(
        ServiceSlotId::new(SLOT).ok_or("slot")?,
        EnvironmentReceipt::new(ENVIRONMENT).ok_or("environment")?,
        &store,
        &clock,
    );

    let mut seen_empty = false;
    let mut seen_registered = false;
    let mut seen_unreadable = false;
    // Replay only the state the seeds and switches leave behind, checking the
    // slot after every row.
    let replayed = replay(&actions)?;
    for row in &replayed.durable {
        if let Some(state) = row.get("slot").and_then(|slot| slot.get("state")) {
            match state.as_str() {
                Some("empty") => seen_empty = true,
                Some("registered") => seen_registered = true,
                Some("unreadable") => seen_unreadable = true,
                other => return Err(format!("unknown recorded slot state {other:?}").into()),
            }
        }
    }
    // And the discovery path itself must classify all three, not just the
    // recorder: a `discover` that never returned `Unreadable` would leave the
    // recovery path unreachable.
    store.seed_isolated(None)?;
    assert!(
        matches!(controller.discover()?.state(), SlotState::Empty),
        "an unclaimed slot must discover as empty"
    );
    store.seed_isolated(Some(b"not a registration\n".to_vec()))?;
    assert!(
        matches!(controller.discover()?.state(), SlotState::Unreadable(_)),
        "a corrupted slot must discover as unreadable"
    );

    assert!(
        seen_empty && seen_registered && seen_unreadable,
        "the corpus must visit all three slot states; it reached empty={seen_empty}, \
         registered={seen_registered}, unreadable={seen_unreadable}"
    );
    Ok(())
}
