//! Agent-authored mail shares capture and replay with operation results.

use super::*;

pub(super) fn pending(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<Item>, String> {
    let mut query = conn.prepare(
        "SELECT e.seq, CASE WHEN length(CAST(e.data AS BLOB))<=397312 THEN e.data END,
            CASE WHEN length(CAST(e.type AS BLOB))<=256 THEN e.type END
         FROM event e WHERE e.aggregate_family='session' AND e.aggregate_id=?1
            AND e.type LIKE 'session.input.admitted.%'
            AND CASE WHEN json_valid(e.data) THEN json_extract(e.data,'$.input.type')='agent-message' ELSE 1 END
            AND NOT EXISTS(SELECT 1 FROM event c WHERE c.id='evt_agent_mail_' || json_extract(e.data,'$.inputID'))
         ORDER BY e.seq LIMIT 8").map_err(|error| error.to_string())?;
    let mut rows = query
        .query([session.as_str()])
        .map_err(|error| error.to_string())?;
    let mut items = Vec::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let seq = u64::try_from(row.get::<_, i64>(0).map_err(|error| error.to_string())?)
            .map_err(|_| "negative agent message sequence")?;
        let raw: Option<String> = row.get(1).map_err(|error| error.to_string())?;
        let kind: Option<String> = row.get(2).map_err(|error| error.to_string())?;
        if kind.as_deref() != Some("session.input.admitted.1") {
            return Err("agent message has an unknown admission version".into());
        }
        let data = serde_json::from_str(&raw.ok_or("agent message exceeds its read bound")?)
            .map_err(|error| error.to_string())?;
        items.push(decode_item(session, seq, data)?);
    }
    Ok(items)
}
