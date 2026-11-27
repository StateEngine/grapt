//! Durable fork and delete operations over the existing Session aggregate.

use std::collections::HashMap;

use grapt_session_history::history::OWNED_KINDS;
use grapt_store::{AggregateId, AppendInput, Appended, EventStore, Projector, WriteError};
use grapt_types::{
    DeterministicAuthorities, EventId, EventSeq, EventVersion, LocationRef, MessageId, SessionId,
    ToolCallId,
};
use serde_json::{json, Value};

use crate::{SessionListProjector, SessionStore};

const MAX_FORK_ROWS: usize = crate::MAX_HISTORY_ROWS + 512;
const MAX_FORK_ROW_BYTES: usize = 1024 * 1024;
const MAX_FORK_DATA_BYTES: usize = 16 * 1024 * 1024;

mod fork_mail;
mod fork_source;

/// The conversation a fork carries, by kind NAME at every version: the
/// history fold's own kinds, the inputs the inbox projects, and the
/// selections. Pinning versions here once dropped every row written at a
/// newer version, and the fork silently lost its conversation.
const FORK_KINDS: &[&str] = &[
    "session.input.admitted",
    "session.input.promoted",
    "session.subagent-mail.presented",
];

// Rewind removes conversation, not the Session's operational selections.
const FORK_SELECTION_KINDS: &[&str] = &[
    "session.agent.selected",
    "session.model.selected",
    "session.preset.selected",
];

/// The tool history a fork carries, by kind NAME at every version: the
/// identity manifest each copied tool call declared before its effect, and
/// the durable terminal outcome of every call it executed. Without them the
/// copied conversation asserts successful calls whose results it does not
/// carry, and the continuation refuses the fork's first turn on the missing
/// outcome. The nested-call classification travels with them: readers tell a
/// nested terminal from a top-level one only by that row in the same
/// aggregate, so without it the fork hands the model nested results as calls
/// it never made.
/// Permissions, execution rows and queued work are never carried: the fork
/// inherits what its history claims, not the parent's live authority.
const FORK_TOOL_KINDS: &[&str] = &[
    "session.tool.input.started",
    "session.tool.nested.started",
    "session.tool.terminal",
];

#[derive(Debug, thiserror::Error)]
pub enum SessionLifecycleError {
    #[error(transparent)]
    Visibility(#[from] grapt_store::history_visibility::VisibilityError),
    #[error("session lifecycle identity is invalid")]
    Identity,
    #[error("session lifecycle read failed: {0}")]
    Read(#[from] rusqlite::Error),
    #[error("session lifecycle state is corrupt: {0}")]
    Corrupt(String),
    #[error("session lifecycle write failed: {0}")]
    Write(#[from] WriteError),
    #[error("session lifecycle event identity repeated")]
    RepeatedEvent,
    #[error("fork source has more than {MAX_FORK_ROWS} retained rows")]
    ForkTooLong,
    #[error("fork source exceeds its retained data bound")]
    ForkDataTooLong,
    #[error("fork cutoff is not a visible event in this session")]
    InvalidCutoff,
    #[error(transparent)]
    PromptSelection(#[from] crate::rewind::RewindError),
    #[error("fork cutoff contains an unfinished assistant message or tool call")]
    UnfinishedCutoff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionForked {
    pub session: SessionId,
    pub parent: SessionId,
    pub copied_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDeleted {
    pub session: SessionId,
}

enum ForkThrough {
    Latest,
    BeforeStep(i64),
    Selected(EventSeq),
}

struct CopiedRow {
    seq: i64,
    kind: String,
    version: EventVersion,
    created_ms: i64,
    data: Value,
    event: EventId,
}

/// Fork `parent` as the frozen oracle `Session.fork` does: a
/// `session.forked` v2 boundary naming the parent sequence the fork inherits
/// through (instructions are read through that lineage), and
/// the parent's conversation up to it under freshly minted message ids,
/// projected by `projectors` exactly as the parent's rows were.
pub fn fork_session(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    projectors: &mut [&mut dyn Projector],
) -> Result<SessionForked, SessionLifecycleError> {
    fork_at(
        store,
        authorities,
        parent,
        ForkThrough::Latest,
        None,
        projectors,
    )
}

/// Fork through an exact, inclusive visible event sequence. The prefix must
/// contain only settled assistant messages and local tool calls. Later parent
/// work and queued inputs are excluded; the parent's current selections remain.
pub fn fork_session_at_seq(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    through: EventSeq,
    projectors: &mut [&mut dyn Projector],
) -> Result<SessionForked, SessionLifecycleError> {
    fork_at(
        store,
        authorities,
        parent,
        ForkThrough::Selected(through),
        None,
        projectors,
    )
}

/// Fork before a surviving initial user prompt. Reuses rewind's bounded
/// selector, without changing the parent or inheriting the selected prompt.
pub fn fork_session_before_prompt(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    before: &MessageId,
    projectors: &mut [&mut dyn Projector],
) -> Result<SessionForked, SessionLifecycleError> {
    let selected = crate::rewind::select(store, parent, before)?;
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), parent)?
            .sql("seq");
    let boundary = i64::try_from(selected.boundary().value())
        .map_err(|_| SessionLifecycleError::InvalidCutoff)?;
    let through: Option<i64> = store.connection().query_row(&format!(
        "SELECT MAX(seq) FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND seq<?2 AND {visible}"),
        rusqlite::params![parent.as_str(), boundary], |row| row.get(0))?;
    let through = through.ok_or(SessionLifecycleError::InvalidCutoff)?;
    let through = u64::try_from(through).map_err(|_| SessionLifecycleError::InvalidCutoff)?;
    fork_session_at_seq(
        store,
        authorities,
        parent,
        EventSeq::new(through),
        projectors,
    )
}

/// Fork the visible conversation before the assistant Step that invoked `call`.
/// The child receives the current instruction without inheriting an unfinished
/// tool call or any work subsequently admitted to its parent.
pub fn fork_session_before_call(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    call: &ToolCallId,
    projectors: &mut [&mut dyn Projector],
) -> Result<SessionForked, SessionLifecycleError> {
    let through = fork_source::before_call(store, parent, call)?;
    fork_at(
        store,
        authorities,
        parent,
        ForkThrough::BeforeStep(through),
        None,
        projectors,
    )
}

/// Fork a spawning Step's history into the admission owner's validated location.
/// The parent route and project remain inherited; no later placement mutation
/// or second creation path is needed for a prepared child worktree.
pub fn fork_session_before_call_at_location(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    call: &ToolCallId,
    placement: &LocationRef,
    projectors: &mut [&mut dyn Projector],
) -> Result<SessionForked, SessionLifecycleError> {
    if placement.directory().len() > 65536
        || placement
            .workspace()
            .is_some_and(|workspace| workspace.as_str().len() > 65536)
    {
        return Err(SessionLifecycleError::ForkDataTooLong);
    }
    let through = fork_source::before_call(store, parent, call)?;
    fork_at(
        store,
        authorities,
        parent,
        ForkThrough::BeforeStep(through),
        Some(placement),
        projectors,
    )
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn fork_at(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    through: ForkThrough,
    placement: Option<&LocationRef>,
    projectors: &mut [&mut dyn Projector],
) -> Result<SessionForked, SessionLifecycleError> {
    fork_source::check_record(store, parent)?;
    let seed = parent_seed(store, parent)?;
    SessionStore::over(store)
        .record(parent)
        .map_err(|error| SessionLifecycleError::Corrupt(error.to_string()))?;
    let selected = matches!(through, ForkThrough::Selected(_));
    let parent_seq = match through {
        ForkThrough::Selected(seq) => fork_source::cutoff(store, parent, seq)?,
        ForkThrough::BeforeStep(seq) => seq,
        ForkThrough::Latest => parent_seq(store, parent)?,
    };
    let mut rows = fork_rows(store, authorities, parent, parent_seq)?;
    rows.retain(promoted_inputs(&rows));
    if selected {
        fork_source::require_settled(parent, &rows)?;
    }
    let mut rows = fork_mail::materialize(store, authorities, parent, rows)?;
    let session = minted_session(authorities)?;
    let created_ms = authorities.now().as_millis();
    let mut info = seed
        .get("info")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| SessionLifecycleError::Corrupt("parent seed info is absent".to_owned()))?;
    if let Some(placement) = placement {
        info.insert("directory".to_owned(), json!(placement.directory()));
        if let Some(workspace) = placement.workspace() {
            info.insert("workspaceID".to_owned(), json!(workspace.as_str()));
        } else {
            info.remove("workspaceID");
        }
    }
    info.insert("id".to_owned(), json!(session.as_str()));
    info.insert("parentID".to_owned(), json!(parent.as_str()));
    info.insert(
        "time".to_owned(),
        json!({"created": created_ms, "updated": created_ms}),
    );
    // Frozen `SessionEvent.Forked` v2: `parentSeq` caps the parent history the
    // fork's inherited instructions. Conversation is copied below.
    let fork_data = json!({
        "sessionID": session.as_str(),
        "parentID": parent.as_str(),
        "parentSeq": parent_seq,
    });
    let seed_data = json!({"sessionID": session.as_str(), "info": info});
    if serde_json::to_vec(&seed_data)
        .map_err(|error| SessionLifecycleError::Corrupt(error.to_string()))?
        .len()
        > 65536
    {
        return Err(SessionLifecycleError::ForkDataTooLong);
    }
    let messages = minted_messages(&rows, authorities)?;
    for row in &mut rows {
        replace_session(&mut row.data, parent.as_str(), session.as_str());
        replace_messages(&mut row.data, &messages);
    }
    let fork_event = minted_event(authorities)?;
    let seed_event = minted_event(authorities)?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction()?;
    let mut projector = SessionListProjector;
    committed(EventStore::append_within(
        &tx,
        AppendInput {
            id: &fork_event,
            aggregate: aggregate.clone(),
            writer,
            kind_name: "session.forked",
            version: version(2)?,
            created_ms,
            data: &fork_data,
        },
        &mut [&mut projector],
    )?)?;
    committed(EventStore::append_within(
        &tx,
        AppendInput {
            id: &seed_event,
            aggregate: aggregate.clone(),
            writer,
            kind_name: "session.created",
            version: version(1)?,
            created_ms,
            data: &seed_data,
        },
        &mut [],
    )?)?;
    for row in &rows {
        committed(EventStore::append_within(
            &tx,
            AppendInput {
                id: &row.event,
                aggregate: aggregate.clone(),
                writer,
                kind_name: &row.kind,
                version: row.version,
                created_ms: row.created_ms,
                data: &row.data,
            },
            projectors,
        )?)?;
    }
    tx.commit()?.discard();
    Ok(SessionForked {
        session,
        parent: parent.clone(),
        copied_rows: rows.len(),
    })
}

pub fn delete_session(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
) -> Result<SessionDeleted, SessionLifecycleError> {
    SessionStore::over(store)
        .record(session)
        .map_err(|error| SessionLifecycleError::Corrupt(error.to_string()))?;
    let event = minted_event(authorities)?;
    let data = json!({"sessionID": session.as_str()});
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction()?;
    let mut projector = SessionListProjector;
    committed(EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.deleted",
            version: version(2)?,
            created_ms: authorities.now().as_millis(),
            data: &data,
        },
        &mut [&mut projector],
    )?)?;
    tx.commit()?.discard();
    Ok(SessionDeleted {
        session: session.clone(),
    })
}

fn parent_seed(store: &EventStore, parent: &SessionId) -> Result<Value, SessionLifecycleError> {
    let raw: Option<String> = store.connection().query_row(
        "SELECT CASE WHEN length(CAST(data AS BLOB)) <= 65536 THEN data END FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND type = 'session.created.1' ORDER BY seq LIMIT 1",
        [parent.as_str()],
        |row| row.get(0),
    )?;
    let raw = raw.ok_or(SessionLifecycleError::ForkDataTooLong)?;
    serde_json::from_str(&raw)
        .map_err(|error| SessionLifecycleError::Corrupt(format!("parent seed JSON: {error}")))
}

/// The parent's latest sequence: the boundary through which the fork inherits.
fn parent_seq(store: &EventStore, parent: &SessionId) -> Result<i64, SessionLifecycleError> {
    Ok(store.connection().query_row(
        "SELECT MAX(seq) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [parent.as_str()],
        |row| row.get(0),
    )?)
}

fn fork_rows(
    store: &EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    through: i64,
) -> Result<Vec<CopiedRow>, SessionLifecycleError> {
    let kinds: Vec<&str> = OWNED_KINDS
        .iter()
        .chain(FORK_KINDS)
        .chain(FORK_SELECTION_KINDS)
        .chain(FORK_TOOL_KINDS)
        .copied()
        .collect();
    let kinds = serde_json::to_string(&kinds)
        .map_err(|error| SessionLifecycleError::Corrupt(format!("fork kinds: {error}")))?;
    let selections = serde_json::to_string(FORK_SELECTION_KINDS)
        .map_err(|error| SessionLifecycleError::Corrupt(format!("fork selections: {error}")))?;
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), parent)?
            .sql("seq");
    // `rtrim` strips the numeric version, leaving `name.`; the kind matches by
    // name at any version.
    let mut statement = store.connection().prepare(&format!(
        "SELECT seq, created, CASE WHEN length(CAST(type AS BLOB)) <= 256 THEN type END,
                CASE WHEN length(CAST(data AS BLOB)) <= ?6 THEN data END FROM event
         WHERE aggregate_family = 'session' AND aggregate_id = ?1
           AND rtrim(type, '0123456789') IN (SELECT value || '.' FROM json_each(?3))
           AND (rtrim(type, '0123456789') IN (SELECT value || '.' FROM json_each(?4)) OR (seq <= ?5 AND {visible}))
         ORDER BY seq LIMIT ?2",
    ))?;
    let probe = i64::try_from(MAX_FORK_ROWS.saturating_add(1)).unwrap_or(i64::MAX);
    let mapped = statement.query_map(
        rusqlite::params![
            parent.as_str(),
            probe,
            kinds,
            selections,
            through,
            i64::try_from(MAX_FORK_ROW_BYTES)
                .map_err(|_| SessionLifecycleError::ForkDataTooLong)?
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        },
    )?;
    let mut rows = Vec::new();
    let mut data_bytes = 0usize;
    for row in mapped {
        let (seq, created_ms, versioned, raw) = row?;
        let versioned = versioned.ok_or_else(|| {
            SessionLifecycleError::Corrupt("fork event type exceeds its read bound".into())
        })?;
        let raw = raw.ok_or(SessionLifecycleError::ForkDataTooLong)?;
        data_bytes = data_bytes.saturating_add(raw.len());
        if data_bytes > MAX_FORK_DATA_BYTES {
            return Err(SessionLifecycleError::ForkDataTooLong);
        }
        if rows.len() >= MAX_FORK_ROWS {
            return Err(SessionLifecycleError::ForkTooLong);
        }
        let (kind, raw_version) = versioned
            .rsplit_once('.')
            .ok_or_else(|| SessionLifecycleError::Corrupt(format!("fork type {versioned}")))?;
        let raw_version = raw_version
            .parse::<u32>()
            .map_err(|_| SessionLifecycleError::Corrupt(format!("fork type {versioned}")))?;
        rows.push(CopiedRow {
            seq,
            kind: kind.to_owned(),
            version: version(raw_version)?,
            created_ms,
            data: serde_json::from_str(&raw).map_err(|error| {
                SessionLifecycleError::Corrupt(format!("fork row {versioned}: {error}"))
            })?,
            event: minted_event(authorities)?,
        });
    }
    Ok(rows)
}

/// Keeps every row except an input admitted but not yet promoted by the fork
/// point. The fork carries the parent's conversation, never its queue: the
/// oracle's `projectFork` copies message rows, which exist only once an input
/// is promoted. A queued input copied into the fork would execute the parent's
/// prompt a second time.
fn promoted_inputs(rows: &[CopiedRow]) -> impl FnMut(&CopiedRow) -> bool {
    let promoted: std::collections::HashSet<String> = rows
        .iter()
        .filter(|row| row.kind == "session.input.promoted")
        .filter_map(|row| row.data.get("inputID").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    move |row| {
        row.kind != "session.input.admitted"
            || (!matches!(
                row.data.pointer("/input/type").and_then(Value::as_str),
                Some("operation-delivery" | "agent-message")
            ) && row
                .data
                .get("inputID")
                .and_then(Value::as_str)
                .is_some_and(|input| promoted.contains(input)))
    }
}

/// A fresh identity for every message the fork copies: its inputs and its
/// assistant messages. Message identities are global, so the parent's cannot
/// be reused, just as the oracle fork re-mints each copied message.
fn minted_messages(
    rows: &[CopiedRow],
    authorities: &DeterministicAuthorities<'_>,
) -> Result<HashMap<String, String>, SessionLifecycleError> {
    let mut messages = HashMap::new();
    for row in rows {
        let key = match row.kind.as_str() {
            "session.input.admitted" => "inputID",
            "session.assistant-message.opened" => "message",
            _ => continue,
        };
        let parent = row.data.get(key).and_then(Value::as_str).ok_or_else(|| {
            SessionLifecycleError::Corrupt(format!("fork {} has no {key}", row.kind))
        })?;
        let minted: MessageId = format!("{}_{}", MessageId::PREFIX, authorities.mint().as_str())
            .parse()
            .map_err(|_| SessionLifecycleError::Identity)?;
        messages.insert(parent.to_owned(), minted.as_str().to_owned());
    }
    Ok(messages)
}

/// Rewrite envelope identities only. User text, model names, metadata and
/// artifact contents may quote an old identity and must remain byte-exact.
fn replace_messages(value: &mut Value, messages: &HashMap<String, String>) {
    for field in ["inputID", "message", "assistantMessageID"] {
        if let Some(Value::String(text)) = value.get_mut(field) {
            if let Some(minted) = messages.get(text.as_str()) {
                *text = minted.clone();
            }
        }
    }
}

fn replace_session(value: &mut Value, parent: &str, child: &str) {
    if let Some(value) = value.get_mut("sessionID") {
        if value.as_str() == Some(parent) {
            *value = json!(child);
        }
    }
}

fn minted_session(
    authorities: &DeterministicAuthorities<'_>,
) -> Result<SessionId, SessionLifecycleError> {
    format!("{}_{}", SessionId::PREFIX, authorities.mint().as_str())
        .parse()
        .map_err(|_| SessionLifecycleError::Identity)
}

fn minted_event(
    authorities: &DeterministicAuthorities<'_>,
) -> Result<EventId, SessionLifecycleError> {
    format!("{}_{}", EventId::PREFIX, authorities.mint().as_str())
        .parse()
        .map_err(|_| SessionLifecycleError::Identity)
}

fn version(value: u32) -> Result<EventVersion, SessionLifecycleError> {
    EventVersion::new(value).ok_or(SessionLifecycleError::Identity)
}

fn committed(value: Appended) -> Result<(), SessionLifecycleError> {
    match value {
        Appended::Committed { .. } => Ok(()),
        Appended::Idempotent { .. } => Err(SessionLifecycleError::RepeatedEvent),
    }
}

#[cfg(test)]
#[path = "lifecycle/tests.rs"]
mod tests;
