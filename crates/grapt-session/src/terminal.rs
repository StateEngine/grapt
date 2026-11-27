//! Durable observations of native processes; tool-call settlements stay unchanged.
use crate::SessionStore;
use grapt_event::session_terminal::{self, TerminalFact, TerminalState};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{DeterministicAuthorities, EventId, EventVersion, OperationId, SessionId};
use rusqlite::OptionalExtension;

pub fn record(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    fact: &TerminalFact,
) -> Result<(), String> {
    let previous = latest(store, fact.session(), fact.operation())?;
    match (previous.as_ref(), fact.state()) {
        (None, TerminalState::Starting) => {
            SessionStore::over(store)
                .record(fact.session())
                .map_err(|error| error.to_string())?;
        }
        (Some(previous), state)
            if previous.state() == &TerminalState::Starting
                && state != &TerminalState::Starting => {}
        (Some(previous), state) if state != &TerminalState::Starting && previous == fact => {
            return Ok(())
        }
        _ => {
            return Err(
                "terminal observation would create an unadmitted or resurrected command".into(),
            )
        }
    }
    let aggregate = AggregateId::Session(fact.session().clone());
    store
        .elect_writer(&aggregate)
        .map_err(|error| error.to_string())?;
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let event = identity(
        fact.session(),
        fact.operation(),
        fact.state() == &TerminalState::Starting,
    )?;
    let data = session_terminal::encode(fact);
    let (_, notices) = store
        .append(
            AppendInput {
                id: &event,
                aggregate,
                writer,
                kind_name: session_terminal::KIND,
                version: EventVersion::new(1).ok_or("terminal observation version refused")?,
                created_ms: authorities.now().as_millis(),
                data: &data,
            },
            &mut [],
        )
        .map_err(|error| error.to_string())?;
    notices.discard();
    Ok(())
}

/// Admission and final observation have distinct stable event identities. Two
/// primary-key reads find either outcome regardless of the Session's lifetime.
/// The event store still owns fencing, commit and exact-retry reconciliation.
fn identity(
    session: &SessionId,
    operation: &OperationId,
    starting: bool,
) -> Result<EventId, String> {
    if session.as_str().len() > session_terminal::MAX_ID_BYTES
        || operation.as_str().len() > session_terminal::MAX_ID_BYTES
    {
        return Err("terminal observation identity exceeds its bound".into());
    }
    let digest = grapt_event::canonical_hash_hex(&serde_json::json!({
        "sessionID":session.as_str(), "operationID":operation.as_str(), "starting":starting
    }));
    format!("evt_terminal_{digest}")
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())
}

pub fn latest(
    store: &EventStore,
    session: &SessionId,
    operation: &OperationId,
) -> Result<Option<TerminalFact>, String> {
    let starting = identity(session, operation, true)?;
    let terminal = identity(session, operation, false)?;
    let row: Option<(String, bool, bool)> = store
        .connection()
        .query_row(
            "SELECT CASE WHEN length(CAST(data AS BLOB)) <= 131072 THEN data END,
                aggregate_family = 'session' AND aggregate_id = ?3
                    AND type = 'session.terminal.observed.1', id = ?1
             FROM event WHERE id IN (?1, ?2) ORDER BY seq DESC LIMIT 1",
            rusqlite::params![starting.as_str(), terminal.as_str(), session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|error| format!("terminal observation is unreadable: {error}"))?;
    row.map(|(data, belongs, starting)| {
        if !belongs {
            return Err("terminal observation differs from its indexed owner or kind".into());
        }
        let value = serde_json::from_str(&data)
            .map_err(|error| format!("terminal observation is corrupt: {error}"))?;
        let fact = session_terminal::decode(&value).map_err(|error| error.to_string())?;
        if fact.session() != session
            || fact.operation() != operation
            || (fact.state() == &TerminalState::Starting) != starting
        {
            return Err(
                "terminal observation identity or phase differs from its event identity".into(),
            );
        }
        Ok(fact)
    })
    .transpose()
}
