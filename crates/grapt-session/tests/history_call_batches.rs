//! BR-H: selected-call batching retains the exact reader's truth and refusals.
use grapt_event::session_message::{encode_appended, ContentAppendedFact};
use grapt_session::{HistoryReadError, SessionHistoryStore, SessionToolCallPage};
use grapt_store::EventStore;
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, ContentKind, ContentOrdinal,
    MessageId, NormalizedContentItem, SessionId, ToolCallId,
};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn fixture() -> Result<(Connection, SessionId)> {
    let source = EventStore::open_in_memory()?;
    let mut conn = Connection::open_in_memory()?;
    // Copy the actual bundled event schema/indexes, then use an independent
    // connection to plant corruption the production write authorizer forbids.
    let backup = Backup::new(source.connection(), &mut conn)?;
    if !matches!(backup.step(4096)?, StepResult::Done) {
        return Err("empty canonical schema exceeded the bounded backup".into());
    }
    drop(backup);
    let session = "ses_br_history_batch".parse()?;
    conn.execute("INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES ('session', ?1, 0)", [SessionId::as_str(&session)])?;
    Ok((conn, session))
}

fn call(raw: &str) -> Result<ToolCallId> {
    ToolCallId::new(raw).ok_or_else(|| "fixture call identity is invalid".into())
}

fn append(conn: &Connection, session: &SessionId, seq: i64, target: &ToolCallId) -> Result<()> {
    let ordinal = u64::try_from(seq)?;
    let fact = ContentAppendedFact::new(
        "msg_br_history_batch".parse::<MessageId>()?,
        NormalizedContentItem::new(
            ContentOrdinal::new(ordinal),
            ContentKind::Tool {
                call: target.clone(),
            },
            ArtifactRef::new(
                ArtifactId::new(format!("art_batch_{seq}")).ok_or("artifact identity")?,
                1,
                ArtifactDigest::new("sha256:aa").ok_or("digest")?,
            ),
        ),
        CaptureDisposition::Complete,
    )?;
    conn.execute("INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, 'session', ?2, ?3, ?3, 'session.content.appended.1', ?4, 1)",
        (format!("evt_br_batch_{seq}"), session.as_str(), seq, encode_appended(&fact).to_string()))?;
    Ok(())
}

fn exact_or_shared(
    history: &SessionHistoryStore<'_>,
    session: &SessionId,
    calls: &[ToolCallId],
) -> Result<Vec<std::result::Result<SessionToolCallPage, HistoryReadError>>> {
    Ok(match history.tool_call_pages_for(session, calls)? {
        Some(pages) => pages,
        None => calls
            .iter()
            .map(|call| history.tool_call_page_for(session, call))
            .collect(),
    })
}

fn comparable(
    page: std::result::Result<SessionToolCallPage, HistoryReadError>,
) -> std::result::Result<SessionToolCallPage, String> {
    page.map_err(|error| error.to_string())
}

#[test]
fn br_h_selected_pages_preserve_order_absence_duplicates_and_family_isolation() -> Result<()> {
    let (conn, session) = fixture()?;
    let calls = [
        call("call_first")?,
        call("call_last")?,
        call("call_absent")?,
        call("call_first")?,
    ];
    append(&conn, &session, 1, &calls[0])?;
    append(&conn, &session, 3, &calls[1])?;
    conn.execute("INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES ('operation', ?1, 0), ('session', 'ses_other_batch', 0)", [session.as_str()])?;
    conn.execute("INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES ('evt_foreign_family', 'operation', ?1, 2, 2, 'session.content.appended.1', '{bad', 1)", [session.as_str()])?;
    conn.execute("INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES ('evt_foreign_session', 'session', 'ses_other_batch', 2, 2, 'session.content.appended.1', '{bad', 1)", [])?;
    conn.execute("INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES ('evt_other_version', 'session', ?1, 4, 4, 'session.content.appended.2', '{bad', 1)", [session.as_str()])?;
    let history = SessionHistoryStore::new(&conn);
    let pages = history
        .tool_call_pages_for(&session, &calls)?
        .ok_or("small selected pages did not share their read")?;
    assert_eq!(pages.len(), calls.len());
    for (index, (target, page)) in calls.iter().zip(pages).enumerate() {
        let page = page?;
        assert_eq!(page, history.tool_call_page_for(&session, target)?);
        assert_eq!(page.rows().len(), usize::from(index != 2));
        if let Some(row) = page.rows().first() {
            assert_eq!(row.call(), target);
        }
    }
    Ok(())
}

#[test]
fn br_h_ambiguous_and_malformed_rows_cannot_resurrect_an_empty_page() -> Result<()> {
    for malformed in [
        "{bad",
        r#"{"item":{"kind":"alien","call":"other"}}"#,
        r#"{"item":{"kind":"tool"}}"#,
        r#"{"item":{"kind":"tool","call":42}}"#,
    ] {
        let (conn, session) = fixture()?;
        let calls = [call("call_selected")?, call("call_absent")?];
        append(&conn, &session, 1, &calls[0])?;
        conn.execute("UPDATE event SET data = ?1 WHERE seq = 1", [malformed])?;
        let history = SessionHistoryStore::new(&conn);
        for (target, page) in calls
            .iter()
            .zip(exact_or_shared(&history, &session, &calls)?)
        {
            assert!(
                matches!(page, Err(HistoryReadError::Corrupt { seq: 1, .. })),
                "owned corruption disappeared for {target}: {page:?}"
            );
            assert_eq!(
                comparable(page),
                comparable(history.tool_call_page_for(&session, target))
            );
        }
    }
    Ok(())
}

#[test]
fn br_h_overflow_keeps_each_target_bound_and_corruption_precedence() -> Result<()> {
    let (conn, session) = fixture()?;
    let calls = [call("call_many")?, call("call_one")?];
    let bound = grapt_session::MAX_TOOL_CALL_PAGE_ROWS as i64;
    for seq in 1..=bound + 1 {
        append(&conn, &session, seq, &calls[0])?;
    }
    append(&conn, &session, bound + 2, &calls[1])?;
    let history = SessionHistoryStore::new(&conn);
    assert!(history.tool_call_pages_for(&session, &calls)?.is_none());
    assert!(matches!(
        history.tool_call_page_for(&session, &calls[0]),
        Err(HistoryReadError::ToolCallPageTooLong { limit, .. })
            if limit == grapt_session::MAX_TOOL_CALL_PAGE_ROWS
    ));
    assert_eq!(
        history
            .tool_call_page_for(&session, &calls[1])?
            .rows()
            .len(),
        1
    );
    conn.execute("UPDATE event SET data = '{bad' WHERE seq = 1", [])?;
    let pages = exact_or_shared(&history, &session, &calls)?;
    assert!(matches!(
        pages[0],
        Err(HistoryReadError::ToolCallPageTooLong { limit, .. })
            if limit == grapt_session::MAX_TOOL_CALL_PAGE_ROWS
    ));
    assert!(matches!(
        pages[1],
        Err(HistoryReadError::Corrupt { seq: 1, .. })
    ));
    for (target, page) in calls.iter().zip(pages) {
        assert_eq!(
            comparable(page),
            comparable(history.tool_call_page_for(&session, target))
        );
    }
    Ok(())
}

#[test]
fn br_h_shared_byte_cap_and_read_failure_are_distinct_from_absence() -> Result<()> {
    let (conn, session) = fixture()?;
    let calls = [call("call_selected")?, call("call_absent")?];
    let history = SessionHistoryStore::new(&conn);
    let pages = history
        .tool_call_pages_for(&session, &calls)?
        .ok_or("absence became fallback")?;
    assert!(pages
        .iter()
        .all(|page| page.as_ref().is_ok_and(|page| page.rows().is_empty())));
    append(&conn, &session, 1, &calls[0])?;
    conn.execute(
        "UPDATE event SET data = json_set(data, '$.padding', ?1) WHERE seq = 1",
        ["x".repeat(600 * 1024)],
    )?;
    assert!(
        history.tool_call_pages_for(&session, &calls)?.is_none(),
        "oversized sharing retained page-sized copies"
    );
    for (target, page) in calls
        .iter()
        .zip(exact_or_shared(&history, &session, &calls)?)
    {
        assert_eq!(
            comparable(page),
            comparable(history.tool_call_page_for(&session, target))
        );
    }
    conn.execute_batch("DROP TABLE event")?;
    assert!(
        matches!(
            history.tool_call_pages_for(&session, &calls),
            Err(HistoryReadError::Store { .. })
        ),
        "a read failure became fallback or absence"
    );
    assert!(matches!(
        history.tool_call_page_for(&session, &calls[1]),
        Err(HistoryReadError::Store { .. })
    ));
    Ok(())
}

#[test]
fn br_h_negative_headers_refuse_only_the_selected_target() -> Result<()> {
    let (conn, session) = fixture()?;
    let calls = [call("call_selected")?, call("call_absent")?];
    append(&conn, &session, 1, &calls[0])?;
    conn.execute("UPDATE event SET seq = -1 WHERE seq = 1", [])?;
    let history = SessionHistoryStore::new(&conn);
    let pages = exact_or_shared(&history, &session, &calls)?;
    assert!(matches!(
        pages[0],
        Err(HistoryReadError::Corrupt { seq: -1, .. })
    ));
    assert!(pages[1].as_ref().is_ok_and(|page| page.rows().is_empty()));
    for (target, page) in calls.iter().zip(pages) {
        assert_eq!(
            comparable(page),
            comparable(history.tool_call_page_for(&session, target))
        );
    }
    Ok(())
}
