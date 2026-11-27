//! D1-RECOVERY language-neutral acceptance runner.
//!
//! `actions.jsonl` contains operations, never Rust test pointers. This runner
//! executes them through the real Inbox, EventStore, recovery owner and
//! SessionHistoryStore, then derives both durable and public-history outcomes.
//! Expected files are compared to those derived values; they are not trusted
//! as an alternative execution path.

use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use grapt_event::AggregateId;
use grapt_inbox::{AdmitOutcome, AdmitRequest, Inbox, PromoteScope};
use grapt_runtime::{recover_process_loss_attempts, ProcessLossRecoveryReport};
use grapt_session::{SessionHistoryStore, StepTerminal};
use grapt_store::{AttemptRecordCeiling, EventStore, ReserveInput, TerminalCoordinates};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, ClockAuthority, DeliveryMode,
    DurableAttemptReceipt, EventId, MessageId, PhysicalAttemptId, RequestSentMarker,
    ReservedMarker, SessionId, StepNumber, Timestamp,
};
use serde_json::{json, Value};

type RunResult<T> = Result<T, String>;

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

struct ScenarioState {
    name: String,
    session: SessionId,
    attempt: PhysicalAttemptId,
    terminal: EventId,
    inbox: Inbox,
    next_step: u64,
    provider_call_count: u64,
    recovery_reports: Vec<ProcessLossRecoveryReport>,
    observations: usize,
}

struct RunOutput {
    durable: Vec<Value>,
    public: Value,
}

fn repository() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    match manifest.parent().and_then(Path::parent) {
        Some(root) => root.to_path_buf(),
        None => panic!("runtime manifest must live under the repository crates directory"),
    }
}

fn corpus() -> PathBuf {
    repository().join("fixtures/rust-reconstruction/d1-recovery-v1")
}

fn read_json(path: &Path) -> RunResult<Value> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn json_lines(path: &Path) -> RunResult<Vec<Value>> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let rows = text
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if line.trim().is_empty() {
                return Err(format!("{} line {} is empty", path.display(), index + 1));
            }
            serde_json::from_str(line)
                .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1))
        })
        .collect::<RunResult<Vec<_>>>()?;
    if rows.is_empty() {
        return Err(format!("{} has no executable rows", path.display()));
    }
    Ok(rows)
}

fn required_str<'a>(row: &'a Value, field: &str) -> RunResult<&'a str> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("action has no non-empty {field}"))
}

fn required_u64(row: &Value, field: &str) -> RunResult<u64> {
    row.get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("action has no unsigned {field}"))
}

fn required_u32(row: &Value, field: &str) -> RunResult<u32> {
    let value = required_u64(row, field)?;
    u32::try_from(value).map_err(|_| format!("action {field} exceeds u32"))
}

fn required_i64(row: &Value, field: &str) -> RunResult<i64> {
    row.get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("action has no signed {field}"))
}

fn required_bool(row: &Value, field: &str) -> RunResult<bool> {
    row.get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("action has no boolean {field}"))
}

fn parse_id<T>(row: &Value, field: &str) -> RunResult<T>
where
    T: std::str::FromStr,
{
    required_str(row, field)?
        .parse()
        .map_err(|_| format!("action {field} is not a valid typed identity"))
}

fn attempt_id(row: &Value, field: &str) -> RunResult<PhysicalAttemptId> {
    PhysicalAttemptId::new(required_str(row, field)?.to_owned())
        .ok_or_else(|| format!("action {field} is empty"))
}

fn ceiling(row: &Value) -> RunResult<AttemptRecordCeiling> {
    let raw = required_u32(row, "receipt_ceiling")?;
    NonZeroU32::new(raw)
        .map(AttemptRecordCeiling::new)
        .ok_or_else(|| "action receipt_ceiling is zero".to_owned())
}

fn identity_bound() -> NonZeroU32 {
    match NonZeroU32::new(4_096) {
        Some(value) => value,
        None => panic!("fixed identity bound must be non-zero"),
    }
}

fn delivery(row: &Value) -> RunResult<DeliveryMode> {
    match required_str(row, "delivery")? {
        "queue" => Ok(DeliveryMode::Queue),
        "steer" => Ok(DeliveryMode::Steer),
        other => Err(format!("unknown delivery {other}")),
    }
}

fn promote_scope(row: &Value) -> RunResult<PromoteScope> {
    match required_str(row, "scope")? {
        "input-at-idle" => Ok(PromoteScope::InputAtIdle),
        "steer" => Ok(PromoteScope::Steer),
        other => Err(format!("unknown promote scope {other}")),
    }
}

fn begin(row: &Value, scenario: &str) -> RunResult<ScenarioState> {
    let session: SessionId = parse_id(row, "session")?;
    let attempt = attempt_id(row, "attempt")?;
    let terminal: EventId = parse_id(row, "terminal")?;
    let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
    store
        .elect_writer(&AggregateId::Session(session.clone()))
        .map_err(|error| error.to_string())?;
    let inbox = Inbox::new(store).map_err(|error| error.to_string())?;
    Ok(ScenarioState {
        name: scenario.to_owned(),
        session,
        attempt,
        terminal,
        inbox,
        next_step: 2,
        provider_call_count: 0,
        recovery_reports: Vec::new(),
        observations: 0,
    })
}

fn admit(row: &Value, state: &mut ScenarioState, retry: bool) -> RunResult<()> {
    let event: EventId = parse_id(row, "event")?;
    let input: MessageId = parse_id(row, "input")?;
    let outcome = state
        .inbox
        .admit(AdmitRequest {
            event_id: &event,
            session: &state.session,
            input: &input,
            text: required_str(row, "text")?,
            delivery: delivery(row)?,
            resume: required_bool(row, "resume")?,
            created_ms: required_i64(row, "created_ms")?,
        })
        .map_err(|error| error.to_string())?;
    match (retry, outcome) {
        (false, AdmitOutcome::Admitted { .. }) | (true, AdmitOutcome::Reconciled { .. }) => Ok(()),
        (false, other) => Err(format!("initial admission did not admit: {other:?}")),
        (true, other) => Err(format!("stable retry did not reconcile: {other:?}")),
    }
}

fn promote(row: &Value, state: &mut ScenarioState) -> RunResult<()> {
    let event: EventId = parse_id(row, "event")?;
    let mut event = Some(event);
    let mut mint = || match event.take() {
        Some(id) => id,
        None => panic!("one-input promotion requested more than one event identity"),
    };
    let promoted = state
        .inbox
        .promote(
            &state.session,
            promote_scope(row)?,
            &mut mint,
            required_i64(row, "created_ms")?,
        )
        .map_err(|error| error.to_string())?;
    if promoted.len() != 1 {
        return Err(format!(
            "scenario {} promoted {} rows instead of one",
            state.name,
            promoted.len()
        ));
    }
    Ok(())
}

fn reserve_attempt(row: &Value, state: &mut ScenarioState) -> RunResult<()> {
    let bound = identity_bound();
    let bounded_attempt = BoundedAttemptId::new(state.attempt.clone(), bound)
        .map_err(|error| format!("attempt identity exceeds bound: {error:?}"))?;
    let bounded_session = BoundedSessionId::new(state.session.clone(), bound)
        .map_err(|error| format!("session identity exceeds bound: {error:?}"))?;
    let step = StepNumber::new(required_u32(row, "step_number")?)
        .ok_or_else(|| "action step_number is zero".to_owned())?;
    let ordinal = AttemptOrdinal::new(required_u32(row, "attempt_ordinal")?)
        .ok_or_else(|| "action attempt_ordinal is zero".to_owned())?;
    let receipt = DurableAttemptReceipt::reserved(
        bounded_attempt,
        bounded_session,
        step,
        ordinal,
        ReservedMarker::new(Timestamp::from_millis(required_i64(row, "reserved_ms")?)),
    );
    let aggregate = AggregateId::Session(state.session.clone());
    let store = state.inbox.store_mut();
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    store
        .reserve_attempt_receipt(&ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &TerminalCoordinates::new(
                state.terminal.clone(),
                required_i64(row, "terminal_created_ms")?,
            ),
            ceiling: ceiling(row)?,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn permit_send(row: &Value, state: &mut ScenarioState) -> RunResult<()> {
    let aggregate = AggregateId::Session(state.session.clone());
    let bound = ceiling(row)?;
    let store = state.inbox.store_mut();
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let transaction = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    EventStore::permit_attempt_send_within(
        &transaction,
        &state.attempt,
        writer,
        bound,
        RequestSentMarker::new(Timestamp::from_millis(required_i64(row, "sent_ms")?)),
    )
    .map_err(|error| error.to_string())?;
    transaction
        .commit()
        .map_err(|error| error.to_string())?
        .discard();
    Ok(())
}

fn recover(row: &Value, state: &mut ScenarioState) -> RunResult<()> {
    let clock = FixedClock(required_i64(row, "now_ms")?);
    // The corpus adapter calls the provider-free recovery owner directly. A
    // semantic mutant that increments provider_call_count here proves the
    // expected no-replay outcome is enforced by execution, not prose.
    let report = recover_process_loss_attempts(state.inbox.store_mut(), &clock, ceiling(row)?)
        .map_err(|error| error.to_string())?;
    state.recovery_reports.push(report);
    Ok(())
}

fn nonnegative(raw: i64, name: &str) -> RunResult<u64> {
    u64::try_from(raw).map_err(|_| format!("{name} is negative"))
}

fn public_history(state: &ScenarioState) -> RunResult<Vec<Value>> {
    let mut rows = state
        .inbox
        .promoted_messages(&state.session, 128)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|message| {
            (
                message.seq,
                json!({
                    "type": "user",
                    "id": message.input.as_str(),
                    "text": message.text,
                    "durable_seq": message.seq
                }),
            )
        })
        .collect::<Vec<_>>();
    let assistants = SessionHistoryStore::over(state.inbox.store())
        .fold(&state.session)
        .map_err(|error| error.to_string())?
        .messages()
        .map_err(|error| error.to_string())?;
    rows.extend(assistants.into_iter().map(|message| {
        let terminal = match message.terminal {
            Some(StepTerminal::Ended { .. }) => Value::String("ended".to_owned()),
            Some(StepTerminal::Failed { .. }) => Value::String("failed".to_owned()),
            None => Value::Null,
        };
        (
            message.opened_at_seq,
            json!({
                "type": "assistant",
                "id": message.message.as_str(),
                "terminal": terminal,
                "durable_seq": message.opened_at_seq
            }),
        )
    }));
    rows.sort_by_key(|(seq, _)| *seq);
    Ok(rows.into_iter().map(|(_, row)| row).collect())
}

fn observe(
    row: &Value,
    state: &mut ScenarioState,
    durable: &mut Vec<Value>,
    public: &mut Vec<Value>,
) -> RunResult<()> {
    let store = state.inbox.store();
    let terminal_count = nonnegative(
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND type = 'session.step.failed.2'",
                [state.session.as_str()],
                |result| result.get(0),
            )
            .map_err(|error| error.to_string())?,
        "terminal count",
    )?;
    // This exact durable count is the resurrection witness. A semantic mutant
    // inflates only later observations and must redden the corpus comparison.
    let ambiguous_terminal_count = nonnegative(
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND type = 'session.step.failed.2' AND data LIKE '%ambiguous%'",
                [state.session.as_str()],
                |result| result.get(0),
            )
            .map_err(|error| error.to_string())?,
        "ambiguous terminal count",
    )?;
    let live_receipt_count = nonnegative(
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM attempt_receipt WHERE attempt_id = ?1",
                [state.attempt.as_str()],
                |result| result.get(0),
            )
            .map_err(|error| error.to_string())?,
        "live receipt count",
    )?;
    let quarantine_count = nonnegative(
        store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM attempt_receipt_quarantine WHERE attempt_id = ?1",
                [state.attempt.as_str()],
                |result| result.get(0),
            )
            .map_err(|error| error.to_string())?,
        "quarantine count",
    )?;
    let report = state
        .recovery_reports
        .last()
        .copied()
        .ok_or_else(|| format!("scenario {} observed before recovery", state.name))?;
    let observation = required_str(row, "observation")?;
    durable.push(json!({
        "scenario": state.name,
        "observation": observation,
        "terminal_count": terminal_count,
        "ambiguous_terminal_count": ambiguous_terminal_count,
        "live_receipt_count": live_receipt_count,
        "quarantine_count": quarantine_count,
        "provider_call_count": state.provider_call_count,
        "recovery_passes": state.recovery_reports.len(),
        "last_recovery": {
            "sessions": report.sessions,
            "settled": report.settled,
            "reclaimed": report.reclaimed,
            "quarantined": report.quarantined
        }
    }));
    public.push(json!({
        "scenario": state.name,
        "observation": observation,
        "public_history": public_history(state)?
    }));
    state.observations = state.observations.saturating_add(1);
    Ok(())
}

fn execute(actions: &[Value]) -> RunResult<RunOutput> {
    let mut scenarios = BTreeMap::<String, ScenarioState>::new();
    let mut durable = Vec::new();
    let mut public = Vec::new();
    for action in actions {
        let scenario = required_str(action, "scenario")?.to_owned();
        let step = required_u64(action, "step")?;
        let operation = required_str(action, "op")?;
        if operation == "begin" {
            if step != 1 {
                return Err(format!(
                    "scenario {scenario} begins at step {step}, not one"
                ));
            }
            if scenarios
                .insert(scenario.clone(), begin(action, &scenario)?)
                .is_some()
            {
                return Err(format!("scenario {scenario} begins twice"));
            }
            continue;
        }
        let state = scenarios
            .get_mut(&scenario)
            .ok_or_else(|| format!("scenario {scenario} acts before begin"))?;
        if step != state.next_step {
            return Err(format!(
                "scenario {scenario} expected step {} but read {step}",
                state.next_step
            ));
        }
        state.next_step = state
            .next_step
            .checked_add(1)
            .ok_or_else(|| format!("scenario {scenario} step overflow"))?;
        match operation {
            "admit_user" => admit(action, state, false)?,
            "promote_user" => promote(action, state)?,
            "provider_request_accepted" => {
                state.provider_call_count = state
                    .provider_call_count
                    .checked_add(1)
                    .ok_or_else(|| format!("scenario {scenario} provider count overflow"))?;
            }
            "reserve_attempt" => reserve_attempt(action, state)?,
            "permit_send" => permit_send(action, state)?,
            "recover_process_loss" => recover(action, state)?,
            "stable_retry" => admit(action, state, true)?,
            "observe" => observe(action, state, &mut durable, &mut public)?,
            other => return Err(format!("scenario {scenario} has unknown operation {other}")),
        }
    }
    for (name, state) in scenarios {
        if state.observations == 0 {
            return Err(format!("scenario {name} has no observation"));
        }
    }
    Ok(RunOutput {
        durable,
        public: json!({"schema_version": 1, "observations": public}),
    })
}

fn pretty(value: &Value) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(text) => text,
        Err(error) => format!("<unrenderable JSON: {error}>"),
    }
}

fn find_observation<'a>(
    rows: &'a [Value],
    scenario: &str,
    observation: &str,
) -> RunResult<&'a Value> {
    rows.iter()
        .find(|row| {
            row.get("scenario").and_then(Value::as_str) == Some(scenario)
                && row.get("observation").and_then(Value::as_str) == Some(observation)
        })
        .ok_or_else(|| format!("missing derived observation {scenario}/{observation}"))
}

#[test]
fn language_neutral_d1_scenarios_execute_and_match_durable_and_public_outcomes() {
    let fixture = corpus();
    let manifest = read_json(&fixture.join("manifest.json"))
        .unwrap_or_else(|error| panic!("D1 manifest refused: {error}"));
    let paths = manifest
        .get("paths")
        .unwrap_or_else(|| panic!("D1 manifest has no paths"));
    let expected_paths = paths
        .get("expected")
        .unwrap_or_else(|| panic!("D1 manifest has no expected paths"));
    let actions_path = required_str(paths, "actions")
        .unwrap_or_else(|error| panic!("D1 manifest refused: {error}"));
    let durable_path = required_str(expected_paths, "durable")
        .unwrap_or_else(|error| panic!("D1 manifest refused: {error}"));
    let state_path = required_str(expected_paths, "state")
        .unwrap_or_else(|error| panic!("D1 manifest refused: {error}"));
    let waivers_path = required_str(expected_paths, "waivers")
        .unwrap_or_else(|error| panic!("D1 manifest refused: {error}"));
    let evidence = manifest["evidence"]
        .as_array()
        .unwrap_or_else(|| panic!("D1 manifest has no explicit witness index"));
    assert_eq!(evidence.len(), 3, "D1 witness index must remain complete");
    for witness in evidence {
        required_str(witness, "path")
            .unwrap_or_else(|error| panic!("D1 witness index refused: {error}"));
        required_str(witness, "test")
            .unwrap_or_else(|error| panic!("D1 witness index refused: {error}"));
    }

    let actions = json_lines(&fixture.join(actions_path))
        .unwrap_or_else(|error| panic!("D1 actions refused: {error}"));
    let actual =
        execute(&actions).unwrap_or_else(|error| panic!("D1 scenario execution refused: {error}"));
    let no_replay = find_observation(
        &actual.durable,
        "request-sent-restart-settlement",
        "after-restart",
    )
    .unwrap_or_else(|error| panic!("D1 no-replay observation refused: {error}"));
    assert_eq!(
        no_replay["provider_call_count"],
        json!(1),
        "D1 semantic counterexample: recovery replayed the provider"
    );
    let resurrection = find_observation(
        &actual.durable,
        "second-restart-no-resurrection",
        "after-second-recovery",
    )
    .unwrap_or_else(|error| panic!("D1 resurrection observation refused: {error}"));
    assert_eq!(
        resurrection["terminal_count"],
        json!(1),
        "D1 resurrection: second recovery changed the exact terminal count"
    );
    let expected_durable = json_lines(&fixture.join(durable_path))
        .unwrap_or_else(|error| panic!("D1 durable outcomes refused: {error}"));
    assert_eq!(
        actual.durable,
        expected_durable,
        "D1 durable/recovery semantics drifted\nactual:\n{}\nexpected:\n{}",
        pretty(&Value::Array(actual.durable.clone())),
        pretty(&Value::Array(expected_durable.clone()))
    );
    let expected_public = read_json(&fixture.join(state_path))
        .unwrap_or_else(|error| panic!("D1 public state refused: {error}"));
    assert_eq!(
        actual.public,
        expected_public,
        "D1 public-history semantics drifted\nactual:\n{}\nexpected:\n{}",
        pretty(&actual.public),
        pretty(&expected_public)
    );
    let waivers = read_json(&fixture.join(waivers_path))
        .unwrap_or_else(|error| panic!("D1 waivers refused: {error}"));
    assert_eq!(waivers["waivers"], json!([]), "D1 carries no waiver");
}

#[test]
fn malformed_d1_action_is_refused_before_any_scenario_executes() {
    let row = json!({"scenario": "missing-op", "step": 1});
    assert_eq!(
        required_str(&row, "op"),
        Err("action has no non-empty op".to_owned())
    );
}
