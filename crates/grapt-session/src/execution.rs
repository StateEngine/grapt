//! Durable observations of process-local execution, never ownership or replay
//! instructions. Only the runtime coordinator can answer whether work is active.
use crate::{CommittedPosition, SessionStore};
use grapt_store::{
    AggregateId, AppendInput, Appended, EventStore, FencedWriter, GovernedTransaction,
};
use grapt_types::{DeterministicAuthorities, EventId, ExecutionId, PolicyVersion, SessionId};
use serde::{Deserialize, Serialize};

mod compaction_recovery;
mod continuity;
pub use compaction_recovery::CompactionRecovery;
pub use continuity::{ContinuityFailure, StateEngineServerFailure};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Interrupted,
}

/// The cause recorded by the execution owner before its terminal is published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InterruptionReason {
    User,
    Shutdown,
    Superseded,
    ServeWindowExpired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub position: CommittedPosition,
    pub session: SessionId,
    pub execution: ExecutionId,
    pub outcome: Option<Outcome>,
    pub interruption: Option<InterruptionReason>,
    /// Present exactly when the outcome is failed.
    pub failure: Option<PublicFailure>,
}

/// Public diagnostics deliberately exclude arbitrary provider/transport strings.
pub const FAILURE_MESSAGE: &str =
    "The execution stopped before completing. Inspect the Session before retrying.";

const CONTEXT_LIMIT_MESSAGE: &str = "The execution stopped because its context could not be prepared or reduced for this model. Its work so far is kept in the Session; send a follow-up prompt to retry from its durable history.";
const STEP_BUDGET_PREFIX: &str = "The execution used all ";
const STEP_BUDGET_GRANT: &str = " of ";
const STEP_BUDGET_POLICY: &str = " Steps permitted by ";
const STEP_BUDGET_SUFFIX: &str = " for this request without finishing. Its work so far is kept in the Session; send a follow-up prompt to continue.";
const MAX_FAILURE_MESSAGE_BYTES: usize = 512;

/// Why an execution failed, in the only vocabulary that is published.
///
/// Each class maps to a fixed product-authored name and message, so a typed
/// cause the owner already knows reaches the user without publishing arbitrary
/// provider, transport or runtime text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// No safe typed cause is known.
    Generic,
    /// One request spent its whole Step budget while the model was still working.
    StepBudgetExhausted {
        policy: PolicyVersion,
        spent: u32,
        granted: u32,
    },
    /// Automatic continuation reached its total output-recovery ceiling.
    OutputLimit,
    /// A bounded provider-slot wait or queue limit, safe to retry from history.
    ProviderCapacity,
    /// An exhausted provider allowance requires account action, not backoff.
    ProviderLimit {
        reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
    },
    /// Context preparation or compaction failed for this model.
    ContextLimit,
    /// The provider failed the request. The summary states the status, the
    /// vendor's own machine code and a bounded, already-redacted excerpt of its
    /// message when one was returned, so the reader sees the cause rather than
    /// a generic sentence.
    ///
    /// It is constructed only from the adapter's masked detail, and it is
    /// bounded again here so a future producer cannot publish an unbounded
    /// provider string through this class.
    ProviderFailed(String),
    /// Closed continuity diagnosis; the existing public context-limit class
    /// carries fixed recovery copy, never remote error text.
    Continuity(ContinuityFailure),
    /// Fixed recovery copy through the existing public context-failure path.
    CompactionRecovery(CompactionRecovery),
}

impl Failure {
    pub fn public(&self) -> PublicFailure {
        let (name, message) = match self {
            Self::Generic => ("ExecutionFailed", FAILURE_MESSAGE.to_owned()),
            Self::StepBudgetExhausted {
                policy,
                spent,
                granted,
            } => (
                "StepBudgetExhausted",
                format!(
                    "{STEP_BUDGET_PREFIX}{spent}{STEP_BUDGET_GRANT}{granted}{STEP_BUDGET_POLICY}{}{STEP_BUDGET_SUFFIX}",
                    policy.as_str()
                ),
            ),
            Self::OutputLimit => ("ExecutionFailed", "The response reached the output limit and used its two automatic continuation Steps (at most ten extra physical attempts). Partial output is kept in the Session; send a new input to continue.".to_owned()),
            Self::ProviderCapacity => ("ProviderCapacity", "The provider slot wait reached its bound. This is retryable; work so far is kept in the Session. Send a follow-up prompt, or use followup_task to resume this child.".to_owned()),
            Self::ProviderLimit { reset } => ("ExecutionFailed", provider_limit_message(*reset)),
            Self::ContextLimit => ("ContextLimit", CONTEXT_LIMIT_MESSAGE.to_owned()),
            // The message carries the cause; the public name stays what clients
            // already parse, so no wire vocabulary changes here.
            Self::ProviderFailed(summary) => ("ExecutionFailed", bounded_refusal(summary)),
            Self::Continuity(failure) => ("ContextLimit", failure.message()),
            Self::CompactionRecovery(cause) => ("ContextLimit", cause.message()),
        };
        PublicFailure {
            name: name.to_owned(),
            message,
        }
    }
}

/// A provider summary as the row may show it: control characters become
/// spaces so the row stays one line, and the text is capped on a character
/// boundary at the durable message bound. The adapter has already masked
/// secrets, so this adds no trust — only a display shape the row can carry.
fn bounded_refusal(summary: &str) -> String {
    let mut out: String = summary
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if out.len() > MAX_FAILURE_MESSAGE_BYTES {
        let mut cut = MAX_FAILURE_MESSAGE_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

pub(crate) fn provider_limit_message(
    reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
) -> String {
    let mut message = "The provider usage limit was reached.".to_owned();
    if let Some(reset) = reset {
        message.push_str(&format!(" The provider reports a reset at {reset}."));
    }
    message
        .push_str(" Switch accounts: sign in with another account to continue from this Session.");
    message
}

/// A recorded public failure, exactly as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicFailure {
    pub name: String,
    pub message: String,
}

impl PublicFailure {
    /// Whether the stored name is one of this build's own public failure
    /// vocabulary, so the product may render it with the wording it owns.
    /// A row carrying any other name — whatever build wrote it — is still
    /// projected, and the display layer says so by rendering the stored
    /// name instead of hiding it.
    pub fn is_known_name(&self) -> bool {
        matches!(
            self.name.as_str(),
            "ExecutionFailed" | "ContextLimit" | "StepBudgetExhausted" | "ProviderCapacity"
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<StoredFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<InterruptionReason>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFailure {
    #[serde(rename = "type")]
    name: String,
    message: String,
}

/// Structural integrity for a stored public failure, and nothing more.
///
/// The NAME and the MESSAGE wording are the future's vocabulary: a later
/// build may rename a class or reword its message, and this reader must
/// still project the durable row rather than refusing it. What is
/// corruption, and is refused, is the shape: a blank name, a blank message,
/// or a message past its bound. The known vocabulary stays published by
/// [`Failure::public`]; this classifier only decides what is a row at all.
fn valid_failure(error: &StoredFailure) -> bool {
    !error.name.is_empty()
        && !error.message.is_empty()
        && error.message.len() <= MAX_FAILURE_MESSAGE_BYTES
}

/// One execution lifecycle fact, without an impossible outcome/reason pair.
#[derive(Debug)]
pub enum Change {
    Start,
    Succeeded,
    Failed(Failure),
    Interrupted(InterruptionReason),
}

/// A start is not a lease. Reopening this record cannot resume a provider call.
pub fn begin(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<Observation, String> {
    Change::Start.record(store, authorities, session, execution)
}

/// The terminal describes the execution owner, not a fabricated Step outcome.
/// An identical repeated settlement is an idempotent read; a different one refuses.
pub fn settle(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    outcome: Outcome,
) -> Result<Observation, String> {
    let change = match outcome {
        Outcome::Succeeded => Change::Succeeded,
        Outcome::Failed => Change::Failed(Failure::Generic),
        Outcome::Interrupted => Change::Interrupted(InterruptionReason::User),
    };
    change.record(store, authorities, session, execution)
}

/// Settle a failed execution with its typed public class. A repeated
/// settlement returns the recorded terminal; it cannot relabel history.
pub fn settle_failed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    failure: Failure,
) -> Result<Observation, String> {
    Change::Failed(failure).record(store, authorities, session, execution)
}

impl Change {
    pub fn record(
        self,
        store: &mut EventStore,
        authorities: &DeterministicAuthorities<'_>,
        session: &SessionId,
        execution: &ExecutionId,
    ) -> Result<Observation, String> {
        // Consume the already elected writer; observations cannot elect ownership.
        let writer = store
            .fencing_token(&AggregateId::Session(session.clone()))
            .map_err(|error| error.to_string())?;
        let tx = store
            .governed_transaction()
            .map_err(|error| error.to_string())?;
        let observed = self.append_within(&tx, writer, authorities, session, execution)?;
        tx.commit().map_err(|error| error.to_string())?.discard();
        Ok(observed)
    }

    /// Append one typed lifecycle fact in the store-owned unit of work.
    /// The caller commits it together with any derived service observations.
    pub fn append_within(
        self,
        tx: &GovernedTransaction<'_>,
        writer: FencedWriter,
        authorities: &DeterministicAuthorities<'_>,
        session: &SessionId,
        execution: &ExecutionId,
    ) -> Result<Observation, String> {
        let (outcome, failure, interruption) = match self {
            Change::Start => (None, None, None),
            Change::Succeeded => (Some(Outcome::Succeeded), None, None),
            Change::Failed(failure) => (Some(Outcome::Failed), Some(failure.public()), None),
            Change::Interrupted(reason) => (Some(Outcome::Interrupted), None, Some(reason)),
        };
        SessionStore::new(tx)
            .record(session)
            .map_err(|error| error.to_string())?;
        let prior = read(tx, session, execution)?;
        match (outcome, prior.as_slice()) {
            (None, []) | (Some(_), [Observation { outcome: None, .. }]) => {}
            (Some(expected), [_, terminal]) if terminal.outcome == Some(expected) => {
                return Ok(terminal.clone())
            }
            _ => {
                return Err(
                    "execution observation would duplicate, skip, or resurrect its lifecycle"
                        .into(),
                )
            }
        }
        let kind = kind(outcome);
        let data = serde_json::to_value(Data {
            session: session.as_str().to_owned(),
            execution: execution.as_str().to_owned(),
            error: failure.as_ref().map(|failure| StoredFailure {
                name: failure.name.clone(),
                message: failure.message.clone(),
            }),
            reason: interruption,
        })
        .map_err(|error| error.to_string())?;
        let event: EventId = format!("evt_{}", authorities.mint().as_str())
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let created_ms = authorities.now().as_millis();
        if created_ms < 0 {
            return Err("execution observation time is negative".into());
        }
        let appended = EventStore::append_within(
            tx,
            AppendInput {
                id: &event,
                aggregate: AggregateId::Session(session.clone()),
                writer,
                kind_name: kind.name(),
                version: kind.version(),
                created_ms,
                data: &data,
            },
            &mut [],
        )
        .map_err(|error| error.to_string())?;
        let Appended::Committed { seq } = appended else {
            return Err("execution observation event identity repeated".into());
        };
        Ok(Observation {
            position: CommittedPosition {
                event,
                seq,
                created_ms,
            },
            session: session.clone(),
            execution: execution.clone(),
            outcome,
            interruption,
            failure,
        })
    }
}

fn kind(outcome: Option<Outcome>) -> grapt_event::EventKind {
    match outcome {
        None => grapt_event::EventKind::SessionExecutionStartedV1,
        Some(Outcome::Succeeded) => grapt_event::EventKind::SessionExecutionSucceededV1,
        Some(Outcome::Failed) => grapt_event::EventKind::SessionExecutionFailedV1,
        Some(Outcome::Interrupted) => grapt_event::EventKind::SessionExecutionInterruptedV1,
    }
}

#[cfg(test)]
mod read_tests;

const READ_SQL: &str = "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END, seq, created,
         type, CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type IN ('session.execution.started.1', 'session.execution.succeeded.1',
          'session.execution.failed.1', 'session.execution.interrupted.1')
         AND json_extract(data, '$.executionID') = ?2 ORDER BY +seq LIMIT 3";

pub fn read(
    connection: &rusqlite::Connection,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<Vec<Observation>, String> {
    read_ordered(connection, session, execution, READ_SQL, true)
}

fn read_ordered(
    connection: &rusqlite::Connection,
    session: &SessionId,
    execution: &ExecutionId,
    sql: &str,
    recover_json_order: bool,
) -> Result<Vec<Observation>, String> {
    let mut query = connection.prepare(sql).map_err(|error| error.to_string())?;
    let mut rows = query
        .query((session.as_str(), execution.as_str()))
        .map_err(|error| error.to_string())?;
    let mut observations = Vec::new();
    while let Some(row) = match rows.next() {
        Ok(row) => row,
        Err(rusqlite::Error::SqliteFailure(_, Some(detail)))
            if recover_json_order && detail == "malformed JSON" =>
        {
            // Sorting owned kinds can inspect JSON after the third observation.
            // Replay the original sequence-index walk only on that SQLite JSON
            // failure, preserving which corruption the streaming decoder reports.
            // Other read failures propagate; this path makes at most two queries.
            let sequence_order = READ_SQL.replacen("ORDER BY +seq", "ORDER BY seq", 1);
            return read_ordered(connection, session, execution, &sequence_order, false);
        }
        Err(error) => return Err(error.to_string()),
    } {
        if observations.len() == 2 {
            return Err("execution has more than one terminal observation".into());
        }
        let id: String = row.get(0).map_err(|error| error.to_string())?;
        let seq: i64 = row.get(1).map_err(|error| error.to_string())?;
        let seq = u64::try_from(seq).map_err(|error| error.to_string())?;
        let created_ms: i64 = row.get(2).map_err(|error| error.to_string())?;
        let event_kind: String = row.get(3).map_err(|error| error.to_string())?;
        let raw: String = row.get(4).map_err(|error| error.to_string())?;
        let data: Data = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        let outcome = match event_kind.as_str() {
            "session.execution.started.1" => None,
            "session.execution.succeeded.1" => Some(Outcome::Succeeded),
            "session.execution.failed.1" => Some(Outcome::Failed),
            "session.execution.interrupted.1" => Some(Outcome::Interrupted),
            _ => return Err("execution observation kind is invalid".into()),
        };
        if data.session != session.as_str()
            || data.execution != execution.as_str()
            || created_ms < 0
            || (observations.is_empty() != outcome.is_none())
            || (data.error.is_some() != (outcome == Some(Outcome::Failed)))
            || (data.reason.is_some() != (outcome == Some(Outcome::Interrupted)))
        {
            return Err("execution observation identity, order, or payload is invalid".into());
        }
        if data
            .error
            .as_ref()
            .is_some_and(|error| !valid_failure(error))
        {
            return Err("execution failure or interruption is malformed".into());
        }
        observations.push(Observation {
            position: CommittedPosition {
                event: id
                    .parse()
                    .map_err(|error: grapt_types::IdError| error.to_string())?,
                seq,
                created_ms,
            },
            session: session.clone(),
            execution: execution.clone(),
            outcome,
            interruption: data.reason,
            failure: data.error.map(|error| PublicFailure {
                name: error.name,
                message: error.message,
            }),
        });
    }
    Ok(observations)
}

/// Bounded durable failures for transcript readers, including executions that
/// failed before an assistant message existed. Every row reuses the lifecycle
/// validator; malformed or resurrected observations cannot disappear as empty.
pub fn failures(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<Observation>, String> {
    failures_at(connection, session, None)
}

/// Validate one selected failure through the same lifecycle decoder as transcript reads.
pub fn failure_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    at: grapt_types::EventSeq,
) -> Result<Option<Observation>, String> {
    Ok(failures_at(connection, session, Some(at.value()))?.pop())
}

fn failures_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    at: Option<u64>,
) -> Result<Vec<Observation>, String> {
    let limit: usize = if at.is_some() { 1 } else { 128 };
    let at = at
        .map(i64::try_from)
        .transpose()
        .map_err(|error| error.to_string())?;
    let visible = grapt_store::history_visibility::HistoryVisibility::read(connection, session)
        .map_err(|error| error.to_string())?
        .sql("seq");
    let mut query = connection
        .prepare(&format!(
            "SELECT CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.execution.failed.1' AND {visible}
         AND (?2 IS NULL OR seq = ?2) ORDER BY seq LIMIT ?3"
        ))
        .map_err(|error| error.to_string())?;
    let probe = i64::try_from(limit + 1).map_err(|error| error.to_string())?;
    let mut rows = query
        .query(rusqlite::params![session.as_str(), at, probe])
        .map_err(|error| error.to_string())?;
    let mut failures = Vec::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        if failures.len() == limit {
            return Err("execution failure history exceeds its bound".into());
        }
        let raw: String = row.get(0).map_err(|error| error.to_string())?;
        let data: Data = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        if data.session != session.as_str() {
            return Err("execution failure belongs to another Session".into());
        }
        let execution = ExecutionId::new(data.execution).ok_or("invalid failed execution id")?;
        let history = read(connection, session, &execution)?;
        let terminal = history
            .get(1)
            .filter(|held| held.outcome == Some(Outcome::Failed))
            .ok_or("execution failure has no validated terminal")?;
        failures.push(terminal.clone());
    }
    Ok(failures)
}
