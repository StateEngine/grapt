//! The durable seat mailbox, attacked from where a consumer actually stands.
//!
//! Integration tests on purpose: everything here reaches `grapt-store` through
//! its PUBLIC surface, which is the surface a future runtime drain will have.
//! A unit test inside the crate could name the write gate and lift the denial;
//! a consumer cannot, and that difference is the whole governance claim.
//!
//! Crash cuts are real cuts, not simulations: the work runs inside a governed
//! transaction that is DROPPED instead of committed, so SQLite rolls back
//! exactly as it would after a lost process, and the assertions are made on a
//! fresh read afterwards.

use grapt_store::{
    EventStore, MailboxAcknowledged, MailboxAdmitted, MailboxConsumed, MailboxPageEntry,
    WriteError, MAX_MAILBOX_PAGE_BYTES, MAX_MAILBOX_PAGE_MESSAGES,
};
use grapt_types::{
    MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor, ProtocolVersion,
    SeatMailboxMessage, SeatMessageKind, SeatRecipientId, MAX_MAILBOX_BODY_BYTES,
    MAX_MAILBOX_MESSAGE_ID_BYTES, MAX_SEAT_RECIPIENT_ID_BYTES,
};
use rusqlite::ErrorCode;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn recipient(id: &str) -> Result<SeatRecipientId, Box<dyn std::error::Error>> {
    SeatRecipientId::new(id).ok_or_else(|| "recipient".into())
}

fn message(
    id: &str,
    to: &str,
    body: &str,
) -> Result<SeatMailboxMessage, Box<dyn std::error::Error>> {
    message_superseding(id, to, body, None)
}

fn message_superseding(
    id: &str,
    to: &str,
    body: &str,
    supersedes: Option<&str>,
) -> Result<SeatMailboxMessage, Box<dyn std::error::Error>> {
    let target = match supersedes {
        Some(raw) => Some(MailboxMessageId::new(raw).ok_or("supersedes")?),
        None => None,
    };
    Ok(SeatMailboxMessage::new(
        ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
        MailboxMessageId::new(id).ok_or("id")?,
        recipient("seat-alpha")?,
        recipient(to)?,
        SeatMessageKind::TaskPointer,
        MailboxBody::new(body)?,
        target,
        MailboxSourceCursor::new(format!("src-{id}")).ok_or("source")?,
    )?)
}

/// Every actionable row id for one recipient, in page order: decoded
/// messages and the named unreadable rows alike.
fn actionable(store: &EventStore, to: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let page = store.read_actionable_mailbox(
        &recipient(to)?,
        MailboxCursor::START,
        MAX_MAILBOX_PAGE_MESSAGES,
    )?;
    Ok(page
        .entries
        .iter()
        .map(|entry| match entry {
            MailboxPageEntry::Message(message) => message.id().as_str().to_owned(),
            MailboxPageEntry::Unreadable(row) => row.message.clone(),
        })
        .collect())
}

/// The decoded message of one page row; refuses to paper over an unreadable
/// row a test did not stage.
fn page_message(entry: &MailboxPageEntry) -> &SeatMailboxMessage {
    match entry {
        MailboxPageEntry::Message(message) => message,
        MailboxPageEntry::Unreadable(row) => panic!("an unexpected unreadable row {row:?}"),
    }
}

fn hostile_record(
    id: &str,
    sender: &str,
    recipient: &str,
    supersedes: Option<&str>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    fn push_field(out: &mut Vec<u8>, value: &str) -> Result<(), Box<dyn std::error::Error>> {
        let len = u32::try_from(value.len())?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(value.as_bytes());
        Ok(())
    }

    let mut out = b"SEMBX1".to_vec();
    for value in [
        "mailbox-1",
        id,
        sender,
        recipient,
        "task-pointer",
        "hostile bound witness",
        "src-hostile",
    ] {
        push_field(&mut out, value)?;
    }
    match supersedes {
        Some(target) => {
            out.push(1);
            push_field(&mut out, target)?;
        }
        None => out.push(0),
    }
    Ok(out)
}

#[test]
fn a_record_addressed_elsewhere_is_refused_and_writes_nothing() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    // The expected recipient is supplied SEPARATELY from the record. A caller
    // that trusted the record's own recipient field would let a message
    // authorise its own delivery.
    let stray = message("msg-stray", "seat-delta", "not yours")?;
    match store.admit_mailbox_message(&recipient("seat-gamma")?, &stray) {
        Err(WriteError::MailboxWrongRecipient {
            message,
            expected,
            found,
        }) => {
            assert_eq!(message, "msg-stray");
            assert_eq!(expected, "seat-gamma");
            assert_eq!(found, "seat-delta");
        }
        other => panic!("a record addressed elsewhere must refuse typed: {other:?}"),
    }
    // Refused means nothing was written for EITHER seat.
    assert!(actionable(&store, "seat-gamma")?.is_empty());
    assert!(actionable(&store, "seat-delta")?.is_empty());
    Ok(())
}

#[test]
fn the_mailbox_insert_takes_the_governed_lift_for_its_own_table() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    // The store's own insert must be permitted by the store's own authorizer.
    // A lift naming a DIFFERENT governed table leaves the mailbox denial in
    // place, so the write the store believes it is making is refused at
    // statement preparation — the shape a lift meaning "governed writes are
    // open" instead of "this one table is open" would hide.
    //
    // The refusal is asserted with a named `panic!` rather than `?`: an
    // Err-returning witness leaves the harness no diagnostic to attribute, and
    // two rows whose evidence reads "failed without a captured diagnostic" are
    // indistinguishable — which is exactly how this witness came to exist.
    match store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?) {
        Ok(MailboxAdmitted::Admitted { seq }) => assert_eq!(seq.value(), 1),
        other => panic!("the mailbox insert must run under the lift for its OWN table: {other:?}"),
    }
    Ok(())
}

#[test]
fn two_admissions_take_distinct_ascending_positions() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    let first = match store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?) {
        Ok(admitted) => admitted.seq(),
        other => panic!("the first admission must take a position: {other:?}"),
    };
    // A collapsed sequence collides on UNIQUE (recipient, admission_seq), so
    // the SECOND admission is where a non-monotonic position surfaces. Named
    // `panic!`, for the same reason as above.
    let second = match store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "two")?) {
        Ok(admitted) => admitted.seq(),
        other => panic!("the second admission must take its OWN later position: {other:?}"),
    };
    assert!(
        first < second,
        "admission positions must ascend: {first:?} then {second:?}"
    );
    Ok(())
}

#[test]
fn an_exact_retry_reconciles_to_the_original_position_and_a_conflict_refuses() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    let first = store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
    let seq = match first {
        MailboxAdmitted::Admitted { seq } => seq,
        other => panic!("the first admission must write: {other:?}"),
    };
    store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "two")?)?;

    // The exact same bytes under the same id: reconciles to the ORIGINAL
    // position. Minting a second position would move a message in the
    // recipient's order and hand a reader the same work twice.
    match store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)? {
        MailboxAdmitted::Idempotent { seq: again } => assert_eq!(
            again, seq,
            "a retry must reconcile to the original admission position"
        ),
        other => panic!("an exact retry must not write again: {other:?}"),
    }

    // Different bytes under the same id: refused. Identity reuse is not a
    // retry.
    match store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "DIFFERENT")?) {
        Err(WriteError::MailboxConflict { message }) => assert_eq!(message, "msg-1"),
        other => panic!("conflicting reuse of a message id must refuse: {other:?}"),
    }
    assert_eq!(actionable(&store, "seat-gamma")?, vec!["msg-1", "msg-2"]);
    Ok(())
}

#[test]
fn a_source_acknowledgement_requires_the_admission_and_is_idempotent() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    let id = MailboxMessageId::new("msg-1").ok_or("id")?;

    // Local admission comes FIRST. Acknowledging first would tell a transport
    // to stop resending work this seat does not durably hold.
    match store.acknowledge_mailbox_source(&to, &id) {
        Err(WriteError::MailboxNotAdmitted { message }) => assert_eq!(message, "msg-1"),
        other => panic!("acknowledging an unadmitted message must refuse: {other:?}"),
    }

    store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
    assert_eq!(
        store.acknowledge_mailbox_source(&to, &id)?,
        MailboxAcknowledged::Acknowledged
    );
    assert_eq!(
        store.acknowledge_mailbox_source(&to, &id)?,
        MailboxAcknowledged::AlreadyAcknowledged,
        "a retried acknowledgement is reconciled, not an error"
    );

    // Acknowledged is NOT consumed: the message is still actionable work.
    assert_eq!(actionable(&store, "seat-gamma")?, vec!["msg-1"]);
    Ok(())
}

#[test]
fn acknowledging_another_seats_message_refuses() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    store.admit_mailbox_message(
        &recipient("seat-gamma")?,
        &message("msg-1", "seat-gamma", "one")?,
    )?;
    let id = MailboxMessageId::new("msg-1").ok_or("id")?;
    match store.acknowledge_mailbox_source(&recipient("seat-delta")?, &id) {
        Err(WriteError::MailboxWrongRecipient { found, .. }) => assert_eq!(found, "seat-gamma"),
        other => panic!("a cross-recipient acknowledgement must refuse: {other:?}"),
    }
    Ok(())
}

#[test]
fn crash_before_admission_leaves_the_message_absent_and_replayable() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    {
        let tx = store.governed_transaction()?;
        EventStore::admit_mailbox_message_within(
            &tx,
            &to,
            &message("msg-1", "seat-gamma", "one")?,
        )?;
        // DROPPED, not committed: the exact state a crash before the commit
        // leaves behind.
    }
    assert!(
        actionable(&store, "seat-gamma")?.is_empty(),
        "an uncommitted admission must not survive"
    );
    // The source still owes it, and replay is safe: the same bytes admit
    // cleanly and take the FIRST position, not a burned one.
    match store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)? {
        MailboxAdmitted::Admitted { seq } => assert_eq!(seq.value(), 1),
        other => panic!("replay after a pre-admission crash must admit: {other:?}"),
    }
    Ok(())
}

#[test]
fn crash_between_admission_and_acknowledgement_replays_to_the_same_position() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    let id = MailboxMessageId::new("msg-1").ok_or("id")?;
    let admitted = store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
    let seq = admitted.seq();

    // The acknowledgement is lost to the crash.
    {
        let tx = store.governed_transaction()?;
        EventStore::acknowledge_mailbox_source_within(&tx, &to, &id)?;
    }

    // Replay finds the exact same bytes: same position, no second admission.
    match store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)? {
        MailboxAdmitted::Idempotent { seq: again } => assert_eq!(again, seq),
        other => panic!("replay must reconcile, never duplicate: {other:?}"),
    }
    // And the acknowledgement can now proceed, exactly once.
    assert_eq!(
        store.acknowledge_mailbox_source(&to, &id)?,
        MailboxAcknowledged::Acknowledged
    );
    assert_eq!(actionable(&store, "seat-gamma")?, vec!["msg-1"]);
    Ok(())
}

#[test]
fn crash_after_acknowledgement_cannot_duplicate_the_admission() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    let id = MailboxMessageId::new("msg-1").ok_or("id")?;
    let seq = store
        .admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?
        .seq();
    store.acknowledge_mailbox_source(&to, &id)?;

    // A transport that crashed before recording our acknowledgement resends.
    match store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)? {
        MailboxAdmitted::Idempotent { seq: again } => assert_eq!(again, seq),
        other => panic!("a resend after acknowledgement must not duplicate: {other:?}"),
    }
    assert_eq!(
        actionable(&store, "seat-gamma")?,
        vec!["msg-1"],
        "one message, one row, one position"
    );
    Ok(())
}

#[test]
fn the_page_cursor_is_exclusive_and_ascending() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    for n in 1..=5 {
        store.admit_mailbox_message(&to, &message(&format!("msg-{n}"), "seat-gamma", "body")?)?;
    }
    let first = store.read_actionable_mailbox(&to, MailboxCursor::START, 2)?;
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| page_message(entry).id().as_str())
            .collect::<Vec<_>>(),
        vec!["msg-1", "msg-2"]
    );
    let cursor = first.next_after.ok_or("a full page owes a cursor")?;

    let second = store.read_actionable_mailbox(&to, cursor, 2)?;
    assert_eq!(
        second
            .entries
            .iter()
            .map(|entry| page_message(entry).id().as_str())
            .collect::<Vec<_>>(),
        vec!["msg-3", "msg-4"],
        "the cursor is EXCLUSIVE: msg-2 must not repeat"
    );

    let third = store.read_actionable_mailbox(&to, second.next_after.ok_or("cursor")?, 2)?;
    assert_eq!(
        third
            .entries
            .iter()
            .map(|entry| page_message(entry).id().as_str())
            .collect::<Vec<_>>(),
        vec!["msg-5"]
    );
    assert!(
        third.next_after.is_none(),
        "a short page ends honestly rather than promising more"
    );

    match store.read_actionable_mailbox(&to, MailboxCursor::START, 0) {
        Err(WriteError::MailboxPageLimitZero) => {}
        other => panic!("a zero page limit must refuse: {other:?}"),
    }
    Ok(())
}

#[test]
fn the_page_is_capped_at_sixty_four_messages() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    for n in 1..=(MAX_MAILBOX_PAGE_MESSAGES + 5) {
        store.admit_mailbox_message(&to, &message(&format!("msg-{n:04}"), "seat-gamma", "b")?)?;
    }
    // Asking for more than the cap does not raise it.
    let page = store.read_actionable_mailbox(&to, MailboxCursor::START, 10_000)?;
    assert_eq!(page.entries.len(), MAX_MAILBOX_PAGE_MESSAGES);
    assert_eq!(MAX_MAILBOX_PAGE_MESSAGES, 64);
    assert!(
        page.next_after.is_some(),
        "a capped page owes the caller its continuation"
    );
    Ok(())
}

#[test]
fn the_page_is_capped_at_two_hundred_fifty_six_kibibytes_of_decoded_bodies() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    // Eight 64-KiB bodies is 512 KiB, twice the aggregate bound.
    let big = "b".repeat(MAX_MAILBOX_BODY_BYTES);
    for n in 1..=8 {
        store.admit_mailbox_message(&to, &message(&format!("msg-{n}"), "seat-gamma", &big)?)?;
    }
    let page =
        store.read_actionable_mailbox(&to, MailboxCursor::START, MAX_MAILBOX_PAGE_MESSAGES)?;
    assert_eq!(MAX_MAILBOX_PAGE_BYTES, 262_144);
    assert!(
        page.body_bytes <= MAX_MAILBOX_PAGE_BYTES,
        "a page returned {} body bytes, above the {MAX_MAILBOX_PAGE_BYTES}-byte bound",
        page.body_bytes
    );
    assert_eq!(
        page.entries.len(),
        MAX_MAILBOX_PAGE_BYTES / MAX_MAILBOX_BODY_BYTES,
        "the byte bound, not the message bound, stopped this page"
    );
    assert!(page.next_after.is_some(), "a bounded page owes a cursor");
    Ok(())
}

#[test]
fn one_oversize_body_still_makes_progress() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    // A body larger than the whole page byte bound cannot exist — the body
    // bound is smaller — so the sharpest case is a single message that fills
    // most of the page budget: it must still be RETURNED rather than starving
    // the mailbox behind a bound it can never satisfy.
    let big = "b".repeat(MAX_MAILBOX_BODY_BYTES);
    store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", &big)?)?;
    let page = store.read_actionable_mailbox(&to, MailboxCursor::START, 1)?;
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.body_bytes, MAX_MAILBOX_BODY_BYTES);
    Ok(())
}

#[test]
fn a_superseded_message_is_no_longer_actionable() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "first")?)?;
    store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "second")?)?;
    store.admit_mailbox_message(
        &to,
        &message_superseding("msg-3", "seat-gamma", "corrects msg-1", Some("msg-1"))?,
    )?;
    assert_eq!(
        actionable(&store, "seat-gamma")?,
        vec!["msg-2", "msg-3"],
        "the superseded message is suppressed; its successor and the untouched \
         sibling remain"
    );
    Ok(())
}

#[test]
fn a_supersedes_edge_must_name_an_existing_same_recipient_message() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let gamma = recipient("seat-gamma")?;
    let delta = recipient("seat-delta")?;

    match store.admit_mailbox_message(
        &gamma,
        &message_superseding("msg-1", "seat-gamma", "b", Some("msg-absent"))?,
    ) {
        Err(WriteError::MailboxSupersedesMissing { message, target }) => {
            assert_eq!(message, "msg-1");
            assert_eq!(target, "msg-absent");
        }
        other => panic!("superseding a message that does not exist must refuse: {other:?}"),
    }

    // A target in ANOTHER seat's mailbox: one seat cannot silence another's.
    store.admit_mailbox_message(&delta, &message("msg-d", "seat-delta", "delta's")?)?;
    match store.admit_mailbox_message(
        &gamma,
        &message_superseding("msg-2", "seat-gamma", "b", Some("msg-d"))?,
    ) {
        Err(WriteError::MailboxSupersedesCrossRecipient { message, target }) => {
            assert_eq!(message, "msg-2");
            assert_eq!(target, "msg-d");
        }
        other => panic!("a cross-recipient supersedes edge must refuse: {other:?}"),
    }
    assert_eq!(
        actionable(&store, "seat-delta")?,
        vec!["msg-d"],
        "the other seat's mail is untouched by the refusal"
    );
    Ok(())
}

#[test]
fn supersession_only_ever_reaches_backwards() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    let target = store
        .admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "first")?)?
        .seq();
    let later = store
        .admit_mailbox_message(
            &to,
            &message_superseding("msg-2", "seat-gamma", "second", Some("msg-1"))?,
        )?
        .seq();
    // The structural invariant behind "only an EARLIER message may be
    // superseded": admission always takes a position one past this recipient's
    // highest, so any target that exists is already below it and a target that
    // does not exist is the missing-target refusal above. There is no third
    // case, which is why there is no third refusal arm.
    assert!(
        target < later,
        "an admitted target must sit strictly below its superseding message"
    );
    Ok(())
}

#[test]
fn a_consumed_message_is_excluded_but_reconciles_by_exact_id() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-gamma")?;
    let id = MailboxMessageId::new("msg-1").ok_or("id")?;
    store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
    store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "two")?)?;

    assert_eq!(
        store.consume_mailbox_message(&to, &id)?,
        MailboxConsumed::Consumed
    );
    assert_eq!(
        actionable(&store, "seat-gamma")?,
        vec!["msg-2"],
        "a consumed message is no longer actionable"
    );
    // A consumer that crashed after taking the message reconciles through the
    // exact id rather than double-counting.
    assert_eq!(
        store.consume_mailbox_message(&to, &id)?,
        MailboxConsumed::AlreadyConsumed
    );
    // Consumption is OURS; it is not the source acknowledgement.
    assert_eq!(
        store.acknowledge_mailbox_source(&to, &id)?,
        MailboxAcknowledged::Acknowledged,
        "consuming locally must not have told the source anything"
    );
    Ok(())
}

#[test]
fn a_consumer_cannot_write_the_mailbox_table() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    store.admit_mailbox_message(
        &recipient("seat-gamma")?,
        &message("msg-1", "seat-gamma", "one")?,
    )?;
    // The public raw-read seam still exists; what it can no longer do is write
    // governed state. `Deny` refuses at statement PREPARATION, so nothing runs
    // — asserting "the row is unchanged" would score a silent `Ignore` as a
    // pass, which is why the error CODE is asserted.
    for sql in [
        "UPDATE mailbox_message SET source_acknowledged = 1",
        "UPDATE mailbox_message SET consumed = 1",
        "DELETE FROM mailbox_message",
        "INSERT INTO mailbox_message
            (message_id, recipient, admission_seq, kind, supersedes,
             source_acknowledged, consumed, record_bytes)
         VALUES ('forged', 'seat-gamma', 99, 'done', NULL, 1, 0, x'00')",
    ] {
        match store.connection().execute(sql, []) {
            Err(rusqlite::Error::SqliteFailure(code, _))
                if code.code == ErrorCode::AuthorizationForStatementDenied => {}
            other => panic!("a consumer statement reached the mailbox: {sql} -> {other:?}"),
        }
    }
    // And the row is exactly as the store left it.
    assert_eq!(actionable(&store, "seat-gamma")?, vec!["msg-1"]);
    Ok(())
}

#[test]
fn a_hostile_row_is_named_in_the_page_and_the_page_keeps_answering() -> TestResult {
    // The authorizer is a per-CONNECTION guard, so a SECOND connection to the
    // same file is the honest way to stage corruption a consumer could not
    // write through the store. That is the hostile seam, not a bypass claim.
    let dir = std::env::temp_dir().join(format!("r3a-mailbox-hostile-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mailbox.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let mut store = EventStore::open(&path, "r3a-test")?;
        let to = recipient("seat-gamma")?;
        store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
        store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "two")?)?;
        store.admit_mailbox_message(&to, &message("msg-3", "seat-gamma", "three")?)?;
    }
    {
        // A row whose KIND column disagrees with its bytes. A page that trusted
        // the column would report the wrong kind; one that silently skipped the
        // row would drop a message; one that refused the page would hold every
        // later read at this row forever. The contract is a fourth: the row is
        // NAMED where it stands, and the page still answers.
        let hostile = rusqlite::Connection::open(&path)?;
        hostile.execute(
            "UPDATE mailbox_message SET kind = 'done' WHERE message_id = ?1",
            ["msg-2"],
        )?;
    }
    let store = EventStore::open(&path, "r3a-test")?;
    let page = store.read_actionable_mailbox(
        &recipient("seat-gamma")?,
        MailboxCursor::START,
        MAX_MAILBOX_PAGE_MESSAGES,
    )?;
    assert_eq!(
        page.entries.len(),
        3,
        "one bad row costs that one position, not the page: {page:?}"
    );
    let [MailboxPageEntry::Message(first), MailboxPageEntry::Unreadable(row), MailboxPageEntry::Message(third)] =
        &page.entries[..]
    else {
        panic!("the placeholder must stand at the corrupted row's own position: {page:?}");
    };
    assert_eq!(first.id().as_str(), "msg-1");
    assert_eq!(third.id().as_str(), "msg-3");
    assert_eq!(row.message, "msg-2");
    assert_eq!(row.kind, "done");
    assert_eq!(row.seq.value(), 2);
    assert!(
        row.reason.contains("kind"),
        "the placeholder must name the disagreeing column: {}",
        row.reason
    );
    assert_eq!(
        page.body_bytes,
        "one".len() + "three".len(),
        "the unreadable row contributes no decoded body bytes"
    );
    assert!(
        page.next_after.is_none(),
        "the page still reached the end of the mailbox honestly"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

#[test]
fn a_row_with_trailing_record_bytes_is_named_unreadable() -> TestResult {
    // Same hostile seam, a second mechanism: the columns AGREE and the RECORD
    // has bytes appended to it. A decoder that stopped at the last field it
    // recognised would accept this and hand a reader a message that is not the
    // one that was stored.
    let dir = std::env::temp_dir().join(format!("mailbox-hostile-trailing-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mailbox.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let mut store = EventStore::open(&path, "r3a-test")?;
        let to = recipient("seat-gamma")?;
        store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
        store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "two")?)?;
        store.admit_mailbox_message(&to, &message("msg-3", "seat-gamma", "three")?)?;
    }
    {
        let hostile = rusqlite::Connection::open(&path)?;
        hostile.execute(
            "UPDATE mailbox_message
             SET record_bytes = CAST(record_bytes || x'00' AS BLOB)
             WHERE message_id = ?1",
            ["msg-2"],
        )?;
    }
    let store = EventStore::open(&path, "r3a-test")?;
    let page = store.read_actionable_mailbox(
        &recipient("seat-gamma")?,
        MailboxCursor::START,
        MAX_MAILBOX_PAGE_MESSAGES,
    )?;
    let [_, MailboxPageEntry::Unreadable(row), _] = &page.entries[..] else {
        panic!("the appended-bytes row must stand named where it is: {page:?}");
    };
    assert_eq!(row.message, "msg-2");
    assert!(
        row.reason.contains("trailing"),
        "the placeholder must name the trailing bytes: {}",
        row.reason
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

#[test]
fn a_named_unreadable_row_is_consumable_and_the_mailbox_moves_on() -> TestResult {
    // The named absence is consumable: the runtime takes it and the mailbox
    // moves on, so the bad row can never hold the reader here again. The
    // durable bytes are untouched by that marker.
    let dir = std::env::temp_dir().join(format!(
        "mailbox-unreadable-consumable-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mailbox.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let mut store = EventStore::open(&path, "r3a-test")?;
        let to = recipient("seat-gamma")?;
        store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
        store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "two")?)?;
        store.admit_mailbox_message(&to, &message("msg-3", "seat-gamma", "three")?)?;
    }
    {
        let hostile = rusqlite::Connection::open(&path)?;
        hostile.execute(
            "UPDATE mailbox_message
             SET record_bytes = CAST(record_bytes || x'00' AS BLOB)
             WHERE message_id = ?1",
            ["msg-2"],
        )?;
    }
    let mut store = EventStore::open(&path, "r3a-test")?;
    let consumed = store.consume_unreadable_mailbox_message(&recipient("seat-gamma")?, "msg-2")?;
    assert_eq!(consumed, MailboxConsumed::Consumed);
    assert_eq!(
        actionable(&store, "seat-gamma")?,
        vec!["msg-1", "msg-3"],
        "the named absence leaves the mailbox readable and moving"
    );
    let still = store.consume_unreadable_mailbox_message(&recipient("seat-gamma")?, "msg-2")?;
    assert_eq!(
        still,
        MailboxConsumed::AlreadyConsumed,
        "the marker is idempotent through the exact row"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

#[test]
fn a_row_outside_the_ordering_law_never_blocks_the_page() -> TestResult {
    // The page query fetches rows strictly AFTER the reader's cursor, and the
    // cursor starts at zero: a row at admission_seq zero is outside the
    // mailbox's own ordering law, so it is not part of the ordered page at
    // all. It is not a placeholder (a placeholder needs a position) and it
    // cannot refuse the page (it is not fetched). It stays durable, and the
    // by-id door still refuses it.
    let dir = std::env::temp_dir().join(format!("mailbox-unorderable-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mailbox.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let mut store = EventStore::open(&path, "mailbox-unorderable-test")?;
        let to = recipient("seat-gamma")?;
        store.admit_mailbox_message(&to, &message("msg-1", "seat-gamma", "one")?)?;
        store.admit_mailbox_message(&to, &message("msg-2", "seat-gamma", "two")?)?;
    }
    {
        let hostile = rusqlite::Connection::open(&path)?;
        hostile.execute(
            "UPDATE mailbox_message SET admission_seq = 0 WHERE message_id = ?1",
            ["msg-2"],
        )?;
    }
    let store = EventStore::open(&path, "mailbox-unorderable-test")?;
    let page = store.read_actionable_mailbox(
        &recipient("seat-gamma")?,
        MailboxCursor::START,
        MAX_MAILBOX_PAGE_MESSAGES,
    )?;
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| match entry {
                MailboxPageEntry::Message(message) => message.id().as_str().to_owned(),
                MailboxPageEntry::Unreadable(row) => row.message.clone(),
            })
            .collect::<Vec<_>>(),
        vec!["msg-1"],
        "the out-of-law row is absent from the ordered page, not a refusal"
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

#[test]
fn existing_rows_over_an_identity_bound_refuse_with_field_bytes_and_max() -> TestResult {
    let recipient_over = "r".repeat(MAX_SEAT_RECIPIENT_ID_BYTES + 1);
    let message_over = "m".repeat(MAX_MAILBOX_MESSAGE_ID_BYTES + 1);
    let cases = [
        (
            "message-id",
            "message id",
            message_over.as_str(),
            "seat-alpha",
            "seat-gamma",
            None,
        ),
        (
            "sender",
            "sender",
            "msg-hostile-sender",
            recipient_over.as_str(),
            "seat-gamma",
            None,
        ),
        (
            "recipient",
            "recipient",
            "msg-hostile-recipient",
            "seat-alpha",
            recipient_over.as_str(),
            None,
        ),
        (
            "supersedes",
            "supersedes target",
            "msg-hostile-supersedes",
            "seat-alpha",
            "seat-gamma",
            Some(message_over.as_str()),
        ),
    ];

    for (case_name, field, record_id, sender, record_recipient, supersedes) in cases {
        let dir = std::env::temp_dir().join(format!(
            "mailbox-id-bound-{case_name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("mailbox.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            let store = EventStore::open(&path, "mailbox-id-bound-test")?;
            drop(store);
            let hostile = rusqlite::Connection::open(&path)?;
            let indexed_id = record_id;
            let indexed_supersedes = supersedes;
            hostile.execute(
                "INSERT INTO mailbox_message
                    (message_id, recipient, admission_seq, kind, supersedes,
                     source_acknowledged, consumed, record_bytes)
                 VALUES (?1, ?2, 1, ?3, ?4, 0, 0, ?5)",
                rusqlite::params![
                    indexed_id,
                    "seat-gamma",
                    "task-pointer",
                    indexed_supersedes,
                    hostile_record(record_id, sender, record_recipient, supersedes)?,
                ],
            )?;
        }

        let store = EventStore::open(&path, "mailbox-id-bound-test")?;
        let page = store.read_actionable_mailbox(
            &recipient("seat-gamma")?,
            MailboxCursor::START,
            MAX_MAILBOX_PAGE_MESSAGES,
        )?;
        // One row was staged; the page must answer with exactly that row's
        // named placeholder, carrying the decoder's own exact refusal text
        // rather than losing the row or the page.
        let [MailboxPageEntry::Unreadable(row)] = &page.entries[..] else {
            panic!("{case_name} over-bound row must stand named, not decode or vanish: {page:?}");
        };
        assert_eq!(row.message, record_id);
        assert_eq!(
            row.reason,
            format!(
                "{field} is 257 UTF-8 bytes, above the 256-byte bound; \
                 the stored row is refused, never truncated"
            ),
            "{case_name} must report its own field and exact byte bound"
        );
        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
    Ok(())
}

#[test]
fn a_wrong_shaped_mailbox_namesake_refuses_the_open() -> TestResult {
    // `CREATE TABLE IF NOT EXISTS` honours a wrong-shaped namesake silently.
    // The open-time column check is what turns that into a refusal.
    let dir = std::env::temp_dir().join(format!("r3a-mailbox-shape-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("alien.sqlite");
    let _ = std::fs::remove_file(&path);
    {
        let alien = rusqlite::Connection::open(&path)?;
        alien.execute_batch(
            "CREATE TABLE mailbox_message (
                message_id TEXT PRIMARY KEY,
                recipient TEXT NOT NULL,
                record_bytes BLOB NOT NULL
            );",
        )?;
    }
    // The refusal names the TABLE and the exact missing column. Asserting the
    // message rather than only the error class is what stops this passing on a
    // refusal that came from somewhere else — three mechanisms can refuse an
    // alien schema, and only one of them is the column check.
    match EventStore::open(&path, "r3a-test") {
        Err(error) => {
            // The schema check runs inside the preparation transaction, so the
            // refusal arrives wrapped; the MESSAGE is what identifies which
            // mechanism refused, and it must name this table and this column.
            let rendered = format!("{error:?}");
            assert!(
                rendered.contains("incompatible preexisting table mailbox_message")
                    && rendered.contains("missing column admission_seq"),
                "the open must refuse by naming the mailbox column it lacks: {rendered}"
            );
        }
        Ok(_) => panic!("a wrong-shaped mailbox namesake must refuse the open"),
    }
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}
