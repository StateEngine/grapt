//! The history generation is the Session's own head, never another family's.
//!
//! The probe used to read `MAX(seq)` over `event` with no family filter, so a
//! row that merely shared the identity string could advance a Session's
//! generation. It now reads the Session-scoped head; this witness holds a
//! later foreign-family row next to it.

use super::head_generation;
use grapt_store::EventStore;
use grapt_types::SessionId;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0)
            .unwrap_or_else(|error| panic!("generation fixture cleanup failed: {error}"));
    }
}

fn fixture(label: &str) -> TestResult<(EventStore, Scratch, rusqlite::Connection)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ORDINAL: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "grapt-generation-{}-{label}-{}",
        std::process::id(),
        ORDINAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root)?;
    let scratch = Scratch(root.clone());
    let database = root.join("events.sqlite3");
    let store = EventStore::open(&database, "history-generation")?;
    let raw = rusqlite::Connection::open(&database)?;
    Ok((store, scratch, raw))
}

fn aggregate(conn: &rusqlite::Connection, family: &str, id: &str) -> TestResult {
    conn.execute(
        "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES (?1, ?2, 0)",
        (family, id),
    )?;
    Ok(())
}

fn insert(conn: &rusqlite::Connection, family: &str, id: &str, seq: i64) -> TestResult {
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES (?1, ?2, ?3, ?4, 0, 'session.updated.1', '{}', 1)",
        (
            format!("evt_generation_{family}_{id}_{seq}"),
            family,
            id,
            seq,
        ),
    )?;
    Ok(())
}

#[test]
fn a_foreign_family_row_does_not_move_the_session_generation() -> TestResult {
    let (store, _scratch, raw) = fixture("foreign-family")?;
    let session: SessionId = "ses_generation_owner".parse()?;
    aggregate(&raw, "session", session.as_str())?;
    insert(&raw, "session", session.as_str(), 0)?;
    insert(&raw, "session", session.as_str(), 4)?;
    // Another family carries the same identity string, later than the Session's
    // own head. Only the Session's rows are its history.
    aggregate(&raw, "form", session.as_str())?;
    insert(&raw, "form", session.as_str(), 9)?;
    assert_eq!(
        head_generation(&store, &session)?.value(),
        4,
        "a foreign-family row advanced the Session's history generation"
    );
    Ok(())
}

#[test]
fn a_session_with_no_rows_has_no_generation_to_advance_from() -> TestResult {
    let (store, _scratch, raw) = fixture("absent-head")?;
    let session: SessionId = "ses_generation_absent".parse()?;
    // The aggregate cursor exists but the log holds no fact: the probe must
    // report absence rather than position zero.
    aggregate(&raw, "session", session.as_str())?;
    let error = head_generation(&store, &session)
        .err()
        .ok_or_else(|| "an empty log reported a generation".to_owned())?;
    assert!(
        error.reason.contains("no head to advance from"),
        "an empty log was not refused by name: {}",
        error.reason
    );
    Ok(())
}
