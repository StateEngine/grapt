//! Position probes must keep absent, negative and zero distinct.
//!
//! These are the public `session_event_queries::bounds` seam, exercised over a
//! real store file through their own connection: the probes read rows a hostile
//! or drifting writer could leave, never only rows this process appended.

use grapt_store::session_event_queries::bounds;
use grapt_store::EventStore;
use grapt_types::{EventSeq, SessionId};
use rusqlite::Connection;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0)
            .unwrap_or_else(|error| panic!("bounds fixture cleanup failed: {error}"));
    }
}

struct Fixture {
    store: EventStore,
    session: SessionId,
    raw: Connection,
    _scratch: Scratch,
}

fn fixture(label: &str) -> TestResult<Fixture> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ORDINAL: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "grapt-store-bounds-{}-{label}-{}",
        std::process::id(),
        ORDINAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root)?;
    let scratch = Scratch(root.clone());
    let database = root.join("events.sqlite3");
    let store = EventStore::open(&database, "store-bounds")?;
    let raw = Connection::open(&database)?;
    // The promoted-input probe reads the inbox projection, which a store-only
    // fixture does not otherwise create. Only the columns it names are needed.
    raw.execute_batch(
        "CREATE TABLE session_message (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL,
            text TEXT NOT NULL, delivery TEXT NOT NULL, seq INTEGER NOT NULL,
            description TEXT, metadata_json TEXT
        );",
    )?;
    let session: SessionId = "ses_store_bounds".parse()?;
    aggregate(&raw, "session", session.as_str())?;
    Ok(Fixture {
        store,
        session,
        raw,
        _scratch: scratch,
    })
}

fn aggregate(conn: &Connection, family: &str, id: &str) -> TestResult {
    conn.execute(
        "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES (?1, ?2, 0)",
        (family, id),
    )?;
    Ok(())
}

fn insert(conn: &Connection, family: &str, id: &str, seq: i64, kind: &str) -> TestResult {
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, '{}', 1)",
        (
            format!("evt_bounds_{family}_{id}_{seq}"),
            family,
            id,
            seq,
            kind,
        ),
    )?;
    Ok(())
}

#[test]
fn absent_negative_and_zero_positions_stay_distinct() -> TestResult {
    let fixture = fixture("positions")?;
    let (store, session, raw) = (&fixture.store, &fixture.session, &fixture.raw);
    // An empty log has no head at all: absence is not position zero.
    assert_eq!(bounds::head(store, session)?, None);
    assert_eq!(
        bounds::visible_head_before(store, session, EventSeq::new(10))?,
        None
    );
    assert_eq!(
        bounds::promoted_after(store, session, EventSeq::new(0))?,
        None
    );
    assert_eq!(
        bounds::first_content_after(store, session, EventSeq::new(0))?,
        None
    );
    assert_eq!(
        bounds::execution_start_before(store, session, EventSeq::new(10))?,
        None
    );
    insert(raw, "session", session.as_str(), 0, "session.created.1")?;
    // Zero is a real position, distinct from absence.
    assert_eq!(bounds::head(store, session)?, Some(EventSeq::new(0)));
    assert!(!bounds::execution_started_between(
        store,
        session,
        EventSeq::new(0),
        EventSeq::new(1)
    )?);
    // A stored negative position is corruption, never an absent head.
    let corrupt: SessionId = "ses_store_bounds_corrupt".parse()?;
    aggregate(raw, "session", corrupt.as_str())?;
    insert(raw, "session", corrupt.as_str(), -3, "session.updated.1")?;
    let refused = bounds::head(store, &corrupt);
    assert!(
        refused.is_err(),
        "a negative stored sequence passed as a position: {refused:?}"
    );
    Ok(())
}

#[test]
fn execution_started_between_uses_an_open_window() -> TestResult {
    let fixture = fixture("window")?;
    let (store, session, raw) = (&fixture.store, &fixture.session, &fixture.raw);
    for seq in [1, 3, 5] {
        insert(raw, "session", session.as_str(), seq, "session.updated.1")?;
    }
    insert(
        raw,
        "session",
        session.as_str(),
        4,
        "session.execution.started.1",
    )?;
    // The window is open at both ends: exactly one start lies strictly inside.
    assert!(bounds::execution_started_between(
        store,
        session,
        EventSeq::new(3),
        EventSeq::new(5)
    )?);
    assert!(
        !bounds::execution_started_between(store, session, EventSeq::new(4), EventSeq::new(5))?,
        "the lower bound was treated as included"
    );
    assert!(
        !bounds::execution_started_between(store, session, EventSeq::new(3), EventSeq::new(4))?,
        "the upper bound was treated as included"
    );
    assert!(
        !bounds::execution_started_between(store, session, EventSeq::new(5), EventSeq::new(1))?,
        "an inverted window reported a start"
    );
    assert!(
        !bounds::execution_started_between(store, session, EventSeq::new(4), EventSeq::new(4))?,
        "an empty window reported a start"
    );
    Ok(())
}

#[test]
fn head_ignores_a_row_in_another_aggregate_family() -> TestResult {
    let fixture = fixture("family")?;
    let (store, session, raw) = (&fixture.store, &fixture.session, &fixture.raw);
    insert(raw, "session", session.as_str(), 2, "session.updated.1")?;
    // Another family may carry the same identity string, at a later position.
    aggregate(raw, "form", session.as_str())?;
    insert(raw, "form", session.as_str(), 9, "form.created.1")?;
    assert_eq!(
        bounds::head(store, session)?,
        Some(EventSeq::new(2)),
        "another family's position moved the Session's head"
    );
    Ok(())
}
