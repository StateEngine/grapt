//! Durable effect and execution associations for continuations.
use super::*;

/// Whether `call`'s effect provably never ran: its permission was asked and
/// no reply granted it. There is no durable record that an effect began, so
/// this negative is the strongest fact the log holds, and it is sound because:
/// (a) every tool kind is prepared, then asks through `begin_tool`, and its
///     effect runs only inside `consume_and_run` after authorization;
/// (b) a grant's `permission.replied.2` commits with the reply the effect's
///     lease depends on;
/// (c) only a rejection cascades to other asks, never a grant;
/// (d) one call has at most one ask (`permission_calls_for` refuses more).
/// A call allowed by a configured, saved or session rule writes no ask, so it
/// is never reported as withheld: the log cannot tell whether it ran.
pub(super) fn effect_withheld(
    store: &EventStore,
    session: &SessionId,
    call: &ToolCallId,
) -> Result<bool, String> {
    let asks = scoped_rows(
        store,
        session,
        "permission.asked.2",
        None,
        None,
        Scope::Call(Some(call), "$.source.callID"),
        MAX_CONTINUATION_ROWS,
    )?;
    let mut request = None;
    for (_, data) in asks {
        let value = serde_json::from_str(&data)
            .map_err(|error| format!("permission ask is not JSON: {error}"))?;
        let Some(PermissionEvent::Asked {
            id, coordinates, ..
        }) = PermissionEvent::decode("permission.asked", 2, &value)
            .map_err(|error| format!("permission ask is corrupt: {error}"))?
        else {
            continue;
        };
        let source = coordinates.and_then(|coordinates| coordinates.source);
        let asked_for = match &source {
            Some(source) => tool_source(source)?.map(|(_, asked)| asked),
            None => None,
        };
        if asked_for.as_ref() != Some(call) {
            continue;
        }
        if request.replace(id).is_some() {
            return Err(format!("tool {call} has duplicate permission asks"));
        }
    }
    let Some(request) = request else {
        return Ok(false);
    };
    let replies = scoped_rows(
        store,
        session,
        "permission.replied.2",
        None,
        None,
        Scope::Request(&request),
        MAX_CONTINUATION_ROWS,
    )?;
    for (_, data) in replies {
        let value = serde_json::from_str(&data)
            .map_err(|error| format!("permission reply is not JSON: {error}"))?;
        let Some(PermissionEvent::Replied {
            request: replied,
            reply,
            ..
        }) = PermissionEvent::decode("permission.replied", 2, &value)
            .map_err(|error| format!("permission reply is corrupt: {error}"))?
        else {
            continue;
        };
        if replied == request && reply != grapt_types::PermissionReply::Reject {
            return Ok(false);
        }
    }
    Ok(true)
}

/// What a Session blocked by an orphaned nested start tells its user: the run
/// that was lost, and the way out.
pub(in crate::dogfood) const ORPHANED_REFUSAL: &str = "an earlier tool run was lost before its result was recorded; run /reconcile to record it as unknown and continue";

pub(in crate::dogfood) fn require_resolved_effects(
    store: &EventStore,
    session: &SessionId,
) -> Result<(), String> {
    if orphaned_nested(store, session)?.is_empty() {
        Ok(())
    } else {
        Err(ORPHANED_REFUSAL.into())
    }
}

/// Nested starts with no terminal, oldest first: what process loss left
/// running. An unknown outcome is a terminal; it settles admission, not the
/// effect, so the model sees its notice and undo keeps its own preconditions.
pub(in crate::dogfood) fn orphaned_nested(
    store: &EventStore,
    session: &SessionId,
) -> Result<Vec<(grapt_types::PhysicalAttemptId, ToolCallId)>, String> {
    // One streaming cursor, no transcript cut and no lifetime-sized collection:
    // only unresolved starts are held, each with its seq for ordering. A
    // larger unresolved set than reconciliation may settle is itself
    // quarantine, never permission to discard.
    let mut pending = std::collections::BTreeMap::new();
    let terminal = grapt_store::stored_terminal::bounded_column("data");
    let mut statement = store.connection().prepare(&format!(
        "SELECT seq, type, CASE WHEN type = 'session.tool.terminal.1' THEN NULL ELSE data END,
                CASE WHEN type = 'session.tool.terminal.1' THEN {terminal} END
         FROM event WHERE aggregate_family = 'session'
         AND aggregate_id = ?1 AND type IN ('session.tool.nested.started.1', 'session.tool.terminal.1')
         ORDER BY seq"
    )).map_err(|e| e.to_string())?;
    let mapped = statement
        .query_map([session.as_str()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(2)?,
                grapt_store::stored_terminal::classify_optional(row, 3)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    for row in mapped {
        let (seq, nested, terminal) = row.map_err(|e| e.to_string())?;
        if seq < 0 {
            return Err("tool effect has negative sequence".into());
        }
        if let Some(data) = nested {
            let value = serde_json::from_str(&data).map_err(|_| "nested admission is not JSON")?;
            let fact = grapt_event::session_tool_nested::decode(&value)
                .map_err(|_| "nested admission is corrupt")?;
            let key = (fact.attempt().clone(), fact.call().clone());
            if pending.insert(key, seq).is_some() {
                return Err("nested admission is duplicated".into());
            }
            if pending.len() > grapt_runtime::MAX_ORPHANED_NESTED {
                return Err(
                    "unresolved nested effects exceed what /reconcile may settle; this session cannot continue".into(),
                );
            }
        } else if let Some(StoredTerminal::Readable(fact)) = terminal {
            // An unreadable terminal settles nothing: its start stays an
            // orphan, which /reconcile settles once with an unknown effect.
            pending.remove(&(fact.attempt().clone(), fact.call().clone()));
        }
    }
    let mut order: Vec<_> = pending.into_iter().collect();
    order.sort_by_key(|(_, seq)| *seq);
    Ok(order.into_iter().map(|(key, _)| key).collect())
}

// Query only an exact prior execution boundary and its original promoted identity.
pub(super) fn execution_input_before(
    inbox: &Inbox,
    session: &SessionId,
    opening: u64,
) -> Result<Option<MessageId>, String> {
    let start = grapt_store::session_event_queries::bounds::execution_start_before(
        inbox.store(),
        session,
        grapt_types::EventSeq::new(opening),
    )?;
    let Some(start) = start else {
        return Ok(None);
    };
    inbox
        .original_promoted_input_before(session, start)
        .map_err(|error| format!("tool execution input is unreadable: {error}"))
}
