//! Agent/model selection belongs to the Session aggregate. The read, optional
//! idempotent model comparison, append and list update share one write lock.
use crate::{SessionStore, SessionStoreError};
use grapt_store::{AggregateId, AppendInput, Appended, EventStore, Projector, WriteError};
use grapt_types::{AgentId, DeterministicAuthorities, EventId, EventVersion, SessionId};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

/// Read the last explicit child-model preference from canonical Session events.
/// Absence and an explicit clear both expose no override; unreadable rows fail.
pub fn subagent_model(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<Value>, SessionStoreError> {
    Ok(match subagent_preference(connection, session)? {
        SubagentPreference::Selected(model) => Some(model),
        SubagentPreference::Absent | SubagentPreference::Cleared => None,
    })
}

enum SubagentPreference {
    Absent,
    Cleared,
    Selected(Value),
}

fn subagent_preference(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<SubagentPreference, SessionStoreError> {
    SessionStore::new(connection).record(session)?;
    let store_error = |source| SessionStoreError::Store {
        session: session.as_str().to_owned(),
        source,
    };
    let corrupt = |reason: String| SessionStoreError::Corrupt {
        session: session.as_str().to_owned(),
        reason,
    };
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= 65536 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type IN ('session.subagent-model.selected.1', 'session.subagent-model.cleared.1')
         ORDER BY seq DESC LIMIT 1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(store_error)?;
    let Some((kind, raw)) = row else {
        return Ok(SubagentPreference::Absent);
    };
    let data: Value = serde_json::from_str(&raw).map_err(|error| corrupt(error.to_string()))?;
    if data.get("sessionID").and_then(Value::as_str) != Some(session.as_str()) {
        return Err(corrupt(
            "subagent model event has a different Session identity".into(),
        ));
    }
    if kind == "session.subagent-model.cleared.1" {
        return Ok(SubagentPreference::Cleared);
    }
    let model = data
        .get("model")
        .ok_or_else(|| corrupt("subagent model event omits model".into()))?;
    crate::create::validate_model(model).map_err(|error| corrupt(error.to_string()))?;
    Ok(SubagentPreference::Selected(model.clone()))
}

pub fn select_subagent_model(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    model: Option<&Value>,
) -> Result<Option<u64>, SelectionError> {
    if let Some(model) = model {
        crate::create::validate_model(model)?;
    }
    select(
        store,
        authorities,
        session,
        Selection::SubagentModel(model),
        &mut [],
    )
}

#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error(transparent)]
    Invalid(#[from] crate::SessionCreateError),
    #[error(transparent)]
    Read(#[from] SessionStoreError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("selection event identity failed: {0}")]
    Identity(#[from] grapt_types::IdError),
    #[error("selection event identity repeated")]
    RepeatedEvent,
}

pub fn select_agent(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    agent: &AgentId,
    projectors: &mut [&mut dyn Projector],
) -> Result<(), SelectionError> {
    select_agent_committed(store, authorities, session, agent, projectors).map(|_| ())
}

/// Return the committed aggregate cursor for advisory publication after persistence.
pub fn select_agent_committed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    agent: &AgentId,
    projectors: &mut [&mut dyn Projector],
) -> Result<Option<u64>, SelectionError> {
    crate::create::bounded("agent", agent.as_str(), crate::create::MAX_AGENT_BYTES)?;
    select(
        store,
        authorities,
        session,
        Selection::Agent(agent),
        projectors,
    )
}

pub fn select_model(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    model: &Value,
    projectors: &mut [&mut dyn Projector],
) -> Result<(), SelectionError> {
    select_model_committed(store, authorities, session, model, projectors).map(|_| ())
}

/// Return the committed aggregate cursor, or absence for an unchanged selection.
pub fn select_model_committed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    model: &Value,
    projectors: &mut [&mut dyn Projector],
) -> Result<Option<u64>, SelectionError> {
    crate::create::validate_model(model)?;
    select(
        store,
        authorities,
        session,
        Selection::Model(model),
        projectors,
    )
}

/// The most bytes a preset name may have; the product validates the name.
pub const MAX_PRESET_BYTES: usize = 64;

/// Record the Session's permission preset (its trust level), or clear it
/// with `None`. Absence for an unchanged selection.
pub fn select_preset_committed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    preset: Option<&str>,
    projectors: &mut [&mut dyn Projector],
) -> Result<Option<u64>, SelectionError> {
    if let Some(preset) = preset {
        crate::create::bounded("preset", preset, MAX_PRESET_BYTES)?;
    }
    select(
        store,
        authorities,
        session,
        Selection::Preset(preset),
        projectors,
    )
}

enum Selection<'a> {
    Preset(Option<&'a str>),
    Agent(&'a AgentId),
    Model(&'a Value),
    SubagentModel(Option<&'a Value>),
}

fn select(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    selection: Selection<'_>,
    projectors: &mut [&mut dyn Projector],
) -> Result<Option<u64>, SelectionError> {
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction().map_err(WriteError::from)?;
    let current = SessionStore::new(&tx).record(session)?;
    let (kind_name, data) = match selection {
        Selection::SubagentModel(model) => {
            let previous = subagent_preference(&tx, session)?;
            if match (&previous, model) {
                (SubagentPreference::Selected(old), Some(new)) => same_model(old, new),
                (SubagentPreference::Cleared, None) => true,
                _ => false,
            } {
                return Ok(None);
            }
            match model {
                Some(model) => (
                    "session.subagent-model.selected",
                    json!({"sessionID":session.as_str(), "model":model}),
                ),
                None => (
                    "session.subagent-model.cleared",
                    json!({"sessionID":session.as_str()}),
                ),
            }
        }
        Selection::Preset(preset) => {
            if current.preset.as_deref() == preset {
                return Ok(None);
            }
            (
                "session.preset.selected",
                json!({"sessionID": session.as_str(), "preset": preset}),
            )
        }
        Selection::Agent(agent) => {
            if current.agent.as_deref() == Some(agent.as_str()) {
                return Ok(None);
            }
            (
                "session.agent.selected",
                json!({"sessionID": session.as_str(), "agent": agent.as_str()}),
            )
        }
        Selection::Model(model) => {
            if current
                .model
                .as_ref()
                .is_some_and(|old| same_model(old, model))
            {
                return Ok(None);
            }
            (
                "session.model.selected",
                json!({"sessionID": session.as_str(), "model": model}),
            )
        }
    };
    let event: EventId = format!("evt_{}", authorities.mint().as_str()).parse()?;
    let version = match EventVersion::new(1) {
        Some(value) => value,
        None => unreachable!("one is nonzero"),
    };
    let appended = EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name,
            version,
            created_ms: authorities.now().as_millis(),
            data: &data,
        },
        projectors,
    )?;
    let seq = match appended {
        Appended::Committed { seq } => seq,
        Appended::Idempotent { .. } => return Err(SelectionError::RepeatedEvent),
    };
    tx.commit().map_err(WriteError::from)?.discard();
    Ok(Some(seq))
}

fn same_model(old: &Value, new: &Value) -> bool {
    old.get("providerID") == new.get("providerID")
        && old.get("id") == new.get("id")
        && old
            .get("variant")
            .and_then(Value::as_str)
            .unwrap_or("default")
            == new
                .get("variant")
                .and_then(Value::as_str)
                .unwrap_or("default")
}
