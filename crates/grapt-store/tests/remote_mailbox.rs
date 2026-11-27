//! NMAIL durable cuts through public store APIs. No real provider is simulated
//! into a pass: these witnesses own only local commit, recovery and refusal.
use grapt_store::{EventStore, RemoteMailboxInput, RemoteMailboxScope, WriteError};
use grapt_types::remote_mail::ack::{DeliveryAdmissionAck, DeliveryAdmissionAckParts};
use grapt_types::remote_mail::*;
use grapt_types::{MailboxBody, MailboxCursor, ProtocolVersion, SeatMessageKind, SeatRecipientId};
use std::path::{Path, PathBuf};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scope() -> Result<RemoteMailboxScope, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("source-project")?,
            TargetProjectId::try_new("secode")?,
        ),
        principal: SourcePrincipalId::try_new("coder")?,
        recipient: SeatRecipientId::try_new("coder")?,
    })
}
fn input(id: &str) -> Result<RemoteMailboxInput, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxInput {
        scope: scope()?,
        protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
        source_message: RemoteMailboxMessageId::try_new(id)?,
        source_binding: SourceBindingId::try_new("original-binding")?,
        source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{id}"))?,
        sender: SourcePrincipalId::try_new("pilot")?,
        kind: SeatMessageKind::Handoff,
        body: MailboxBody::new("bounded complete mail")?,
        source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
        refs: vec!["base-commit".into(), "plan-path".into()],
        supersedes: None,
        superseded_by: None,
        archive: SourceArchiveDisposition::Active,
        attention: MailAttention::Wake,
    })
}
fn ack_parts(ack: &DeliveryAdmissionAck) -> DeliveryAdmissionAckParts {
    DeliveryAdmissionAckParts {
        ack_id: ack.ack_id().clone(),
        source_binding: ack.source_binding().clone(),
        projects: ack.projects().clone(),
        source_message: ack.source_message().clone(),
        source_cursor: ack.source_cursor().clone(),
        recipient: ack.recipient().clone(),
        local_message: ack.local_message().clone(),
        local_admission: ack.local_admission(),
        canonical_message_digest: ack.canonical_message_digest().clone(),
        protocol: ack.protocol().clone(),
    }
}
struct OwnedDb(PathBuf);
impl OwnedDb {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "grapt-nmail-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }
    fn path(&self) -> PathBuf {
        self.0.join("mail.sqlite")
    }
    fn open(&self) -> Result<EventStore, WriteError> {
        EventStore::open(&self.path(), "nmail-durable-test")
    }
}
impl Drop for OwnedDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn count(store: &EventStore) -> Result<i64, rusqlite::Error> {
    store
        .connection()
        .query_row("SELECT count(*) FROM mailbox_message", [], |row| row.get(0))
}
fn checkpoint(store: &EventStore) -> Result<Option<ExclusiveSourceCursor>, WriteError> {
    store
        .remote_mailbox_checkpoint_page(
            &scope().map_err(|e| WriteError::RemoteMailbox {
                reason: e.to_string(),
            })?,
            None,
            64,
            262144,
        )
        .and_then(|page| {
            if page.next_before.is_some() {
                Err(WriteError::RemoteMailbox {
                    reason: "test must finish every prefix page before claiming a checkpoint"
                        .into(),
                })
            } else {
                Ok(page.candidate)
            }
        })
}
fn mutate(path: &Path, id: &str, update: impl FnOnce(&mut Vec<u8>)) -> TestResult {
    let hostile = rusqlite::Connection::open(path)?;
    let mut bytes: Vec<u8> = hostile.query_row(
        "SELECT record_bytes FROM mailbox_message WHERE message_id=?1",
        [id],
        |row| row.get(0),
    )?;
    update(&mut bytes);
    hostile.execute(
        "UPDATE mailbox_message SET record_bytes=?1 WHERE message_id=?2",
        rusqlite::params![bytes, id],
    )?;
    Ok(())
}

#[test]
fn admission_and_original_debt_roll_back_together_before_commit() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    {
        let tx = store.governed_transaction()?;
        EventStore::admit_remote_mailbox_within(&tx, &expected, &input("one")?)?;
        // Drop is the real SQLite transaction loss cut.
    }
    assert_eq!(count(&store)?, 0);
    assert!(checkpoint(&store)?.is_none());
    assert!(store
        .read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?
        .records
        .is_empty());
    Ok(())
}

#[test]
fn reopen_after_admission_replays_original_ack_coordinates_on_new_binding() -> TestResult {
    let db = OwnedDb::new("admission-cut")?;
    let expected = scope()?;
    let original = {
        let mut store = db.open()?;
        store.admit_remote_mailbox(&expected, &input("one")?)?.ack
    };
    let mut store = db.open()?;
    let mut replay = input("one")?;
    replay.source_binding = SourceBindingId::try_new("new-binding")?;
    replay.source_cursor = ExclusiveSourceCursor::try_new("new-cursor")?;
    let retry = store.admit_remote_mailbox(&expected, &replay)?;
    assert_eq!(retry.admitted.seq(), original.local_admission());
    assert!(
        retry.ack.is_exact_repeat_of(&original),
        "reconnect must retain every original ACK coordinate"
    );
    assert_eq!(count(&store)?, 1);
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert_eq!(page.records.len(), 1);
    assert!(page.records[0].delivery_pending());
    assert_eq!(page.records[0].refs(), ["base-commit", "plan-path"]);
    assert!(checkpoint(&store)?.is_none());
    Ok(())
}

#[test]
fn remote_success_before_local_settlement_retains_debt_and_exact_retry() -> TestResult {
    let db = OwnedDb::new("ack-cut")?;
    let expected = scope()?;
    let ack = {
        let mut store = db.open()?;
        store.admit_remote_mailbox(&expected, &input("one")?)?.ack
    };
    // The external effect has happened. Dropping its local settlement must not
    // infer that it did, advance checkpoint, or clear exact retry coordinates.
    {
        let mut store = db.open()?;
        let tx = store.governed_transaction()?;
        EventStore::settle_remote_mailbox_delivery_within(
            &tx,
            &expected,
            &ack,
            Some(SecondaryProjectionState::Posted),
        )?;
    }
    let mut store = db.open()?;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert!(
        page.records[0].delivery_pending(),
        "lost local settlement must leave primary delivery debt"
    );
    assert_eq!(page.records[0].ack(), &ack);
    assert!(checkpoint(&store)?.is_none());
    store.settle_remote_mailbox_delivery(
        &expected,
        &ack,
        Some(SecondaryProjectionState::AlreadyPresent),
    )?;
    drop(store);
    let store = db.open()?;
    assert_eq!(checkpoint(&store)?, Some(ack.source_cursor().clone()));
    assert_eq!(count(&store)?, 1);
    assert!(!store
        .read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?
        .records[0]
        .consumed());
    Ok(())
}

#[test]
fn a_later_ack_cannot_advance_checkpoint_past_an_earlier_debt() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    let first = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
    let second = store.admit_remote_mailbox(&expected, &input("two")?)?.ack;
    store.settle_remote_mailbox_delivery(
        &expected,
        &second,
        Some(SecondaryProjectionState::Posted),
    )?;
    assert!(
        checkpoint(&store)?.is_none(),
        "checkpoint must not skip the earlier pending admission"
    );
    store.settle_remote_mailbox_delivery(
        &expected,
        &first,
        Some(SecondaryProjectionState::Posted),
    )?;
    assert_eq!(checkpoint(&store)?, Some(second.source_cursor().clone()));
    Ok(())
}

#[test]
fn secondary_projection_debt_does_not_erase_primary_delivery_or_read_attention() -> TestResult {
    let db = OwnedDb::new("projection-debt")?;
    let expected = scope()?;
    let original = {
        let mut store = db.open()?;
        let ack = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
        store.settle_remote_mailbox_delivery(
            &expected,
            &ack,
            Some(SecondaryProjectionState::Unreadable),
        )?;
        ack
    };
    let mut store = db.open()?;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert!(!page.records[0].delivery_pending());
    assert!(page.records[0].repair_pending());
    assert!(!page.records[0].consumed());
    assert_eq!(page.records[0].ack(), &original);
    assert_eq!(checkpoint(&store)?, Some(original.source_cursor().clone()));
    store.settle_remote_mailbox_delivery(
        &expected,
        &original,
        Some(SecondaryProjectionState::AlreadyPresent),
    )?;
    assert!(!store
        .read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?
        .records[0]
        .repair_pending());
    Ok(())
}

#[test]
fn foreign_scope_and_changed_ack_coordinates_cannot_settle_or_consume() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    let ack = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
    let mut foreign = expected.clone();
    foreign.principal = SourcePrincipalId::try_new("delta")?;
    assert!(store
        .admit_remote_mailbox(&foreign, &input("two")?)
        .is_err());
    assert!(store
        .settle_remote_mailbox_delivery(&foreign, &ack, Some(SecondaryProjectionState::Posted))
        .is_err());
    assert!(store
        .consume_remote_mailbox(&foreign, ack.local_message())
        .is_err());
    let mut parts = ack_parts(&ack);
    parts.source_binding = SourceBindingId::try_new("new-binding")?;
    assert!(store
        .settle_remote_mailbox_delivery(
            &expected,
            &DeliveryAdmissionAck::new(parts),
            Some(SecondaryProjectionState::Posted)
        )
        .is_err());
    assert!(checkpoint(&store)?.is_none());
    assert_eq!(count(&store)?, 1);
    Ok(())
}

#[test]
fn conflicting_body_ref_order_timestamp_or_supersedes_refuses_without_a_new_admission() -> TestResult
{
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    store.admit_remote_mailbox(&expected, &input("one")?)?;
    let mut mutations = Vec::new();
    let mut body = input("one")?;
    body.body = MailboxBody::new("changed")?;
    mutations.push(body);
    let mut refs = input("one")?;
    refs.refs.reverse();
    mutations.push(refs);
    let mut timestamp = input("one")?;
    timestamp.source_created = SourceTimestamp::try_new("different-time")?;
    mutations.push(timestamp);
    let mut edge = input("one")?;
    edge.supersedes = Some(RemoteMailboxMessageId::try_new("older")?);
    mutations.push(edge);
    for changed in mutations {
        assert!(
            matches!(
                store.admit_remote_mailbox(&expected, &changed),
                Err(WriteError::MailboxConflict { .. })
            ),
            "changed immutable bytes must refuse"
        );
    }
    assert_eq!(count(&store)?, 1);
    Ok(())
}

#[test]
fn project_pairs_with_the_same_remote_id_remain_distinct() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let one = input("same-id")?;
    let mut two = one.clone();
    two.scope.projects = ProjectPair::new(
        SourceProjectId::try_new("other-source")?,
        TargetProjectId::try_new("secode")?,
    );
    let a = store.admit_remote_mailbox(&one.scope, &one)?.ack;
    let result = store.admit_remote_mailbox(&two.scope, &two);
    assert!(
        result.is_ok(),
        "same source identity in another project must remain distinct: {result:?}"
    );
    let b = result?.ack;
    assert_ne!(a.local_message(), b.local_message());
    assert_eq!(count(&store)?, 2);
    assert_eq!(
        store
            .read_remote_mailbox(&one.scope, MailboxCursor::START, 64, 262144)?
            .records
            .len(),
        1
    );
    Ok(())
}

#[test]
fn quiet_done_and_explicit_action_done_keep_provider_attention_separate_from_kind() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    let mut quiet = input("quiet")?;
    quiet.kind = SeatMessageKind::Done;
    quiet.attention = MailAttention::Quiet;
    let a = store.admit_remote_mailbox(&expected, &quiet)?.ack;
    let mut action = input("action")?;
    action.kind = SeatMessageKind::Done;
    let b = store.admit_remote_mailbox(&expected, &action)?.ack;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert!(!page.records[0].actionable());
    assert!(page.records[1].actionable());
    assert!(store
        .consume_remote_mailbox(&expected, a.local_message())
        .is_err());
    store.consume_remote_mailbox(&expected, b.local_message())?;
    assert!(store
        .read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?
        .records[1]
        .delivery_pending());
    assert!(
        store
            .acknowledge_mailbox_source(&expected.recipient, b.local_message())
            .is_err(),
        "legacy ACK cannot bypass remote proof"
    );
    assert!(
        store
            .consume_mailbox_message(&expected.recipient, a.local_message())
            .is_err(),
        "legacy consumption cannot bypass remote scope"
    );
    assert!(
        store
            .read_actionable_mailbox(&expected.recipient, MailboxCursor::START, 64)
            .is_err(),
        "legacy presentation cannot bypass remote scope"
    );
    Ok(())
}

#[test]
fn archived_superseded_and_consumed_replays_cannot_resurrect_work() -> TestResult {
    let db = OwnedDb::new("resurrection")?;
    let expected = scope()?;
    let one = input("one")?;
    let two = input("two")?;
    let three = input("three")?;
    {
        let mut store = db.open()?;
        let ack = store.admit_remote_mailbox(&expected, &one)?.ack;
        store.consume_remote_mailbox(&expected, ack.local_message())?;
        let mut archived = two.clone();
        archived.archive =
            SourceArchiveDisposition::Archived(SourceArchiveReason::RecipientDismissed);
        store.admit_remote_mailbox(&expected, &archived)?;
        let mut superseded = three.clone();
        superseded.superseded_by = Some(RemoteMailboxMessageId::try_new("later")?);
        store.admit_remote_mailbox(&expected, &superseded)?;
    }
    let mut store = db.open()?;
    store.admit_remote_mailbox(&expected, &one)?;
    store.admit_remote_mailbox(&expected, &two)?;
    store.admit_remote_mailbox(&expected, &three)?;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert!(page.records[0].consumed());
    assert!(!page.records[0].actionable());
    assert!(
        !page.records[1].actionable(),
        "an older active projection cannot resurrect a terminal disposition"
    );
    assert!(
        !page.records[2].actionable(),
        "an old projection cannot resurrect a superseded row"
    );
    assert_eq!(count(&store)?, 3);
    Ok(())
}

#[test]
fn malformed_metadata_and_column_disagreement_refuse_before_scope_or_attention_filtering(
) -> TestResult {
    for mode in 0..5 {
        let db = OwnedDb::new(&format!("corruption-{mode}"))?;
        let expected = scope()?;
        let ack = {
            let mut store = db.open()?;
            store.admit_remote_mailbox(&expected, &input("one")?)?.ack
        };
        match mode {
            0 => mutate(&db.path(), ack.local_message().as_str(), |bytes| {
                let size = bytes.len();
                bytes[size - 3] = 9;
            })?,
            1 => mutate(&db.path(), ack.local_message().as_str(), |bytes| {
                bytes.push(0)
            })?,
            2 => mutate(&db.path(), ack.local_message().as_str(), |bytes| {
                let positions: Vec<_> = bytes
                    .windows(b"bounded complete mail".len())
                    .enumerate()
                    .filter_map(|(at, value)| (value == b"bounded complete mail").then_some(at))
                    .collect();
                assert_eq!(positions.len(), 1, "canonical full body must occur once");
                bytes[positions[0]] = b'x';
            })?,
            3 => {
                rusqlite::Connection::open(db.path())?
                    .execute("UPDATE mailbox_message SET source_acknowledged=1", [])?;
            }
            _ => mutate(&db.path(), ack.local_message().as_str(), |bytes| {
                let size = bytes.len();
                bytes[size - 2] = 9;
            })?,
        }
        let store = db.open()?;
        let mut foreign = expected.clone();
        foreign.principal = SourcePrincipalId::try_new("delta")?;
        assert!(
            store
                .read_remote_mailbox(&foreign, MailboxCursor::START, 64, 262144)
                .is_err(),
            "corruption must refuse before filtering a different scope"
        );
    }
    Ok(())
}

#[test]
fn count_byte_and_other_scope_pages_make_bounded_explicit_progress() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    let mut foreign = input("foreign")?;
    foreign.scope.principal = SourcePrincipalId::try_new("delta")?;
    store.admit_remote_mailbox(&foreign.scope, &foreign)?;
    store.admit_remote_mailbox(&expected, &input("one")?)?;
    let first = store.read_remote_mailbox(&expected, MailboxCursor::START, 1, 262144)?;
    assert!(first.records.is_empty());
    let after = first.next_after.ok_or("pending scanned page")?;
    let second = store.read_remote_mailbox(&expected, after, 1, 262144)?;
    assert_eq!(second.records.len(), 1);
    assert!(second.next_after.is_none());
    assert!(store
        .read_remote_mailbox(&expected, MailboxCursor::START, 1, 1)
        .is_err());
    let bounded =
        store.read_remote_mailbox(&expected, MailboxCursor::START, 64, first.decoded_bytes)?;
    assert!(bounded.records.is_empty());
    assert!(bounded.next_after.is_some());
    assert!(store
        .read_remote_mailbox(&expected, MailboxCursor::START, 0, 1)
        .is_err());
    Ok(())
}

#[test]
fn concurrent_same_message_admissions_serialize_to_one_original_receipt() -> TestResult {
    let db = OwnedDb::new("race")?;
    let initial = db.open()?;
    let owner = initial.owner().clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(2);
    let mut starts = Vec::new();
    let mut jobs = Vec::new();
    for _ in 0..2 {
        let path = db.path();
        let owner = owner.clone();
        let ready = ready_tx.clone();
        let mail = input("one")?;
        let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
        starts.push(start_tx);
        jobs.push(std::thread::spawn(
            move || -> Result<DeliveryAdmissionAck, WriteError> {
                let mut store = EventStore::open_as(&path, "nmail-race", &owner)?;
                ready.send(()).map_err(|_| WriteError::RemoteMailbox {
                    reason: "race readiness receiver closed".into(),
                })?;
                start_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| WriteError::RemoteMailbox {
                        reason: "bounded race start timeout".into(),
                    })?;
                store
                    .admit_remote_mailbox(&mail.scope, &mail)
                    .map(|result| result.ack)
            },
        ));
    }
    for _ in 0..2 {
        ready_rx.recv_timeout(std::time::Duration::from_secs(5))?;
    }
    for start in starts {
        start.send(())?;
    }
    let mut acks = Vec::new();
    for job in jobs {
        acks.push(
            job.join()
                .map_err(|_| "remote admission thread panicked")??,
        );
    }
    assert_eq!(
        acks[0], acks[1],
        "both racers must retain the one original receipt"
    );
    assert_eq!(count(&initial)?, 1);
    Ok(())
}

#[test]
fn consumer_sql_cannot_change_remote_debt_or_consumption() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    let ack = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
    assert!(
        matches!(store.connection().execute("UPDATE mailbox_message SET source_acknowledged=1, consumed=1", []), Err(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::AuthorizationForStatementDenied)
    );
    assert!(store
        .read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?
        .records[0]
        .delivery_pending());
    store.settle_remote_mailbox_delivery(
        &expected,
        &ack,
        Some(SecondaryProjectionState::Posted),
    )?;
    Ok(())
}

#[test]
fn checkpoint_scan_reports_pending_pages_and_positive_byte_bounds() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    let first = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
    store.settle_remote_mailbox_delivery(
        &expected,
        &first,
        Some(SecondaryProjectionState::Posted),
    )?;
    let mut other = input("other")?;
    other.scope.principal = SourcePrincipalId::try_new("delta")?;
    let second = store.admit_remote_mailbox(&other.scope, &other)?.ack;
    store.settle_remote_mailbox_delivery(
        &other.scope,
        &second,
        Some(SecondaryProjectionState::Posted),
    )?;
    let page = store.remote_mailbox_checkpoint_page(&expected, None, 1, 262144)?;
    assert!(page.candidate.is_none());
    assert_eq!(page.next_before, Some(second.local_admission()));
    let next = store.remote_mailbox_checkpoint_page(&expected, page.next_before, 1, 262144)?;
    assert_eq!(next.candidate, Some(first.source_cursor().clone()));
    assert!(next.next_before.is_none());
    assert!(store
        .remote_mailbox_checkpoint_page(&expected, None, 1, 1)
        .is_err());
    assert!(store
        .remote_mailbox_checkpoint_page(&expected, None, 1, 0)
        .is_err());
    Ok(())
}

#[test]
fn over_bound_reference_input_and_hostile_record_refuse_without_partial_admission() -> TestResult {
    let db = OwnedDb::new("bounds")?;
    let mut store = db.open()?;
    let expected = scope()?;
    let mut large = input("large")?;
    large.refs = vec!["x".repeat(grapt_store::MAX_REMOTE_MAIL_RECORD_BYTES)];
    assert!(store.admit_remote_mailbox(&expected, &large).is_err());
    assert_eq!(count(&store)?, 0);
    let mut empty = input("empty")?;
    empty.refs = vec![String::new()];
    assert!(store.admit_remote_mailbox(&expected, &empty).is_err());
    assert_eq!(count(&store)?, 0);
    let ack = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
    drop(store);
    mutate(&db.path(), ack.local_message().as_str(), |bytes| {
        bytes.resize(grapt_store::MAX_REMOTE_MAIL_RECORD_BYTES + 1, 0)
    })?;
    let store = db.open()?;
    assert!(store
        .read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)
        .is_err());
    Ok(())
}

#[test]
fn checkpoint_does_not_skip_a_corrupt_earlier_settled_body() -> TestResult {
    let db = OwnedDb::new("checkpoint-corruption")?;
    let expected = scope()?;
    let first = {
        let mut store = db.open()?;
        let a = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
        let b = store.admit_remote_mailbox(&expected, &input("two")?)?.ack;
        store.settle_remote_mailbox_delivery(
            &expected,
            &a,
            Some(SecondaryProjectionState::Posted),
        )?;
        store.settle_remote_mailbox_delivery(
            &expected,
            &b,
            Some(SecondaryProjectionState::Posted),
        )?;
        a
    };
    mutate(&db.path(), first.local_message().as_str(), |bytes| {
        bytes.push(0)
    })?;
    let store = db.open()?;
    assert!(
        store
            .remote_mailbox_checkpoint_page(&expected, None, 64, 262144)
            .is_err(),
        "latest settled row cannot hide corruption in its earlier prefix"
    );
    Ok(())
}

#[test]
fn a_new_backward_supersession_excludes_its_target_without_refetching_the_old_dto() -> TestResult {
    for reversed in [false, true] {
        let mut store = EventStore::open_in_memory()?;
        let expected = scope()?;
        let old = input("older")?;
        let mut new = input("newer")?;
        new.supersedes = Some(old.source_message.clone());
        let old_ack = if reversed {
            store.admit_remote_mailbox(&expected, &new)?;
            store.admit_remote_mailbox(&expected, &old)?.ack
        } else {
            let ack = store.admit_remote_mailbox(&expected, &old)?.ack;
            store.admit_remote_mailbox(&expected, &new)?;
            ack
        };
        let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
        assert_eq!(page.records.len(), 2);
        for record in &page.records {
            assert_eq!(
                record.actionable(),
                record.ack().source_message() == &new.source_message,
                "backward supersession must suppress the older target without a forward DTO update"
            );
        }
        assert!(store
            .consume_remote_mailbox(&expected, old_ack.local_message())
            .is_err());
    }
    Ok(())
}

#[test]
fn one_remote_mail_read_does_not_scan_the_recipient_history() -> TestResult {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn read_work(count: u32) -> Result<u32, Box<dyn std::error::Error>> {
        let mut store = EventStore::open_in_memory()?;
        let expected = scope()?;
        for number in 0..count {
            store.admit_remote_mailbox(&expected, &input(&format!("history-{number}"))?)?;
        }
        let callbacks = Arc::new(AtomicU32::new(0));
        let observed = Arc::clone(&callbacks);
        store.progress_handler(
            100,
            Some(move || observed.fetch_add(1, Ordering::Relaxed) >= 1000),
        )?;
        let read = store.read_remote_mailbox(&expected, MailboxCursor::START, 1, 262144);
        store.progress_handler(0, None::<fn() -> bool>)?;
        let page = read?;
        assert_eq!(page.records.len(), 1);
        assert!(page.records[0].actionable());
        Ok(callbacks.load(Ordering::Relaxed))
    }

    let small = read_work(16)?;
    let large = read_work(512)?;
    // Allow fixed VM bookkeeping, while bounding growth from unrelated mail.
    assert!(
        large <= small.saturating_add(8),
        "one-message read grew from {small} to {large} callbacks at 100 SQLite VM operations"
    );
    Ok(())
}

#[test]
fn a_corrupt_successor_cannot_silently_hide_a_valid_mailbox_target() -> TestResult {
    let db = OwnedDb::new("successor-corruption")?;
    let expected = scope()?;
    let (old, new) = {
        let mut store = db.open()?;
        let a = store.admit_remote_mailbox(&expected, &input("old")?)?.ack;
        store.admit_remote_mailbox(&expected, &input("middle")?)?;
        let b = store.admit_remote_mailbox(&expected, &input("new")?)?.ack;
        (a, b)
    };
    // Forge only the index: the canonical new record has no backward edge.
    rusqlite::Connection::open(db.path())?.execute(
        "UPDATE mailbox_message SET supersedes=?1 WHERE message_id=?2",
        rusqlite::params![old.local_message().as_str(), new.local_message().as_str()],
    )?;
    let store = db.open()?;
    assert!(
        store
            .read_remote_mailbox(&expected, MailboxCursor::START, 1, 262144)
            .is_err(),
        "a successor index must be validated before it can suppress its target"
    );
    Ok(())
}

#[test]
fn ordinary_local_mail_cannot_supersede_scoped_remote_mail() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let expected = scope()?;
    let remote = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
    let local = grapt_types::SeatMailboxMessage::new(
        ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
        grapt_types::MailboxMessageId::try_new("local-new")?,
        SeatRecipientId::try_new("pilot")?,
        expected.recipient.clone(),
        SeatMessageKind::Handoff,
        MailboxBody::new("local-only mail")?,
        Some(remote.local_message().clone()),
        grapt_types::MailboxSourceCursor::new("local-cursor").ok_or("cursor")?,
    )?;
    assert!(
        store
            .admit_mailbox_message(&expected.recipient, &local)
            .is_err(),
        "legacy recipient-only authority cannot suppress a scoped remote target"
    );
    assert!(store
        .read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?
        .records[0]
        .actionable());
    assert_eq!(count(&store)?, 1);
    Ok(())
}

#[test]
fn exact_foreign_sender_is_preserved_without_assuming_it_is_a_local_seat_address() -> TestResult {
    let db = OwnedDb::new("sender-mapping")?;
    let expected = scope()?;
    let mut mail = input("one")?;
    mail.sender = SourcePrincipalId::try_new(expected.recipient.as_str())?;
    let mut store = db.open()?;
    let result = store.admit_remote_mailbox(&expected, &mail);
    assert!(
        result.is_ok(),
        "a source principal is not the target's local seat address: {result:?}"
    );
    let ack = result?.ack;
    drop(store);
    let store = db.open()?;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert_eq!(page.records[0].sender(), &mail.sender);
    assert_ne!(page.records[0].message().sender(), &expected.recipient);
    assert_eq!(page.records[0].ack(), &ack);
    Ok(())
}

#[test]
fn notification_consumption_can_commit_with_its_window_and_rolls_back_on_process_loss() -> TestResult
{
    let db = OwnedDb::new("consumption-cut")?;
    let expected = scope()?;
    let ack = {
        let mut store = db.open()?;
        let ack = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
        let tx = store.governed_transaction()?;
        assert_eq!(
            EventStore::consume_remote_mailbox_within(&tx, &expected, ack.local_message())?,
            grapt_store::MailboxConsumed::Consumed
        );
        drop(tx);
        ack
    };
    let mut store = db.open()?;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert!(!page.records[0].consumed());
    assert!(page.records[0].delivery_pending());
    store.consume_remote_mailbox(&expected, ack.local_message())?;
    drop(store);
    let store = db.open()?;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert!(page.records[0].consumed());
    assert!(page.records[0].delivery_pending());
    Ok(())
}

#[test]
fn witnessed_primary_with_unmeasured_fake_projection_is_not_pending_or_fabricated() -> TestResult {
    let db = OwnedDb::new("unmeasured-projection")?;
    let expected = scope()?;
    let ack = {
        let mut store = db.open()?;
        let ack = store.admit_remote_mailbox(&expected, &input("one")?)?.ack;
        store.settle_remote_mailbox_delivery(&expected, &ack, None)?;
        ack
    };
    let store = db.open()?;
    let page = store.read_remote_mailbox(&expected, MailboxCursor::START, 64, 262144)?;
    assert!(!page.records[0].delivery_pending());
    assert!(!page.records[0].repair_pending());
    assert_eq!(page.records[0].projection(), None);
    assert_eq!(checkpoint(&store)?, Some(ack.source_cursor().clone()));
    Ok(())
}

#[path = "remote_mailbox/origin.rs"]
mod origin;
#[path = "remote_mailbox/presentation.rs"]
mod presentation;

#[path = "remote_mailbox/progress.rs"]
mod progress;
#[path = "remote_mailbox/wake_scan.rs"]
mod wake_scan;
