//! Status-only views of native nested admissions; no payload reads or retained cache.
use super::*;
use grapt_api::u3a::{ToolProgressEntry, ToolProgressMetadata, ToolProgressStatus};

pub(in crate::dogfood) fn parent(
    store: &EventStore,
    session: &SessionId,
    call: &ToolCallId,
) -> Result<Option<ToolCallId>, String> {
    let mut parent = None;
    for (_, data) in scoped_rows(
        store,
        session,
        "session.tool.nested.started.1",
        None,
        None,
        Scope::Call(Some(call), "$.call"),
        MAX_CONTINUATION_ROWS,
    )? {
        let value = serde_json::from_str(&data).map_err(|_| "nested admission is not JSON")?;
        let fact = grapt_event::session_tool_nested::decode(&value)
            .map_err(|_| "nested admission corrupt")?;
        if fact.call() == call {
            if parent.is_some() {
                return Err("nested call identity repeated".into());
            }
            parent = Some(fact.parent().clone());
        }
    }
    Ok(parent)
}

/// The visible model call owns both the row and its assistant message.
pub(in crate::dogfood) fn owner(
    store: &EventStore,
    session: &SessionId,
    call: &ToolCallId,
) -> Result<Option<(ToolCallId, MessageId)>, String> {
    let display = parent(store, session, call)?.unwrap_or_else(|| call.clone());
    let page = grapt_session::SessionHistoryStore::over(store)
        .tool_call_page_for(session, &display)
        .map_err(|error| error.to_string())?;
    match page.rows() {
        [row] if row.call() == &display => Ok(Some((display, row.message().clone()))),
        [] => Ok(None),
        _ => Err("progress call ownership is ambiguous".into()),
    }
}

pub(in crate::dogfood) fn snapshot(
    store: &EventStore,
    session: &SessionId,
    parent: &ToolCallId,
    live: bool,
) -> Result<Option<ToolProgressMetadata>, String> {
    let mut calls = Vec::new();
    for (_, data) in scoped_rows(
        store,
        session,
        "session.tool.nested.started.1",
        None,
        None,
        Scope::Call(Some(parent), "$.parent"),
        MAX_CONTINUATION_ROWS,
    )? {
        let value = serde_json::from_str(&data).map_err(|_| "nested admission is not JSON")?;
        let fact = grapt_event::session_tool_nested::decode(&value)
            .map_err(|_| "nested admission corrupt")?;
        if fact.parent() == parent {
            if calls.len() >= 64 || calls.iter().any(|(call, _, _)| call == fact.call()) {
                return Err("nested progress admission bound or duplicate".into());
            }
            calls.push((
                fact.call().clone(),
                fact.tool().as_str().to_owned(),
                ToolProgressStatus::Running,
            ));
        }
    }
    if calls.is_empty() {
        return Ok(None);
    }
    let mut parent_done = false;
    let mut closed = std::collections::BTreeSet::new();
    for ScopedTerminal { terminal: data, .. } in scoped_terminals(
        store,
        session,
        None,
        None,
        Scope::Composition(parent),
        MAX_CONTINUATION_ROWS,
    )? {
        let StoredTerminal::Readable(fact) = data else {
            continue;
        };
        if fact.call() == parent {
            parent_done = true;
        }
        if let Some((_, _, status)) = calls.iter_mut().find(|(call, _, _)| call == fact.call()) {
            if !closed.insert(fact.call().clone()) {
                return Err("nested terminal duplicated".into());
            }
            *status = if matches!(fact.outcome(), ToolTerminalOutcome::Completed { .. }) {
                ToolProgressStatus::Completed
            } else {
                ToolProgressStatus::Error
            };
        }
    }
    if live && parent_done {
        return Ok(None);
    }
    let entries = calls
        .into_iter()
        .map(|(_, tool, status)| {
            ToolProgressEntry::new(
                &tool,
                if parent_done && status == ToolProgressStatus::Running {
                    ToolProgressStatus::Error
                } else {
                    status
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    ToolProgressMetadata::new(entries)
        .map(Some)
        .map_err(Into::into)
}
