//! Fold only the durable events owned by one selected assistant message.
use super::*;
use rusqlite::OptionalExtension;

impl SessionHistoryStore<'_> {
    /// Bounds apply to one message, so a long Session never requires a lifetime fold.
    pub fn message_at(
        &self,
        session: &SessionId,
        opening: grapt_types::EventSeq,
    ) -> Result<Option<AssistantMessage>, HistoryReadError> {
        let store = |source| HistoryReadError::Store {
            session: session.as_str().into(),
            source,
        };
        let opening = i64::try_from(opening.value()).map_err(|_| HistoryReadError::Corrupt {
            session: session.as_str().into(),
            seq: i64::MAX,
            kind: "history opening".into(),
            reason: "sequence exceeds SQLite range".into(),
        })?;
        let visible = visibility(self.conn, session)?.sql("seq");
        let until: Option<i64> = self
            .conn
            .query_row(
                &format!(
                    "SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
             AND {visible} AND seq>?2 AND (type='session.assistant-message.opened' OR type GLOB 'session.assistant-message.opened.*')
             ORDER BY seq LIMIT 1"
                ),
                (session.as_str(), opening),
                |row| row.get(0),
            )
            .optional()
            .map_err(store)?;
        let owned = OWNED_KINDS
            .iter()
            .chain(BINDING_EVIDENCE_KINDS.iter())
            .map(|kind| format!("(type='{kind}' OR type GLOB '{kind}.*')"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut statement = self.conn.prepare(&format!(
            "SELECT id, seq, created, type, CASE WHEN length(CAST(data AS BLOB)) <= 1048576 THEN data END
             FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND {visible}
             AND seq>=?2 AND (?3 IS NULL OR seq<?3) AND ({owned}) ORDER BY seq LIMIT ?4" )).map_err(store)?;
        let probe = i64::try_from(MAX_HISTORY_ROWS + 1).map_err(|_| HistoryReadError::TooLong {
            session: session.as_str().into(),
        })?;
        let mapped = statement
            .query_map(
                rusqlite::params![session.as_str(), opening, until, probe],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map_err(store)?;
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        for row in mapped {
            let row = row.map_err(store)?;
            bytes = bytes.saturating_add(row.4.as_ref().map_or(0, String::len));
            if bytes > 16 * 1024 * 1024 {
                return Err(HistoryReadError::WindowDataTooLong {
                    session: session.as_str().into(),
                    limit: 16 * 1024 * 1024,
                });
            }
            rows.push(row);
            if rows.len() > MAX_HISTORY_ROWS {
                return Err(HistoryReadError::TooLong {
                    session: session.as_str().into(),
                });
            }
        }
        let history = self.assemble_history(session, &rows, Some(16 * 1024 * 1024))?;
        Ok(history.messages()?.into_iter().find(|message| {
            message
                .opened
                .as_ref()
                .is_some_and(|position| position.seq == opening as u64)
        }))
    }
}
