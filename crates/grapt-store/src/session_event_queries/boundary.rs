//! Structural history evidence, without continuation limits or compaction policy.
use crate::write::EventStore;
use grapt_types::{EventSeq, SessionId};

mod probes;
pub use probes::Probe;

fn from_sql(from: Option<EventSeq>) -> Result<i64, String> {
    from.map(|from| i64::try_from(from.value()))
        .transpose()
        .map(|from| from.unwrap_or(i64::MIN))
        .map_err(|_| "continuation cut exceeds SQLite range".to_owned())
}

fn decoded(seq: Option<i64>, reason: &str) -> Result<Option<EventSeq>, String> {
    seq.map(|seq| {
        u64::try_from(seq)
            .map(EventSeq::new)
            .map_err(|_| reason.to_owned())
    })
    .transpose()
}

/// Observe the first row past a caller's bound, without materializing payloads.
/// Visibility is deliberately ignored: counting hidden rows is conservative.
pub fn first_over(
    store: &EventStore,
    session: &SessionId,
    from: Option<EventSeq>,
    probe: Probe,
    limit: usize,
) -> Result<Option<EventSeq>, String> {
    let from = from_sql(from)?;
    let limit = i64::try_from(limit).map_err(|_| "probe bound exceeds SQLite range")?;
    let seq = store
        .connection()
        .query_row(probe.sql(), (session.as_str(), from, limit), |row| {
            row.get(0)
        })
        .map_err(|error| format!("{} boundary probe failed: {error}", probe.label()))?;
    decoded(seq, "structural boundary has a negative sequence")
}

/// The first exchange start at or after `seq`, a promoted input or an
/// assistant opening: a summary window begins only there, so it holds whole
/// Steps and never starts between a call and its result.
pub fn exchange_start_at_or_after(
    store: &EventStore,
    session: &SessionId,
    seq: EventSeq,
) -> Result<Option<EventSeq>, String> {
    let seq = i64::try_from(seq.value()).map_err(|_| "window start exceeds SQLite range")?;
    let found: Option<i64> = store
        .connection()
        .query_row(
            "SELECT MIN(seq) FROM (
               SELECT seq FROM session_message
                WHERE session_id = ?1 AND type IN ('user', 'synthetic') AND seq >= ?2
               UNION ALL
               SELECT seq FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                AND type IN ('session.assistant-message.opened.1', 'session.assistant-message.opened.2')
                AND seq >= ?2)",
            (session.as_str(), seq),
            |row| row.get(0),
        )
        .map_err(|error| format!("window start probe failed: {error}"))?;
    decoded(found, "window start has a negative sequence")
}

pub fn is_promoted(
    store: &EventStore,
    session: &SessionId,
    boundary: EventSeq,
) -> Result<bool, String> {
    let boundary =
        i64::try_from(boundary.value()).map_err(|_| "promoted boundary exceeds SQLite range")?;
    store
        .connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM session_message
                 WHERE session_id = ?1 AND type IN ('user', 'synthetic') AND seq = ?2)",
            (session.as_str(), boundary),
            |row| row.get(0),
        )
        .map_err(|error| format!("promoted boundary check failed: {error}"))
}

pub fn latest_assistant_opening(
    store: &EventStore,
    session: &SessionId,
    from: Option<EventSeq>,
    before: Option<EventSeq>,
) -> Result<Option<EventSeq>, String> {
    let from = from_sql(from)?;
    let before = before
        .map(|before| i64::try_from(before.value()))
        .transpose()
        .map_err(|_| "continuation window exceeds SQLite range")?
        .unwrap_or(i64::MAX);
    let seq = store
        .connection()
        .query_row(
            "SELECT MAX(seq) FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND seq >= ?2 AND seq < ?3
                   AND type IN ('session.assistant-message.opened.1',
                                'session.assistant-message.opened.2')",
            (session.as_str(), from, before),
            |row| row.get(0),
        )
        .map_err(|error| format!("assistant boundary probe failed: {error}"))?;
    decoded(seq, "summary cut has a negative sequence")
}

/// Opening of an exchange whose call is before the cut but output is not.
pub fn split_exchange_opening(
    store: &EventStore,
    session: &SessionId,
    from: Option<EventSeq>,
    until: EventSeq,
) -> Result<Option<EventSeq>, String> {
    let from = from_sql(from)?;
    let until =
        i64::try_from(until.value()).map_err(|_| "structural boundary exceeds SQLite range")?;
    let seq = store
        .connection()
        .query_row(
            "SELECT MIN(o.seq) FROM event c
                 JOIN event t ON t.aggregate_family = 'session' AND t.aggregate_id = ?1
                   AND t.type = 'session.tool.terminal.1' AND t.seq >= ?3
                   AND json_valid(t.data)
                   AND json_extract(t.data, '$.call') = json_extract(c.data, '$.item.call')
                 JOIN event o ON o.aggregate_family = 'session' AND o.aggregate_id = ?1
                   AND o.seq <= c.seq
                   AND o.type IN ('session.assistant-message.opened.1',
                                  'session.assistant-message.opened.2')
                   AND json_valid(o.data)
                   AND json_extract(o.data, '$.message') = json_extract(c.data, '$.message')
                 WHERE c.aggregate_family = 'session' AND c.aggregate_id = ?1
                   AND c.seq >= ?2 AND c.seq < ?3
                   AND c.type = 'session.content.appended.1' AND json_valid(c.data)
                   AND json_extract(c.data, '$.item.kind') = 'tool'",
            (session.as_str(), from, until),
            |row| row.get(0),
        )
        .map_err(|error| format!("split exchange probe failed: {error}"))?;
    decoded(seq, "exchange opening has a negative sequence")
}

/// Bounded preview ceiling for local tools whose terminal outputs are
/// projected inline unless excluded by `unpreviewed_tools`.
#[derive(Debug, Clone, Copy)]
pub struct PreviewBudget<'a> {
    pub cap: usize,
    pub unpreviewed_tools: &'a [&'a str],
}

/// First position past a declared-payload budget. No artifact is opened.
/// The caller owns both the per-item charge cap and the total byte ceiling.
/// Candidate terminal rows are validated through the shared
/// `grapt_event::session_tool_terminal` decoder so only decodable terminals
/// are charged and malformed rows never abort the query.
pub fn declared_bytes_over(
    store: &EventStore,
    session: &SessionId,
    from: Option<EventSeq>,
    until: Option<EventSeq>,
    cap: usize,
    limit: usize,
    preview: Option<PreviewBudget<'_>>,
) -> Result<Option<EventSeq>, String> {
    // Match the payload reader's saturated range for an out-of-range cut.
    let at = |seq: EventSeq| i64::try_from(seq.value()).unwrap_or(i64::MAX);
    let from = from.map_or(i64::MIN, at);
    let until = until.map(at);
    let cap_sql = i64::try_from(cap).map_err(|_| "artifact bound")?;
    let (preview_cap, unpreviewed_json) = match preview {
        Some(preview) => (
            preview.cap,
            Some(
                serde_json::to_string(preview.unpreviewed_tools)
                    .map_err(|error| format!("unpreviewed tool list encoding failed: {error}"))?,
            ),
        ),
        None => (cap, None),
    };
    let probe = |error: rusqlite::Error| format!("declared content boundary probe failed: {error}");
    // Messages and content declare their clamped length in SQL. A terminal
    // declares only what its decoded fact names: an undecodable one is no
    // evidence of any output and charges nothing.
    let terminal = crate::stored_terminal::bounded_column("e.data");
    let mut statement = store
        .connection()
        .prepare(&format!(
            "WITH local_preview_calls AS (
                SELECT json_extract(data, '$.callID') AS call_id
                  FROM event
                 WHERE ?5 IS NOT NULL
                   AND aggregate_family = 'session'
                   AND aggregate_id = ?1
                   AND type IN ('session.tool.input.started.1', 'session.tool.input.started.2')
                   AND CASE
                         WHEN typeof(data) = 'text' AND json_valid(data)
                         THEN coalesce(json_extract(data, '$.execution'), 'local') = 'local'
                              AND json_type(data, '$.name') = 'text'
                              AND NOT EXISTS (
                                  SELECT 1 FROM json_each(?5)
                                   WHERE value = json_extract(data, '$.name'))
                         ELSE 0
                       END = 1
            )
            SELECT seq, bytes, terminal, is_previewed FROM (
                SELECT seq, min(max(coalesce(length(CAST(text AS BLOB)), 0), 0), ?4) AS bytes,
                       NULL AS terminal, 0 AS is_previewed FROM session_message
                 WHERE session_id = ?1 AND type IN ('user', 'synthetic')
                   AND (seq >= ?2 OR seq < 0) AND (?3 IS NULL OR seq < ?3)
                UNION ALL
                SELECT seq, min(max(coalesce(CASE WHEN typeof(data) = 'text' AND json_valid(data)
                    THEN json_extract(data, '$.item.payload.length') END, 0), 0), ?4),
                    NULL, 0 FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND (seq >= ?2 OR seq < 0) AND (?3 IS NULL OR seq < ?3)
                   AND type = 'session.content.appended.1'
                   AND CASE
                         WHEN typeof(data) != 'text' OR json_valid(data) = 0 THEN 1
                         WHEN json_extract(data, '$.item.kind') NOT IN ('reasoning', 'opaque') THEN 1
                         ELSE 0
                       END = 1
                UNION ALL
                SELECT e.seq, 0, {terminal},
                       CASE WHEN ?5 IS NOT NULL
                             AND typeof(e.data) = 'text'
                             AND json_valid(e.data)
                             AND json_extract(e.data, '$.call') IN (SELECT call_id FROM local_preview_calls)
                            THEN 1 ELSE 0 END AS is_previewed
                  FROM event e
                 WHERE e.aggregate_family = 'session' AND e.aggregate_id = ?1
                   AND (e.seq >= ?2 OR e.seq < 0) AND (?3 IS NULL OR e.seq < ?3)
                   AND e.type = 'session.tool.terminal.1'
                   AND CASE
                         WHEN typeof(e.data) = 'text' AND json_valid(e.data)
                         THEN NOT EXISTS (SELECT 1 FROM event n
                           WHERE n.aggregate_family = 'session' AND n.aggregate_id = e.aggregate_id
                             AND n.type = 'session.tool.nested.started.1'
                             AND typeof(n.data) = 'text' AND json_valid(n.data)
                             AND json_extract(n.data, '$.attempt') = json_extract(e.data, '$.attempt')
                             AND json_extract(n.data, '$.call') = json_extract(e.data, '$.call'))
                         ELSE 1
                       END = 1)
             ORDER BY seq"
        ))
        .map_err(probe)?;
    let mut rows = statement
        .query((session.as_str(), from, until, cap_sql, unpreviewed_json))
        .map_err(probe)?;
    let mut total = 0f64;
    while let Some(row) = rows.next().map_err(probe)? {
        let seq: i64 = row.get(0).map_err(probe)?;
        let terminal = crate::stored_terminal::classify_optional(row, 2).map_err(probe)?;
        total += match terminal {
            Some(terminal) => {
                let is_previewed: i64 = row.get(3).map_err(probe)?;
                let row_cap = if is_previewed != 0 { preview_cap } else { cap };
                terminal_bytes(terminal).min(row_cap) as f64
            }
            None => row.get::<_, f64>(1).map_err(probe)?,
        };
        if total > limit as f64 {
            return decoded(Some(seq), "structural boundary has a negative sequence");
        }
    }
    Ok(None)
}

/// The output a stored terminal declares; an unreadable one declares none.
fn terminal_bytes(terminal: grapt_event::session_tool_terminal::StoredTerminal) -> usize {
    use grapt_event::session_tool_terminal::StoredTerminal;
    use grapt_types::ToolTerminalOutcome;
    let StoredTerminal::Readable(fact) = terminal else {
        return 0;
    };
    let artifact = match fact.outcome() {
        ToolTerminalOutcome::Completed { output } => output.as_ref(),
        ToolTerminalOutcome::Failed { detail, .. } => detail.as_ref(),
        _ => None,
    };
    artifact.map_or(0, |artifact| {
        usize::try_from(artifact.reference().length()).unwrap_or(usize::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ScratchDir(std::path::PathBuf);
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open_fixture() -> Result<(EventStore, rusqlite::Connection, SessionId, ScratchDir), String> {
        static ORDINAL: AtomicUsize = AtomicUsize::new(0);
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "grapt-boundary-test-{}-{}",
                std::process::id(),
                ORDINAL.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        };
        let scratch = ScratchDir(root.clone());
        let database = root.join("events.sqlite3");
        let store =
            EventStore::open(&database, "boundary-test").map_err(|error| error.to_string())?;
        let raw = rusqlite::Connection::open(&database).map_err(|error| error.to_string())?;
        raw.execute_batch(
            "CREATE TABLE session_message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL,
                text TEXT NOT NULL, delivery TEXT NOT NULL, seq INTEGER NOT NULL,
                description TEXT, metadata_json TEXT
            );",
        )
        .map_err(|error| error.to_string())?;
        let session = "ses_boundary_test"
            .parse::<SessionId>()
            .map_err(|error| error.to_string())?;
        raw.execute(
            "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES ('session', ?1, 0)",
            [session.as_str()],
        )
        .map_err(|error| error.to_string())?;
        Ok((store, raw, session, scratch))
    }

    #[test]
    fn undecodable_terminal_rows_charge_zero_bytes() -> Result<(), String> {
        let (store, raw, session, _scratch) = open_fixture()?;
        let corrupt_rows = [
            r#"{"outcome":{"output":{"length":1000000}}}"#,
            r#"{"call":"c1","outcome":{"kind":"completed","output":{"length":1000000}}}"#,
            r#"{"attempt":"a1","outcome":{"kind":"completed","output":{"length":1000000}}}"#,
            r#"{"attempt":"a2","call":"c2","outcome":{"output":{"length":1000000}}}"#,
            r#"{"attempt":"a3","call":"c3","outcome":{"kind":"bogus","detail":{"length":1000000}}}"#,
        ];
        for (index, payload) in corrupt_rows.iter().enumerate() {
            let seq = i64::try_from(index + 1).map_err(|_| "seq overflow")?;
            raw.execute(
                "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
                 VALUES (?1, 'session', ?2, ?3, 0, 'session.tool.terminal.1', ?4, 1)",
                (format!("ev_corrupt_{seq}"), session.as_str(), seq, *payload),
            )
            .map_err(|error| error.to_string())?;
        }

        let over = declared_bytes_over(
            &store,
            &session,
            None,
            None,
            1024 * 1024,
            4 * 1024 * 1024,
            Some(PreviewBudget {
                cap: 32 * 1024,
                unpreviewed_tools: &["read"],
            }),
        )?;
        assert_eq!(over, None);
        let strict = declared_bytes_over(&store, &session, None, None, 1024 * 1024, 0, None)?;
        assert_eq!(strict, None);
        Ok(())
    }

    #[test]
    fn failed_terminals_missing_reason_charge_zero_bytes() -> Result<(), String> {
        let (store, raw, session, _scratch) = open_fixture()?;
        for seq in 1i64..=5 {
            let payload = format!(
                r#"{{"attempt":"pat_{seq}","call":"call_{seq}","outcome":{{"kind":"failed","detail":{{"id":"art_{seq}","length":1000000,"digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","disposition":"complete"}}}}}}"#
            );
            raw.execute(
                "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
                 VALUES (?1, 'session', ?2, ?3, 0, 'session.tool.terminal.1', ?4, 1)",
                (format!("ev_missing_reason_{seq}"), session.as_str(), seq, payload),
            )
            .map_err(|error| error.to_string())?;
        }

        let over = declared_bytes_over(
            &store,
            &session,
            None,
            None,
            1024 * 1024,
            4 * 1024 * 1024,
            Some(PreviewBudget {
                cap: 32 * 1024,
                unpreviewed_tools: &["read"],
            }),
        )?;
        assert_eq!(over, None);
        let strict = declared_bytes_over(&store, &session, None, None, 1024 * 1024, 0, None)?;
        assert_eq!(strict, None);
        Ok(())
    }

    #[test]
    fn malformed_terminal_alongside_valid_nested_start_does_not_abort_query() -> Result<(), String>
    {
        let (store, raw, session, _scratch) = open_fixture()?;
        raw.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES ('ev_nested_1', 'session', ?1, 1, 0, 'session.tool.nested.started.1', ?2, 1)",
            (
                session.as_str(),
                r#"{"attempt":"pat_nested","call":"call_nested"}"#,
            ),
        )
        .map_err(|error| error.to_string())?;
        raw.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES ('ev_malformed_term', 'session', ?1, 2, 0, 'session.tool.terminal.1', '{not-json', 1)",
            [session.as_str()],
        )
        .map_err(|error| error.to_string())?;

        let over = declared_bytes_over(
            &store,
            &session,
            None,
            None,
            1024 * 1024,
            4 * 1024 * 1024,
            Some(PreviewBudget {
                cap: 32 * 1024,
                unpreviewed_tools: &["read"],
            }),
        )?;
        assert_eq!(over, None);
        Ok(())
    }

    #[test]
    fn invalid_utf8_terminal_text_charges_zero_bytes_without_aborting() -> Result<(), String> {
        let (store, raw, session, _scratch) = open_fixture()?;
        raw.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES ('ev_bad_utf8', 'session', ?1, 1, 0, 'session.tool.terminal.1',
                     CAST(X'7b2261223a22ff227d' AS TEXT), 1)",
            [session.as_str()],
        )
        .map_err(|error| error.to_string())?;
        let valid: i64 = raw
            .query_row(
                "SELECT json_valid(data) FROM event WHERE id = 'ev_bad_utf8'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(valid, 1);

        let over = declared_bytes_over(
            &store,
            &session,
            None,
            None,
            1024 * 1024,
            0,
            Some(PreviewBudget {
                cap: 32 * 1024,
                unpreviewed_tools: &["read"],
            }),
        )?;
        assert_eq!(over, None);
        Ok(())
    }

    #[test]
    fn fractional_real_content_length_below_budget_does_not_abort() -> Result<(), String> {
        let (store, raw, session, _scratch) = open_fixture()?;
        raw.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
             VALUES ('ev_real_len', 'session', ?1, 1, 0, 'session.content.appended.1',
                     '{\"item\":{\"kind\":\"text\",\"payload\":{\"length\":12.5}}}', 1)",
            [session.as_str()],
        )
        .map_err(|error| error.to_string())?;

        let under = declared_bytes_over(&store, &session, None, None, 1024 * 1024, 100, None)?;
        assert_eq!(under, None);
        let over = declared_bytes_over(&store, &session, None, None, 1024 * 1024, 12, None)?;
        assert_eq!(over, Some(EventSeq::new(1)));
        Ok(())
    }
}
