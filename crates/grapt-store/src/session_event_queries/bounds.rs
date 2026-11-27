//! Durable session positions: where one Session's log ends, and the first row
//! past a position that a bounded request could not carry.
//!
//! These are position probes only. Payloads stay with the readers that own
//! their decoders, so a caller learns where a window stops without holding the
//! rows it refuses to read.
use crate::history_visibility::HistoryVisibility;
use crate::projection;
use crate::write::EventStore;
use grapt_event::AggregateId;
use grapt_types::{EventSeq, SessionId};

/// Absent, negative and zero stay distinct: an empty log has no head, while a
/// stored negative position is corruption.
fn position(seq: Option<i64>, reason: &str) -> Result<Option<EventSeq>, String> {
    seq.map(|value| {
        u64::try_from(value)
            .map(EventSeq::new)
            .map_err(|_| reason.to_owned())
    })
    .transpose()
}

fn stored(seq: EventSeq, reason: &str) -> Result<i64, String> {
    i64::try_from(seq.value()).map_err(|_| reason.to_owned())
}

/// The highest sequence present in this Session's `event` rows.
pub fn head(store: &EventStore, session: &SessionId) -> Result<Option<EventSeq>, String> {
    projection::head(store.connection(), &AggregateId::Session(session.clone()))
        .map(|head| head.map(EventSeq::new))
        .map_err(|error| format!("durable session head is unreadable: {error}"))
}

/// The highest visible sequence strictly before `before`, after the Session's
/// rewind cuts.
pub fn visible_head_before(
    store: &EventStore,
    session: &SessionId,
    before: EventSeq,
) -> Result<Option<EventSeq>, String> {
    let before = stored(before, "recovery position exceeds SQLite range")?;
    let visible = HistoryVisibility::read(store.connection(), session)
        .map_err(|error| error.to_string())?
        .sql("seq");
    let seq: Option<i64> = store
        .connection()
        .query_row(
            &format!(
                "SELECT MAX(seq) FROM event WHERE aggregate_family='session'
                   AND aggregate_id=?1 AND seq<?2 AND {visible}"
            ),
            (session.as_str(), before),
            |row| row.get(0),
        )
        .map_err(|error| format!("visible session head is unreadable: {error}"))?;
    position(seq, "visible session head has a negative sequence")
}

/// The first promoted input after `after`: the ceiling a request planned at
/// `after` could not carry.
pub fn promoted_after(
    store: &EventStore,
    session: &SessionId,
    after: EventSeq,
) -> Result<Option<EventSeq>, String> {
    let after = stored(after, "request position exceeds SQLite range")?;
    let seq: Option<i64> = store
        .connection()
        .query_row(
            "SELECT MIN(seq) FROM session_message
             WHERE session_id = ?1 AND type IN ('user', 'synthetic') AND seq > ?2",
            (session.as_str(), after),
            |row| row.get(0),
        )
        .map_err(|error| format!("request ceiling is unreadable: {error}"))?;
    position(seq, "promoted input position is negative")
}

/// The first row after `head` a request planned at `head` could not carry: an
/// input, content, a tool terminal, presented mail or a response opening.
pub fn first_content_after(
    store: &EventStore,
    session: &SessionId,
    head: EventSeq,
) -> Result<Option<EventSeq>, String> {
    let head = stored(head, "durable head exceeds SQLite range")?;
    let seq: Option<i64> = store
        .connection()
        .query_row(
            "SELECT MIN(seq) FROM (
                SELECT seq FROM session_message
                 WHERE session_id = ?1 AND type IN ('user', 'synthetic') AND seq > ?2
                UNION ALL
                SELECT seq FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND seq > ?2
                   AND (type IN ('session.content.appended.1', 'session.tool.terminal.1')
                        OR type LIKE 'session.assistant-message.opened.%'
                        OR type LIKE 'session.mail.presented.%'
                        OR type LIKE 'session.subagent-mail.presented.%'))",
            (session.as_str(), head),
            |row| row.get(0),
        )
        .map_err(|error| format!("request coverage bound is unreadable: {error}"))?;
    position(seq, "coverage bound is negative")
}

/// The latest execution start strictly before an assistant opening.
pub fn execution_start_before(
    store: &EventStore,
    session: &SessionId,
    opening: EventSeq,
) -> Result<Option<EventSeq>, String> {
    let before = stored(opening, "assistant opening exceeds SQLite range")?;
    let seq: Option<i64> = store
        .connection()
        .query_row(
            "SELECT (SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
               AND type='session.execution.started.1' AND seq<?2 ORDER BY seq DESC LIMIT 1)",
            (session.as_str(), before),
            |row| row.get(0),
        )
        .map_err(|error| format!("tool execution association is unreadable: {error}"))?;
    position(seq, "tool execution has a negative sequence")
}

/// Whether an execution started strictly between two positions. A window that
/// is empty or inverted never reports one, matching the open interval.
pub fn execution_started_between(
    store: &EventStore,
    session: &SessionId,
    after: EventSeq,
    before: EventSeq,
) -> Result<bool, String> {
    let after = stored(after, "recovery position exceeds SQLite range")?;
    let before = stored(before, "response position exceeds SQLite range")?;
    store
        .connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='session'
               AND aggregate_id=?1 AND type='session.execution.started.1' AND seq>?2 AND seq<?3)",
            (session.as_str(), after, before),
            |row| row.get(0),
        )
        .map_err(|error| format!("execution restart window is unreadable: {error}"))
}
