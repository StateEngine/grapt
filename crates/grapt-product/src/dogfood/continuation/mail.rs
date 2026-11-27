//! Replay captured mail at its canonical sequence, including compaction cuts.
use super::*;
use grapt_event::mail_presented::{MailPresented, MAX_PRESENTATION_BYTES};
use grapt_store::RemoteMailboxScope;

pub(super) fn rows(
    store: &EventStore,
    session: &SessionId,
    from: Option<grapt_types::EventSeq>,
    until: Option<grapt_types::EventSeq>,
    ceiling: u64,
) -> Result<Vec<(u64, CodexInputItem)>, String> {
    let from = from.map_or(Ok(i64::MIN), |seq| {
        i64::try_from(seq.value()).map_err(|_| "mail context cut exceeds SQLite range")
    })?;
    let until = until
        .map(|seq| {
            i64::try_from(seq.value()).map_err(|_| "mail context window exceeds SQLite range")
        })
        .transpose()?;
    let ceiling = i64::try_from(ceiling).unwrap_or(i64::MAX);
    let mut statement = store
        .connection()
        .prepare(
            "SELECT seq,
            CASE WHEN length(CAST(data AS BLOB))<=?5 THEN data ELSE NULL END,
            CASE WHEN length(type)<=256 THEN type ELSE NULL END
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
            AND type LIKE 'session.mail.presented.%'
            AND (seq>=?2 OR seq<0) AND (?3 IS NULL OR seq<?3) AND seq<?6
         ORDER BY seq LIMIT ?4",
        )
        .map_err(|e| e.to_string())?;
    let mut cursor = statement
        .query((
            session.as_str(),
            from,
            until,
            i64::try_from(MAX_CONTINUATION_ROWS + 1).map_err(|_| "mail row bound")?,
            i64::try_from(MAX_PRESENTATION_BYTES).map_err(|_| "mail codec bound")?,
            ceiling,
        ))
        .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next().map_err(|e| e.to_string())? {
        if rows.len() == MAX_CONTINUATION_ROWS {
            return Err("mail captures exceed the continuation row bound".into());
        }
        let seq = u64::try_from(row.get::<_, i64>(0).map_err(|e| e.to_string())?)
            .map_err(|_| "negative mail capture sequence")?;
        let kind: Option<String> = row.get(2).map_err(|e| e.to_string())?;
        if kind.as_deref() != Some("session.mail.presented.1") {
            return Err("unknown mail capture version".into());
        }
        let raw: Option<String> = row.get(1).map_err(|e| e.to_string())?;
        let value: serde_json::Value =
            serde_json::from_str(&raw.ok_or("mail capture exceeds its codec bound")?)
                .map_err(|_| "mail capture is not JSON")?;
        let capture = MailPresented::decode(&value)?;
        // Only this input's canonical window is selected. Corruption within
        // it refuses continuation; a later user's mail does not enter it.
        let expected = RemoteMailboxScope {
            projects: capture.projects.clone(),
            principal: capture.principal.clone(),
            recipient: capture.recipient.clone(),
        };
        let records = store
            .remote_mail_presentation(&expected, &capture)
            .map_err(|e| format!("captured mail history is unreadable: {e}"))?;
        let mut content = String::new();
        for record in records {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&grapt_runtime::mailbox::remote::render_notification(
                &record,
            )?);
        }
        rows.push((
            seq,
            CodexInputItem::Message(InputMessage {
                role: MessageRole::User,
                content,
            }),
        ));
    }
    Ok(rows)
}
