//! Bounded reads and fenced, deduplicated session-state observations.
use super::*;
use grapt_event::EventKind;
use grapt_session::execution as observations;
use grapt_session::SessionStore;
use grapt_store::{
    AggregateId, AppendInput, Appended, EventStore, FencedWriter, GovernedTransaction,
};
use grapt_types::{DeterministicAuthorities, EventId, SessionId};

const MAX_DATA_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// A newly admitted owner, checked against the live registry by its caller.
    Start,
    /// A phase or wait belonging to the already recorded owner.
    Update,
    /// Details of the terminal the execution lifecycle has already committed.
    Finish,
    /// The recorded owner is gone, after startup or live cancellation cleanup.
    Recover,
}

impl Change {
    fn validate(self, state: &State) -> Result<(), String> {
        let valid_transition = match self {
            Self::Start => {
                state.active()
                    || (state.kind == Kind::Paused && state.reason == Some(Reason::Preparing))
            }
            Self::Update => state.active(),
            Self::Finish => matches!(
                state.kind,
                Kind::Running | Kind::Completed | Kind::Failed | Kind::Paused | Kind::Interrupted
            ),
            Self::Recover => {
                (state.kind == Kind::Disconnected && state.reason == Some(Reason::ProcessLoss))
                    || (state.kind == Kind::Interrupted
                        && matches!(
                            state.reason,
                            Some(Reason::User | Reason::Shutdown | Reason::WindowClosed)
                        ))
            }
        };
        if !valid_transition {
            return Err("session status does not match its owner transition".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    state: State,
}

/// A phase cannot revive a terminal or mutate a successor's state; an old
/// producer receives the current snapshot without appending anything.
pub fn record(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    state: State,
    change: Change,
) -> Result<Snapshot, String> {
    state.validate()?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let snapshot = record_within(&tx, writer, authorities, session, execution, state, change)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(snapshot)
}

pub(crate) fn record_within(
    tx: &GovernedTransaction<'_>,
    writer: FencedWriter,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    state: State,
    change: Change,
) -> Result<Snapshot, String> {
    state.validate()?;
    change.validate(&state)?;
    let prior = read(tx, session)?;
    let same_owner = prior.execution.as_ref() == Some(execution);
    if (matches!(change, Change::Update | Change::Recover)
        && (!same_owner || !prior.state.active()))
        || (change == Change::Finish && !same_owner)
        || (same_owner && prior.state == state)
    {
        return Ok(prior);
    }
    if change == Change::Start
        && same_owner
        && !prior.state.active()
        && (prior.state.kind != Kind::Paused
            || prior.state.reason != Some(Reason::Preparing)
            || !observations::read(tx, session, execution)?.is_empty())
    {
        return Ok(prior);
    }
    let data = serde_json::to_value(Data {
        session: session.as_str().into(),
        execution: execution.as_str().into(),
        state: state.clone(),
    })
    .map_err(|error| error.to_string())?;
    if serde_json::to_vec(&data)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_DATA_BYTES
    {
        return Err("session status exceeds its encoded bound".into());
    }
    let event: EventId = format!("evt_{}", authorities.mint().as_str())
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let created_ms = authorities.now().as_millis();
    if created_ms < 0 {
        return Err("session status time is negative".into());
    }
    let kind = match change {
        Change::Start => EventKind::SessionStatusStartedV1,
        _ => EventKind::SessionStatusChangedV1,
    };
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
        return Err("session status event identity repeated".into());
    };
    Ok(Snapshot {
        position: CommittedPosition {
            event,
            seq,
            created_ms,
        },
        execution: Some(execution.clone()),
        state,
    })
}

/// Decode the owned journal payload at the read boundary.
pub(super) fn decode(
    raw: &str,
    session: &SessionId,
    position: CommittedPosition,
) -> Result<Snapshot, String> {
    let data: Data = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    if data.session != session.as_str() {
        return Err("session status names a different session".into());
    }
    data.state.validate()?;
    if data.state.kind == Kind::Idle {
        return Err("an owned session status cannot be idle".into());
    }
    let execution =
        ExecutionId::new(data.execution).ok_or("session status execution is invalid")?;
    Ok(Snapshot {
        position,
        execution: Some(execution),
        state: data.state,
    })
}
