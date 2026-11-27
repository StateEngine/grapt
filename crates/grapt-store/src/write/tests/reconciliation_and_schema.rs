use super::*;
/// The raw `event_sequence` cursor, read straight from the table.
///
/// Deliberately not through [`read_sequence_cursor`]: a mutation inside the
/// store's own read path would otherwise also move the measurement, and a
/// witness whose ruler is made of the thing under test measures nothing.
fn stored_cursor(
    store: &EventStore,
    aggregate: &AggregateId,
) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    let found: Option<i64> = store
        .connection()
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_family = ?1 AND aggregate_id = ?2",
            rusqlite::params![
                crate::election::family_of(aggregate),
                crate::election::identity_of(aggregate)
            ],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e)
            }
        })?;
    match found {
        None => Ok(None),
        Some(value) => Ok(Some(u64::try_from(value)?)),
    }
}

/// The store's own initializer enables foreign keys, proven WITHOUT
/// relying on the driver default.
///
/// The precondition is the whole witness. `rusqlite` enables foreign keys
/// by default, so a test that merely opened a store and staged an orphan
/// row would pass today with no initializer in the tree at all — it would
/// be witnessing the dependency rather than this crate. This defeats the
/// default first and asserts the defeat numerically, then hands the
/// connection to the real prepared-open path.
#[test]
fn the_store_enables_foreign_keys_on_a_connection_whose_default_was_off() -> TestResult {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("PRAGMA foreign_keys = OFF")?;
    let before: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    assert_eq!(
        before, 0,
        "precondition: the driver default must be defeated before the store runs, \
         or this witness proves rusqlite rather than grapt-store"
    );

    let token = grapt_types::IdToken::new("fk-independence-token")
        .ok_or("the witness token must be constructible")?;
    let store = EventStore::open_prepared(conn, "in-memory", &FixedToken(token))?;

    let after: i64 = store
        .connection()
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    assert_eq!(
        after, 1,
        "the store's own initializer must re-enable foreign-key enforcement"
    );

    // The CONSEQUENCE, not merely the pragma's value: a parentless event
    // row is refused by the database, and by the foreign key specifically.
    let aggregate = session("ses_fkindep")?;
    let orphan = hostile_rows::install_event(
        store.connection(),
        store.gate(),
        hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: "evt_fk_orphan",
            seq: 0,
            created_ms: 1786060800000,
            versioned_type: "session.input.admitted.1",
            data_text: "{}",
            writer_epoch: Some(1),
        },
    );
    match orphan {
        Err(rusqlite::Error::SqliteFailure(code, _)) => assert_eq!(
            code.extended_code,
            rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
            "an event row with no event_sequence parent must be refused by the \
             FOREIGN KEY, not by some other constraint that happens to fire first"
        ),
        other => panic!("an orphan event row must be refused, got {other:?}"),
    }

    // SCOPE INVERSION. Same row shape, same gate, same tables, same
    // connection — only enforcement differs. Without this arm the refusal
    // above could be attributed to anything else about the row.
    store
        .connection()
        .execute_batch("PRAGMA foreign_keys = OFF")?;
    hostile_rows::install_event(
        store.connection(),
        store.gate(),
        hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: "evt_fk_orphan_control",
            seq: 1,
            created_ms: 1786060800000,
            versioned_type: "session.input.admitted.1",
            data_text: "{}",
            writer_epoch: Some(1),
        },
    )
    .map_err(|e| {
        format!(
            "with enforcement off the identical orphan row must be ACCEPTED, so that \
             the refusal above is attributable to the foreign key and not the row: {e}"
        )
    })?;
    Ok(())
}

/// The inspection is a READ: it classifies the cursor it found and does not
/// move it.
#[test]
fn inspecting_a_lost_writer_consumes_no_sequence_number() -> TestResult {
    let (mut store, aggregate) = elected_store()?;
    // A cursor with no event behind it — the state the inspection exists to
    // find, and one the public append path cannot produce because it writes
    // the cursor and the event in one transaction.
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, 5)?;

    let cursor_before = stored_cursor(&store, &aggregate)?;
    assert_eq!(
        cursor_before,
        Some(5),
        "precondition: the staged cursor is 5 before the inspection"
    );

    let _state = store.inspect_lost_writer(&aggregate)?;

    let cursor_after = stored_cursor(&store, &aggregate)?;
    assert_eq!(
        cursor_after, cursor_before,
        "a diagnostic inspection must allocate nothing: the cursor moved from \
         {cursor_before:?} to {cursor_after:?}, and that sequence number is now \
         lost to the aggregate forever"
    );
    Ok(())
}

/// A healthy aggregate is reported Settled, never as needing reconciliation.
///
/// The inverse of the arm below, and it is a real failure direction rather
/// than symmetry for its own sake: a classifier that demanded reconciliation
/// for a perfectly consistent database would push an operator to quarantine
/// or re-drive healthy work.
#[test]
fn an_aggregate_whose_cursor_matches_its_head_is_settled() -> TestResult {
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let data = json!({"sessionID":"ses_w0","inputID":"msg_settled"});
    append_only(
        &mut store,
        input(writer, &aggregate, &"evt_settled".parse()?, &data),
        &mut [],
    )?;

    match store.inspect_lost_writer(&aggregate)? {
        LostWriterState::Settled { head, .. } => assert_eq!(
            head,
            Some(0),
            "a committed append leaves the cursor and the durable head in agreement"
        ),
        other => panic!("an aggregate whose last append committed is Settled, not {other:?}"),
    }
    Ok(())
}

/// A sequence was allocated and its event is not durable: reconciliation is
/// required, and the numbers that say so are reported exactly.
#[test]
fn an_allocated_sequence_with_no_durable_event_requires_reconciliation() -> TestResult {
    let (mut store, aggregate) = elected_store()?;
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, 7)?;

    match store.inspect_lost_writer(&aggregate)? {
        LostWriterState::ReconciliationRequired { cursor, head, .. } => {
            assert_eq!(cursor, Some(7), "the allocated cursor is reported exactly");
            assert_eq!(head, None, "and no event is durable behind it");
        }
        other => panic!(
            "a cursor with no durable event behind it must require reconciliation, \
             got {other:?}"
        ),
    }
    Ok(())
}

/// The election controls of the reconciliation grid.
///
/// Neither arm carries a manifest row, and that is recorded rather than
/// implied. The Retained arm's exact mutation is ALREADY row B07 — the same
/// line and the same edit — so a second row would record one law twice.
/// `F3B4B_RESIDUALS.md` names B07 as this arm's inherited carrier. The
/// NoWriterElected arm is a boundary neither classifier row reaches.
#[test]
fn the_reconciliation_grid_keeps_its_election_controls() -> TestResult {
    let mut store = EventStore::open_in_memory()?;

    // An aggregate nobody ever elected is not the same finding as one that
    // finished cleanly.
    let never = session("ses_never")?;
    assert_eq!(
        store.inspect_lost_writer(&never)?,
        LostWriterState::NoWriterElected,
        "an aggregate with no election is distinct from a settled one"
    );

    // Re-election by the incumbent keeps the epoch, so the inspection below
    // cannot be reading a freshly advanced one.
    let aggregate = agg()?;
    let first = store.elect_writer(&aggregate)?;
    let again = store.elect_writer(&aggregate)?;
    assert_eq!(
        again,
        Election::Retained {
            epoch: first.epoch()
        },
        "a re-election by the incumbent retains its epoch"
    );
    match store.inspect_lost_writer(&aggregate)? {
        LostWriterState::Settled { epoch, .. } => assert_eq!(
            epoch,
            first.epoch(),
            "the inspection reports the elected epoch, unadvanced"
        ),
        other => panic!("a freshly elected aggregate with no writes is Settled, not {other:?}"),
    }
    Ok(())
}

#[test]
fn open_refuses_an_incompatible_preexisting_event_table() -> TestResult {
    // Review round 2, item E: a REAL EventStore::open over an alien
    // `event` table must fail and leave the database unowned.
    let dir = std::env::temp_dir().join(format!("grapt-open-alien-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("alien.db");
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY);
             CREATE TABLE event (wrong_column INTEGER)",
        )?;
    }
    assert!(
        EventStore::open(&db, "0.0.1").is_err(),
        "alien schema must refuse"
    );
    let conn = Connection::open(&db)?;
    let v: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, 0, "failed open leaves the database unowned");
    let attribution: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'engine_epoch'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(attribution, 0, "no attribution table left behind");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn open_refuses_a_pre_fencing_event_table() -> TestResult {
    // The shape this lane replaced is not "alien" in the obvious sense: it
    // is the store's OWN previous schema, and `CREATE TABLE IF NOT EXISTS`
    // accepts it silently. Opening it would append rows with no family and
    // no epoch column, so the column check must refuse it by name.
    let dir = std::env::temp_dir().join(format!("grapt-open-prefence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("prefencing.db");
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY);
             CREATE TABLE event_sequence (aggregate_id TEXT NOT NULL PRIMARY KEY, seq INTEGER NOT NULL);
             CREATE TABLE event (
                 id TEXT PRIMARY KEY,
                 aggregate_id TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 created INTEGER NOT NULL,
                 type TEXT NOT NULL,
                 data TEXT NOT NULL
             )",
        )?;
    }
    // The message matters, not merely the refusal. The no-new-v1 trigger's
    // own `CREATE` names `NEW.writer_epoch`, so it too would fail on this
    // table — asserting only `is_err()` would score that as this law being
    // enforced. The column check is given first refusal and named here.
    let message = match EventStore::open(&db, "0.0.1") {
        Ok(_) => panic!("a pre-fencing event table must refuse to open"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("missing column aggregate_family"),
        "the COLUMN check must be what refuses, got {message}"
    );
    let v: u32 = Connection::open(&db)?.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, 0, "the refusal left the database unowned");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// G13's witness: `CREATE TABLE IF NOT EXISTS` honours a wrong-shaped
/// namesake silently — the graph records a live hazard of exactly this
/// class for the legacy event indexes — so the open path must verify the
/// plan-read table's shape and refuse an alien one by name.
#[test]
fn open_refuses_a_wrong_shaped_plan_read_table() -> TestResult {
    let dir = std::env::temp_dir().join(format!("grapt-open-planread-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("planread.db");
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE plan_read_receipt (receipt_id TEXT PRIMARY KEY, payload TEXT)",
        )?;
    }
    let message = match EventStore::open(&db, "0.0.1") {
        Ok(_) => panic!("a wrong-shaped plan_read_receipt namesake must refuse to open"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("incompatible preexisting table plan_read_receipt"),
        "the COLUMN check must be what refuses, got: {message}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Crash cut 6 (R2C5A1): a wrong-shaped `attempt_receipt` namesake is
/// refused by the COLUMN check and the refusal NAMES the table.
///
/// `CREATE TABLE IF NOT EXISTS` silently honours a preexisting namesake, so
/// without this check a database that reached this shape by any other route
/// would run with a table that cannot hold a receipt at all — and the first
/// reserve would fail at the moment an external call was about to be made.
/// The ORDER matters too: the unsettled index names `settled_ms`, so on this
/// alien table the index would otherwise be what refused, and the message
/// would describe an index rather than a missing column.
#[test]
fn open_refuses_a_wrong_shaped_attempt_receipt_table() -> TestResult {
    let dir =
        std::env::temp_dir().join(format!("grapt-open-attemptreceipt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("attempt.db");
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE attempt_receipt (attempt_id TEXT PRIMARY KEY, payload TEXT)",
        )?;
    }
    let message = match EventStore::open(&db, "0.0.1") {
        Ok(_) => panic!("a wrong-shaped attempt_receipt namesake must refuse to open"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("incompatible preexisting table attempt_receipt"),
        "the COLUMN check must be what refuses, got: {message}"
    );
    assert!(
        message.contains("missing column session_id"),
        "and it must name the missing column, got: {message}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// The recovery index's COLUMN ORDER is verified at every open, not merely
/// requested.
///
/// `CREATE INDEX IF NOT EXISTS` silently honours a preexisting namesake, and
/// `PRAGMA table_info` cannot see an index at all — so a database carrying a
/// same-named index in another shape would pass every column check while the
/// store ran on the wrong access law. Here the namesake leads with
/// `attempt_id`, which cannot serve an equality on `session_id`.
///
/// This check proves the index matches its DECLARATION. It cannot prove the
/// declaration matches the query — the previous shape passed this test while
/// serving nothing. `the_recovery_scan_uses_its_index_rather_than_scanning`
/// in `attempt_receipt` is the other half, and asks SQLite directly.
#[test]
fn open_refuses_a_same_named_recovery_index_over_the_wrong_columns() -> TestResult {
    let dir = std::env::temp_dir().join(format!("grapt-open-attemptindex-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("attempt-index.db");
    {
        // The table itself is exactly the target shape, so the COLUMN check
        // cannot be what refuses. Only the index is wrong.
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE attempt_receipt (
                attempt_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                step INTEGER NOT NULL,
                ordinal INTEGER NOT NULL,
                writer_epoch INTEGER NOT NULL,
                terminal_event_id TEXT NOT NULL,
                terminal_created_ms INTEGER NOT NULL,
                reserved_ms INTEGER NOT NULL,
                request_sent_ms INTEGER,
                settled_ms INTEGER,
                record_bytes BLOB NOT NULL
             );
             CREATE INDEX attempt_receipt_recovery
                ON attempt_receipt (attempt_id, session_id);",
        )?;
    }
    match EventStore::open(&db, "0.0.1") {
        Err(WriteError::IndexShapeMismatch {
            index,
            found,
            expected,
            ..
        }) => {
            assert_eq!(index, "attempt_receipt_recovery");
            assert_eq!(found, ["attempt_id", "session_id"]);
            assert_eq!(expected, ["session_id", "attempt_id"]);
        }
        other => panic!(
            "a same-named recovery index over the wrong column order must \
             refuse the open, got {other:?}"
        ),
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
