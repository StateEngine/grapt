//! Mixed origin durability on one authenticated recipient's receiving floor.
use super::*;

fn from_origin(origin: &str, id: &str) -> Result<RemoteMailboxInput, Box<dyn std::error::Error>> {
    let mut row = input(id)?;
    row.scope = row.scope.for_origin(SourceProjectId::try_new(origin)?);
    row.source_cursor = ExclusiveSourceCursor::try_new(format!("cursor-{origin}-{id}"))?;
    Ok(row)
}

#[test]
fn mixed_origins_keep_equal_ids_distinct_and_binding_reads_bounded() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let binding = scope()?;
    let one = from_origin("origin-one", "same-id")?;
    let two = from_origin("origin-two", "same-id")?;
    let a = store.admit_remote_mailbox(&one.scope, &one)?.ack;
    let b = store.admit_remote_mailbox(&two.scope, &two)?.ack;
    assert_ne!(a.local_message(), b.local_message());
    assert_eq!(a.projects(), &one.scope.projects);
    assert_eq!(b.projects(), &two.scope.projects);
    let first = store.read_remote_mailbox_for_binding(&binding, MailboxCursor::START, 1, 262144)?;
    assert_eq!(first.records.len(), 1);
    assert_eq!(first.records[0].ack(), &a);
    let second = store.read_remote_mailbox_for_binding(
        &binding,
        first.next_after.ok_or("bounded next page")?,
        1,
        262144,
    )?;
    assert_eq!(second.records.len(), 1);
    assert_eq!(second.records[0].ack(), &b);
    assert!(second.next_after.is_none());
    assert_eq!(
        store
            .read_remote_mailbox(&one.scope, MailboxCursor::START, 64, 262144)?
            .records
            .len(),
        1
    );
    assert_eq!(
        store
            .read_remote_mailbox(&binding, MailboxCursor::START, 64, 262144)?
            .records
            .len(),
        0
    );
    let tag: Vec<u8> = store.connection().query_row(
        "SELECT substr(record_bytes,1,6) FROM mailbox_message WHERE message_id=?1",
        [a.local_message().as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(tag.as_slice(), b"SEMRM2");
    Ok(())
}

#[test]
fn cross_origin_admission_before_commit_rolls_back_and_replays_once() -> TestResult {
    let db = OwnedDb::new("origin-admission-cut")?;
    let mut store = db.open()?;
    let row = from_origin("outside-origin", "one")?;
    {
        let tx = store.governed_transaction()?;
        EventStore::admit_remote_mailbox_within(&tx, &row.scope, &row)?;
    }
    drop(store);
    let mut store = db.open()?;
    assert_eq!(count(&store)?, 0);
    let original = store.admit_remote_mailbox(&row.scope, &row)?.ack;
    assert_eq!(store.admit_remote_mailbox(&row.scope, &row)?.ack, original);
    assert_eq!(count(&store)?, 1);
    assert_eq!(original.projects(), &row.scope.projects);
    assert!(store
        .remote_mailbox_message(&row.scope, original.local_message())?
        .delivery_pending());
    Ok(())
}

#[test]
fn mixed_origin_debt_reopen_checkpoint_and_consumption_keep_original_provenance() -> TestResult {
    let db = OwnedDb::new("mixed-origin-reopen")?;
    let mut store = db.open()?;
    let binding = scope()?;
    let one = from_origin("origin-one", "same-id")?;
    let two = from_origin("origin-two", "same-id")?;
    let a = store.admit_remote_mailbox(&one.scope, &one)?.ack;
    let b = store.admit_remote_mailbox(&two.scope, &two)?.ack;
    store.settle_remote_mailbox_delivery(&two.scope, &b, Some(SecondaryProjectionState::Posted))?;
    assert!(store
        .remote_mailbox_checkpoint_page_for_binding(&binding, None, 64, 262144)?
        .candidate
        .is_none());
    drop(store);
    let mut store = db.open()?;
    let recovered =
        store.read_remote_mailbox_for_binding(&binding, MailboxCursor::START, 64, 262144)?;
    assert_eq!(recovered.records.len(), 2);
    assert_eq!(recovered.records[0].ack(), &a);
    assert_eq!(recovered.records[1].ack(), &b);
    assert!(recovered.records[0].delivery_pending());
    assert!(!recovered.records[1].delivery_pending());
    store.settle_remote_mailbox_delivery(
        &one.scope,
        &a,
        Some(SecondaryProjectionState::Unreadable),
    )?;
    assert!(store
        .remote_mailbox_message(&one.scope, a.local_message())?
        .repair_pending());
    store.settle_remote_mailbox_delivery(&one.scope, &a, Some(SecondaryProjectionState::Posted))?;
    assert_eq!(
        store
            .remote_mailbox_checkpoint_page_for_binding(&binding, None, 64, 262144)?
            .candidate,
        Some(b.source_cursor().clone())
    );
    store.consume_remote_mailbox(&one.scope, a.local_message())?;
    let mut replay = one.clone();
    replay.source_binding = SourceBindingId::try_new("replacement")?;
    replay.source_cursor = ExclusiveSourceCursor::try_new("replacement-cursor")?;
    assert_eq!(store.admit_remote_mailbox(&one.scope, &replay)?.ack, a);
    drop(store);
    let store = db.open()?;
    let rows = store
        .read_remote_mailbox_for_binding(&binding, MailboxCursor::START, 64, 262144)?
        .records;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].ack(), &a);
    assert!(rows[0].consumed());
    assert!(!rows[0].actionable());
    assert!(rows[1].actionable());
    Ok(())
}

#[test]
fn binding_reads_cannot_cross_target_principal_or_recipient() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let row = from_origin("legitimate-origin", "one")?;
    store.admit_remote_mailbox(&row.scope, &row)?;
    let expected = scope()?;
    let mut target = expected.clone();
    target.projects = ProjectPair::new(
        target.projects.source().clone(),
        TargetProjectId::try_new("foreign-target")?,
    );
    let mut principal = expected.clone();
    principal.principal = SourcePrincipalId::try_new("delta")?;
    let mut recipient = expected.clone();
    recipient.recipient = SeatRecipientId::try_new("delta")?;
    for foreign in [target, principal, recipient] {
        assert!(store
            .read_remote_mailbox_for_binding(&foreign, MailboxCursor::START, 64, 262144)?
            .records
            .is_empty());
        assert!(store
            .remote_mailbox_checkpoint_page_for_binding(&foreign, None, 64, 262144)?
            .candidate
            .is_none());
    }
    assert_eq!(count(&store)?, 1);
    Ok(())
}
