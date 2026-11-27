//! Preserve captured mail as quoted history, never as delivery ownership.
use super::*;
use grapt_store::operation_mail::OperationMailPresented;

/// A fork cannot acknowledge the parent's receipt or replay a pending message.
/// Only a visible presentation becomes a synthetic conversation entry, already
/// promoted in the same fork transaction. Its opaque body remains quoted data.
pub(super) fn materialize(
    store: &EventStore,
    authorities: &DeterministicAuthorities<'_>,
    parent: &SessionId,
    rows: Vec<CopiedRow>,
) -> Result<Vec<CopiedRow>, SessionLifecycleError> {
    let row_limit =
        i64::try_from(MAX_FORK_ROW_BYTES).map_err(|_| SessionLifecycleError::ForkDataTooLong)?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    for mut row in rows {
        if row.kind == "session.subagent-mail.presented" {
            if row.version.value() != 1 {
                return Err(corrupt("unknown inherited mail presentation version"));
            }
            let capture = OperationMailPresented::decode(&row.data).map_err(corrupt)?;
            let source = i64::try_from(capture.source_seq.value())
                .map_err(|_| corrupt("inherited mail source exceeds SQLite range"))?;
            if capture.session != *parent || source >= row.seq {
                return Err(corrupt("inherited mail has a foreign or later source"));
            }
            let raw: Option<String> = store.connection().query_row(
                "SELECT CASE WHEN length(CAST(data AS BLOB))<=?3 THEN data END
                 FROM event WHERE aggregate_family='session' AND aggregate_id=?1
                 AND seq=?2 AND type='session.input.admitted.1'",
                rusqlite::params![parent.as_str(), source, row_limit],
                |row| row.get(0),
            )?;
            let data: Value =
                serde_json::from_str(&raw.ok_or(SessionLifecycleError::ForkDataTooLong)?)
                    .map_err(|error| corrupt(error.to_string()))?;
            if data.get("sessionID").and_then(Value::as_str) != Some(parent.as_str())
                || data.get("inputID").and_then(Value::as_str) != Some(capture.input.as_str())
                || !matches!(
                    data.pointer("/input/type").and_then(Value::as_str),
                    Some("operation-delivery" | "agent-message")
                )
            {
                return Err(corrupt("inherited mail differs from its captured source"));
            }
            let text = grapt_store::operation_mail::render_quoted_input(&data).map_err(corrupt)?;
            // This becomes an ordinary historical synthetic input; retain that
            // owner's text bound as well as the fork's row and total bounds.
            if text.len() > 256 * 1024 {
                return Err(SessionLifecycleError::ForkDataTooLong);
            }
            row.kind = "session.input.admitted".to_owned();
            row.data = json!({"sessionID":parent.as_str(),"inputID":capture.input.as_str(),
                "input":{"type":"synthetic","rustAuthorized":true,"delivery":"queue",
                    "data":{"text":text,"description":"Inherited agent mail",
                        "metadata":{"source":"fork-agent-mail","parentID":parent.as_str(),
                            "sourceSeq":source}}}});
            let promoted = CopiedRow {
                seq: row.seq,
                kind: "session.input.promoted".to_owned(),
                version: version(1)?,
                created_ms: row.created_ms,
                data: json!({"sessionID":parent.as_str(),"inputID":capture.input.as_str()}),
                event: minted_event(authorities)?,
            };
            push(&mut result, &mut bytes, row)?;
            push(&mut result, &mut bytes, promoted)?;
        } else {
            push(&mut result, &mut bytes, row)?;
        }
    }
    Ok(result)
}

fn push(
    rows: &mut Vec<CopiedRow>,
    bytes: &mut usize,
    row: CopiedRow,
) -> Result<(), SessionLifecycleError> {
    if rows.len() >= MAX_FORK_ROWS {
        return Err(SessionLifecycleError::ForkTooLong);
    }
    let charge = serde_json::to_vec(&row.data)
        .map_err(|error| corrupt(error.to_string()))?
        .len();
    *bytes = bytes.saturating_add(charge);
    if charge > MAX_FORK_ROW_BYTES || *bytes > MAX_FORK_DATA_BYTES {
        return Err(SessionLifecycleError::ForkDataTooLong);
    }
    rows.push(row);
    Ok(())
}

fn corrupt(reason: impl Into<String>) -> SessionLifecycleError {
    SessionLifecycleError::Corrupt(reason.into())
}
