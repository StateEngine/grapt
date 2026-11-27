//! Message-projection cross-counts (task: message differential, v1 scope).
//!
//! Exact parities proven here; the user/assistant row derivations go through
//! the STATEFUL projector (red-team §B.2: `projector.ts` reads
//! SessionMessageTable mid-fold; `message-updater.ts` is 524 LOC of law) —
//! those are R3 scope and their deltas are pinned as measured facts, not
//! reproduced by guesswork.

#[path = "../../../test-support/evidence.rs"]
mod evidence;

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    match conn.query_row(sql, [], |r| r.get(0)) {
        Ok(n) => n,
        Err(e) => panic!("{sql}: {e}"),
    }
}

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run with --include-ignored on the evidence machine"]
fn message_projection_cross_counts_hold() {
    let path = evidence::frozen_clone();
    let conn = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => panic!("open: {e}"),
    };

    // Exact one-event-one-row parities.
    for (kind, msg_type) in [
        ("session.agent.selected.1", "agent-switched"),
        ("session.model.selected.1", "model-switched"),
        ("session.compaction.started.1", "compaction"),
    ] {
        let events = count(
            &conn,
            &format!("SELECT COUNT(*) FROM event WHERE type = '{kind}'"),
        );
        let rows = count(
            &conn,
            &format!("SELECT COUNT(*) FROM session_message WHERE type = '{msg_type}'"),
        );
        assert_eq!(events, rows, "{kind} must project 1:1 into {msg_type}");
    }

    // Measured facts pinned (deltas owned by the R3 projector law):
    // 885 promotions -> 783 user rows (retention/deletion gap: 305 event
    // aggregates vs 253 sessions with messages); 4,955 step starts ->
    // 4,802 assistant rows; 19 synthetic events vs 121 synthetic rows
    // (synthetic rows are minted by more than the synthetic event).
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM session_message WHERE type='user'"
        ),
        783
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM session_message WHERE type='assistant'"
        ),
        4_802
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM session_message WHERE type='synthetic'"
        ),
        121
    );
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(DISTINCT session_id) FROM session_message"
        ),
        253
    );
    eprintln!("MESSAGES OK: three exact parities + four pinned facts");
}
