//! The native wake scan reads open work only, and verifies only what it claims.
use super::*;
use grapt_event::AggregateId;
use grapt_store::MailBindingExpectation;
use grapt_types::{InstructionKey, SessionId};

fn session() -> Result<SessionId, Box<dyn std::error::Error>> {
    Ok("ses_native_wake".parse()?)
}
fn binding() -> Result<MailBindingExpectation, Box<dyn std::error::Error>> {
    Ok(MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound-coder".into(),
    )?)
}
/// Admit and settle one row, returning its local message id.
fn settled(
    store: &mut EventStore,
    id: &str,
) -> Result<grapt_types::MailboxMessageId, Box<dyn std::error::Error>> {
    let expected = scope()?;
    let ack = store.admit_remote_mailbox(&expected, &input(id)?)?.ack;
    store.settle_remote_mailbox_delivery(
        &expected,
        &ack,
        Some(SecondaryProjectionState::Posted),
    )?;
    Ok(ack.local_message().clone())
}

#[test]
fn a_wake_scan_never_decodes_consumed_history() -> TestResult {
    let db = OwnedDb::new("wake-consumed")?;
    let expected = scope()?;
    let mut consumed = Vec::new();
    {
        let mut store = db.open()?;
        // More than two whole scan pages of finished mail, then one open row.
        for index in 0..150 {
            let id = settled(&mut store, &format!("finished-{index}"))?;
            store.consume_remote_mailbox(&expected, &id)?;
            consumed.push(id);
        }
        settled(&mut store, "open")?;
    }
    // A scan that decoded any consumed row would now refuse it.
    for id in &consumed {
        mutate(&db.path(), id.as_str(), |bytes| bytes.push(0))?;
    }
    let store = db.open()?;
    let scan = store.prepare_recent_remote_mail_wake(
        &session()?,
        &expected,
        binding()?,
        None,
        MailboxCursor::START,
    )?;
    let claim = scan.claim.ok_or("the open row is a wake candidate")?;
    assert_eq!(
        claim
            .source_messages()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        ["open"]
    );
    assert!(
        !scan.pending,
        "consumed history left no further page to walk"
    );
    Ok(())
}

#[test]
fn a_candidate_with_a_broken_digest_is_refused_when_its_claim_is_consumed() -> TestResult {
    let db = OwnedDb::new("wake-digest")?;
    let expected = scope()?;
    let id = {
        let mut store = db.open()?;
        settled(&mut store, "tampered")?
    };
    // Same length and still decodable: only the immutable digest can tell.
    mutate(&db.path(), id.as_str(), |bytes| {
        let body = b"bounded complete mail";
        if let Some(at) = bytes.windows(body.len()).position(|window| window == body) {
            bytes[at] = b'c';
        }
    })?;
    let mut store = db.open()?;
    store.elect_writer(&AggregateId::Session(session()?))?;
    store.connection().execute_batch("CREATE TABLE instruction_entry(session_id TEXT,key TEXT,value TEXT,removed INTEGER); INSERT INTO instruction_entry VALUES('ses_native_wake','stateengine-identity','bound-coder',0);")?;
    let scan = store.prepare_recent_remote_mail_wake(
        &session()?,
        &expected,
        binding()?,
        None,
        MailboxCursor::START,
    )?;
    let claim = scan
        .claim
        .ok_or("the scan selects a candidate without re-hashing it")?;
    let tx = store.governed_transaction()?;
    let refused = claim.consume_within(&tx);
    assert!(
        refused
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("immutable digest")),
        "a claim must re-verify every selected row: {refused:?}"
    );
    Ok(())
}

/// The deferred mailbox indexes present on the file at `path`.
fn scan_indexes(path: &std::path::Path) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(rusqlite::Connection::open(path)?.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('mailbox_message_open_idx','mailbox_message_debt_idx')",
        [],
        |row| row.get(0),
    )?)
}

/// The source ids of the open, debt and recent open reads, in read order.
fn mail_reads(store: &EventStore) -> Result<[Vec<String>; 3], Box<dyn std::error::Error>> {
    let expected = scope()?;
    let ids = |records: &[grapt_store::RemoteMailboxRecord]| {
        records
            .iter()
            .map(|record| record.ack().source_message().as_str().to_owned())
            .collect::<Vec<_>>()
    };
    let read = |rows| {
        store.read_remote_mailbox_for_binding_rows(
            &expected,
            MailboxCursor::START,
            64,
            262144,
            rows,
        )
    };
    let open = read(grapt_store::HistoryRows::Open)?;
    let debt = read(grapt_store::HistoryRows::Debt)?;
    let recent = store.read_recent_remote_mailbox_for_binding_rows(
        &expected,
        None,
        64,
        262144,
        grapt_store::HistoryRows::OpenScan,
    )?;
    Ok([ids(&open.records), ids(&debt.records), ids(&recent.records)])
}

#[test]
fn the_mail_reads_return_the_same_rows_before_the_scan_indexes_exist() -> TestResult {
    // Open never builds these indexes; the service builds them after
    // readiness. The open-row, debt and recent reads must not need them.
    let db = OwnedDb::new("deferred-indexes")?;
    let expected = scope()?;
    let mut store = db.open()?;
    for index in 0..3 {
        let id = settled(&mut store, &format!("finished-{index}"))?;
        store.consume_remote_mailbox(&expected, &id)?;
    }
    settled(&mut store, "open")?;
    store.admit_remote_mailbox(&expected, &input("unacknowledged")?)?;
    assert_eq!(scan_indexes(&db.path())?, 0, "open does not build them");
    let without = mail_reads(&store)?;
    assert_eq!(
        without,
        [
            vec!["open".to_owned(), "unacknowledged".to_owned()],
            vec!["unacknowledged".to_owned()],
            vec!["unacknowledged".to_owned(), "open".to_owned()],
        ]
    );
    store.build_mailbox_scan_indexes()?;
    assert_eq!(scan_indexes(&db.path())?, 2);
    assert_eq!(mail_reads(&store)?, without, "same rows, same order");
    drop(store);
    // A reopen verifies the shapes of the indexes that are now present.
    drop(db.open()?);
    Ok(())
}

#[test]
fn a_store_carrying_an_index_it_does_not_know_still_opens() -> TestResult {
    // What a build older than the open-work index sees: an extra index on
    // mailbox_message. Shapes are verified by name, so it must not refuse.
    let db = OwnedDb::new("unknown-index")?;
    db.open()?.build_mailbox_scan_indexes()?;
    assert_eq!(scan_indexes(&db.path())?, 2);
    rusqlite::Connection::open(db.path())?.execute_batch(
        "CREATE INDEX mailbox_message_later_build_idx ON mailbox_message (kind, recipient);",
    )?;
    let mut store = db.open()?;
    settled(&mut store, "after-reopen")?;
    Ok(())
}

#[test]
fn recent_reads_and_the_wake_scan_never_read_below_their_mark() -> TestResult {
    let db = OwnedDb::new("wake-floor")?;
    let expected = scope()?;
    let mut ids = Vec::new();
    {
        let mut store = db.open()?;
        for id in ["first", "second", "third", "fourth"] {
            ids.push(settled(&mut store, id)?);
        }
    }
    let store = db.open()?;
    let all = store.read_recent_remote_mailbox_for_binding_rows(
        &expected,
        None,
        64,
        262144,
        grapt_store::HistoryRows::OpenScan,
    )?;
    let seqs: Vec<_> = all
        .records
        .iter()
        .map(|record| record.ack().local_admission())
        .collect();
    assert_eq!(all.top, seqs.first().copied(), "the top is the newest row");
    // The mark sits on the second row: the two rows at or below it are made
    // undecodable, so a read that touched either would refuse.
    let mark = MailboxCursor::after(*seqs.get(2).ok_or("four rows")?);
    drop(store);
    for id in &ids[..2] {
        mutate(&db.path(), id.as_str(), |bytes| bytes.push(0))?;
    }
    let store = db.open()?;
    let above = store.read_recent_remote_mailbox_above(
        &expected,
        None,
        mark,
        64,
        262144,
        grapt_store::HistoryRows::OpenScan,
    )?;
    let sources: Vec<_> = above
        .records
        .iter()
        .map(|record| record.ack().source_message().as_str().to_owned())
        .collect();
    assert_eq!(sources, ["fourth", "third"], "only rows above the mark");
    assert_eq!(
        above.next_before, None,
        "no row at or below the mark was read"
    );
    let scan =
        store.prepare_recent_remote_mail_wake(&session()?, &expected, binding()?, None, mark)?;
    let mut claimed: Vec<_> = scan
        .claim
        .as_ref()
        .ok_or("the rows above the mark are candidates")?
        .source_messages()
        .map(|id| id.as_str().to_owned())
        .collect();
    claimed.sort();
    assert_eq!(claimed, ["fourth", "third"]);
    assert!(!scan.pending, "the scan ends at the mark");
    let newest = all.top.ok_or("a top")?;
    let empty = store.prepare_recent_remote_mail_wake(
        &session()?,
        &expected,
        binding()?,
        None,
        MailboxCursor::after(newest),
    )?;
    assert!(empty.claim.is_none() && !empty.pending && empty.top.is_none());
    Ok(())
}

#[test]
fn a_deferred_index_built_after_open_cannot_be_reindexed() -> TestResult {
    // The authorizer is installed at open, before these indexes exist; they
    // are governed by name all the same, with no reopen.
    let db = OwnedDb::new("deferred-reindex")?;
    let mut store = db.open()?;
    store.build_mailbox_scan_indexes()?;
    for sql in [
        "REINDEX mailbox_message_open_idx",
        "REINDEX MAILBOX_MESSAGE_DEBT_IDX",
    ] {
        let result = store.connection().execute(sql, []);
        assert!(
            matches!(&result, Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::AuthorizationForStatementDenied),
            "{sql} must be denied: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn a_namesake_of_a_deferred_index_is_refused_at_open() -> TestResult {
    // SQLite index names are case-insensitive and schema-wide, so either
    // namesake would make the deferred CREATE INDEX IF NOT EXISTS keep it.
    for (label, sql, name) in [
        (
            "upper-case-namesake",
            "CREATE INDEX MAILBOX_MESSAGE_OPEN_IDX ON mailbox_message (kind)",
            "mailbox_message_open_idx",
        ),
        (
            "other-table-namesake",
            "CREATE TABLE side_table (recipient, source_acknowledged, admission_seq); CREATE INDEX mailbox_message_debt_idx ON side_table (recipient, source_acknowledged, admission_seq)",
            "mailbox_message_debt_idx",
        ),
    ] {
        let db = OwnedDb::new(label)?;
        drop(db.open()?);
        rusqlite::Connection::open(db.path())?.execute_batch(sql)?;
        match db.open() {
            Err(WriteError::IndexShapeMismatch { index, .. }) => assert_eq!(index, name),
            Err(other) => panic!("{label}: expected IndexShapeMismatch, got {other}"),
            Ok(_) => panic!("{label}: a store with a wrong-shaped namesake opened"),
        }
    }
    Ok(())
}
