//! A successor's triggering message must enter that run's bounded context.
use super::*;
use rusqlite::OptionalExtension;

fn cause(
    conn: &rusqlite::Connection,
    session: &SessionId,
    input: &MessageId,
) -> Result<Option<Item>, String> {
    if input.as_str().len() > 512 {
        return Err("priority mail identity exceeds its bound".into());
    }
    let source = input
        .as_str()
        .strip_prefix("msg_agent_mail_")
        .ok_or("priority input is not native agent mail")?;
    let event = format!("evt_agent_message_{source}");
    let row = conn
        .query_row(
            "SELECT seq,
        CASE WHEN length(CAST(data AS BLOB))<=397312 THEN data END,
        CASE WHEN length(CAST(type AS BLOB))<=256 THEN type END,
        EXISTS(SELECT 1 FROM event c WHERE c.id=?3)
        FROM event WHERE id=?1 AND aggregate_family='session' AND aggregate_id=?2",
            (event, session.as_str(), capture_id(input)?.as_str()),
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or("priority mail source is missing or foreign")?;
    if row.2.as_deref() != Some("session.input.admitted.1") {
        return Err("priority mail admission version is invalid".into());
    }
    let seq = u64::try_from(row.0).map_err(|_| "negative priority mail sequence")?;
    let data = serde_json::from_str(&row.1.ok_or("priority mail exceeds its read bound")?)
        .map_err(|error| error.to_string())?;
    let item = decode_item(session, seq, data)?;
    if item.input != *input || item.receipt.is_some() {
        return Err("priority input differs from its canonical agent message".into());
    }
    Ok((!row.3).then_some(item))
}

pub(super) fn select(
    conn: &rusqlite::Connection,
    session: &SessionId,
    step: StepNumber,
    priority: Option<&MessageId>,
) -> Result<Vec<Item>, String> {
    let prior: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family='session' AND aggregate_id=?1
        AND type='session.subagent-mail.presented.1' AND json_extract(data,'$.step')=?2",
            (session.as_str(), step.value()),
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let prior = usize::try_from(prior).map_err(|_| "negative mail presentation count")?;
    if prior > 8 {
        return Err("agent mail exceeds its per-Step presentation bound".into());
    }
    let priority = priority
        .map(|input| cause(conn, session, input))
        .transpose()?
        .flatten();
    if prior == 8 {
        if priority.is_some() {
            return Err("followup cause exceeds its per-Step presentation bound".into());
        }
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    let mut bytes = 0usize;
    if let Some(item) = priority {
        bytes = render(&item.data)?.len();
        result.push(item);
    }
    for item in pending(conn, session, false)? {
        if result.len() == 8 - prior {
            break;
        }
        if result.iter().any(|selected| selected.input == item.input) {
            continue;
        }
        let charge = render(&item.data)?.len();
        if bytes.saturating_add(charge) > MAX_BYTES {
            break;
        }
        bytes += charge;
        result.push(item);
    }
    if bytes > MAX_BYTES {
        return Err("followup cause exceeds the presentation byte bound".into());
    }
    result.sort_by_key(|item| item.seq);
    Ok(result)
}
