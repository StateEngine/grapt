// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: resolve the parent spawn-call cutoff through the
// existing Session owner; see THIRD_PARTY_NOTICES.md.
//! Resolve a closed conversation prefix from the canonical tool manifest.

use super::*;
use grapt_event::{session_message, session_tool_identity};
use grapt_session_history::{HistoryRow, MessageHistory};

/// `SessionStore::record` folds these lifecycle kinds before the fork copies
/// conversation. Bound that prerequisite read as well as the copy itself.
pub(super) fn check_record(
    store: &EventStore,
    parent: &SessionId,
) -> Result<(), SessionLifecycleError> {
    let (rows, bytes, largest): (i64, i64, i64) = store.connection().query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(CAST(data AS BLOB))),0),
                COALESCE(MAX(length(CAST(data AS BLOB))),0)
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
           AND type IN ('session.created.1','session.renamed.1',
                        'session.agent.selected.1','session.model.selected.1',
                        'session.preset.selected.1','session.launched.1')",
        [parent.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if u64::try_from(rows).map_or(true, |rows| rows > MAX_FORK_ROWS as u64) {
        return Err(SessionLifecycleError::ForkTooLong);
    }
    if u64::try_from(bytes).map_or(true, |bytes| bytes > MAX_FORK_DATA_BYTES as u64)
        || u64::try_from(largest).map_or(true, |bytes| bytes > MAX_FORK_ROW_BYTES as u64)
    {
        return Err(SessionLifecycleError::ForkDataTooLong);
    }
    Ok(())
}

pub(super) fn before_call(
    store: &EventStore,
    parent: &SessionId,
    call: &ToolCallId,
) -> Result<i64, SessionLifecycleError> {
    if call.as_str().len() > 512 {
        return Err(corrupt("tool call identity exceeds its bound"));
    }
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), parent)?
            .sql("seq");
    let (manifest_seq, version, data) = exact(
        store,
        parent,
        "session.tool.input.started",
        "callID",
        call.as_str(),
        &visible,
    )?;
    let identity = session_tool_identity::decode(version, &data)
        .map_err(|error| corrupt(error.to_string()))?;
    if identity.session != *parent || identity.call != *call {
        return Err(corrupt("tool manifest belongs to a different source"));
    }
    let (opened_seq, version, data) = exact(
        store,
        parent,
        "session.assistant-message.opened",
        "message",
        identity.message.as_str(),
        &visible,
    )?;
    let opened = match version {
        1 => session_message::decode_opened(&data),
        2 => session_message::decode_opened_v2(&data),
        _ => return Err(corrupt("unsupported assistant opening version")),
    }
    .map_err(|error| corrupt(error.to_string()))?;
    if opened.message() != &identity.message || opened_seq <= 0 || opened_seq >= manifest_seq {
        return Err(corrupt("tool manifest has no preceding assistant opening"));
    }
    Ok(opened_seq - 1)
}

fn exact(
    store: &EventStore,
    parent: &SessionId,
    kind: &str,
    field: &str,
    identity: &str,
    visible: &str,
) -> Result<(i64, u32, Value), SessionLifecycleError> {
    let mut query = store.connection().prepare(&format!(
        "SELECT seq, CASE WHEN length(CAST(type AS BLOB)) <= 256 THEN type END,
                CASE WHEN length(CAST(data AS BLOB)) <= 16384 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
           AND rtrim(type, '0123456789') = ?2 || '.' AND {visible}
           AND CASE WHEN json_valid(data) THEN json_extract(data, ?3) = ?4 ELSE 1 END
         ORDER BY seq LIMIT 2"
    ))?;
    let mut rows = query.query(rusqlite::params![
        parent.as_str(),
        kind,
        format!("$.{field}"),
        identity
    ])?;
    let row = rows
        .next()?
        .ok_or_else(|| corrupt("fork source is absent"))?;
    let seq: i64 = row.get(0)?;
    let versioned: Option<String> = row.get(1)?;
    let versioned = versioned.ok_or_else(|| corrupt("fork source type exceeds its read bound"))?;
    let raw: Option<String> = row.get(2)?;
    let raw = raw.ok_or(SessionLifecycleError::ForkDataTooLong)?;
    let version = versioned
        .rsplit_once('.')
        .and_then(|(_, version)| version.parse().ok())
        .ok_or_else(|| corrupt("fork source version is invalid"))?;
    let data = serde_json::from_str(&raw).map_err(|error| corrupt(error.to_string()))?;
    if rows.next()?.is_some() {
        return Err(corrupt("fork source identity is repeated"));
    }
    Ok((seq, version, data))
}

fn corrupt(reason: impl Into<String>) -> SessionLifecycleError {
    SessionLifecycleError::Corrupt(reason.into())
}

/// The exact source event must survive rewind in this Session. Resolve before
/// copying so a foreign, hidden or future sequence cannot mint a child.
pub(super) fn cutoff(
    store: &EventStore,
    parent: &SessionId,
    through: EventSeq,
) -> Result<i64, SessionLifecycleError> {
    let seq = i64::try_from(through.value()).map_err(|_| SessionLifecycleError::InvalidCutoff)?;
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), parent)?;
    let exists: bool = store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND seq=?2)",
        rusqlite::params![parent.as_str(), seq], |row| row.get(0),
    )?;
    if !exists || !visible.contains(through.value()) {
        return Err(SessionLifecycleError::InvalidCutoff);
    }
    Ok(seq)
}

// Terminal-turn truncation: use canonical visible facts,
// refuse a partial source, and pass the closed prefix to the existing fork owner.
pub(super) fn require_settled(
    parent: &SessionId,
    rows: &[CopiedRow],
) -> Result<(), SessionLifecycleError> {
    use grapt_types::{ContentKind, ToolExecution};
    use std::collections::{HashMap, HashSet};
    let mut history = MessageHistory::default();
    let mut calls = HashMap::new();
    let mut terminals = HashSet::new();
    for row in rows {
        history
            .apply(HistoryRow {
                seq: u64::try_from(row.seq).map_err(|_| corrupt("negative fork sequence"))?,
                kind_name: &row.kind,
                kind_version: row.version.value(),
                data: &row.data,
            })
            .map_err(|error| corrupt(error.to_string()))?;
        match row.kind.as_str() {
            "session.tool.input.started" => {
                let identity = session_tool_identity::decode(row.version.value(), &row.data)
                    .map_err(|error| corrupt(error.to_string()))?;
                if identity.session != *parent
                    || calls.insert(identity.call, identity.execution).is_some()
                {
                    return Err(corrupt("fork tool manifest is foreign or repeated"));
                }
            }
            "session.tool.terminal" => {
                if row.version.value() != 1 {
                    return Err(corrupt("unsupported fork tool terminal"));
                }
                let fact = grapt_event::session_tool_terminal::decode(&row.data)
                    .map_err(|error| corrupt(error.to_string()))?;
                if !terminals.insert(fact.call().clone()) {
                    return Err(corrupt("fork tool terminal is repeated"));
                }
            }
            _ => {}
        }
    }
    let messages = history
        .messages()
        .map_err(|error| corrupt(error.to_string()))?;
    if messages.iter().any(|message| message.terminal.is_none())
        || calls.iter().any(|(call, execution)| {
            *execution == ToolExecution::Local && !terminals.contains(call)
        })
    {
        return Err(SessionLifecycleError::UnfinishedCutoff);
    }
    for message in messages {
        if let Some(content) = message.content {
            for item in content.items() {
                if let ContentKind::Tool { call } = item.item.kind() {
                    if !calls.contains_key(call) {
                        return Err(corrupt("fork tool call has no identity manifest"));
                    }
                }
            }
        }
    }
    Ok(())
}
