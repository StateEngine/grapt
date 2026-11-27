#[path = "../../../../test-support/commit_fault.rs"]
mod commit_fault;
use super::*;
use grapt_event::AggregateId;
use grapt_store::mailbox::remote::progress::{stream_id, RecordMailPage};
use grapt_types::remote_mail::progress::*;
fn gap() -> Result<RefusedMailRow, Box<dyn std::error::Error>> {
    Ok(RefusedMailRow::new(
        None,
        0,
        12,
        RefusedRowReason::MalformedEnvelope,
        ExclusiveSourceCursor::try_new("ref1.gap")?,
    )
    .ok_or("gap")?)
}
fn record<'a>(
    scope: &'a RemoteMailboxScope,
    messages: &'a [RemoteMailboxInput],
    gaps: &'a [RefusedMailRow],
    cursor: &'a ExclusiveSourceCursor,
) -> RecordMailPage<'a> {
    RecordMailPage {
        scope,
        protocol: MailTransportVersion::V2,
        previous: None,
        checkpoint: cursor,
        messages,
        gaps,
        decode_failures: &[],
        created_ms: 17,
    }
}
fn events(store: &EventStore) -> Result<i64, rusqlite::Error> {
    store
        .connection()
        .query_row("SELECT count(*) FROM event", [], |r| r.get(0))
}
#[test]
fn nmv2_atomic_page_rollback_and_reopen_retains_original_debt_and_gap() -> TestResult {
    let db = OwnedDb::new("v2-atomic")?;
    let mut store = db.open()?;
    let scope = scope()?;
    let messages = [input("healthy")?];
    let gaps = [gap()?];
    let cursor = gaps[0].cursor().clone();
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let writer = store.fencing_token(&AggregateId::Mail(stream_id(
        &scope,
        MailTransportVersion::V2,
    )?))?;
    {
        let tx = store.governed_transaction()?;
        EventStore::record_remote_mail_page_within(
            &tx,
            record(&scope, &messages, &gaps, &cursor),
            writer,
        )?;
    }
    assert_eq!(count(&store)?, 0);
    assert_eq!(events(&store)?, 0);
    let acks = store.record_remote_mail_page(record(&scope, &messages, &gaps, &cursor))?;
    assert_eq!(count(&store)?, 1);
    assert_eq!(events(&store)?, 1);
    drop(store);
    let store = db.open()?;
    let page = store
        .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 64, 262144)?
        .ok_or("page")?;
    assert_eq!(page.recorded.fact.gaps(), gaps);
    assert_eq!(page.recorded.fact.checkpoint, cursor);
    let recovered = store.remote_mail_progress_acks(&scope, &page.recorded.fact)?;
    assert_eq!(recovered, acks);
    assert!(store
        .remote_mailbox_message(&scope, acks[0].local_message())?
        .delivery_pending());
    Ok(())
}
#[test]
fn nmv2_duplicate_page_is_exact_and_refusal_never_admission() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let scope = scope()?;
    let gaps = [gap()?];
    let cursor = gaps[0].cursor().clone();
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    assert!(store
        .record_remote_mail_page(record(&scope, &[], &gaps, &cursor))?
        .is_empty());
    assert!(store
        .record_remote_mail_page(record(&scope, &[], &gaps, &cursor))?
        .is_empty());
    assert_eq!(count(&store)?, 0);
    assert_eq!(events(&store)?, 1);
    let changed =
        [
            RefusedMailRow::new(None, 0, 12, RefusedRowReason::MalformedJson, cursor.clone())
                .ok_or("gap")?,
        ];
    assert!(store
        .record_remote_mail_page(record(&scope, &[], &changed, &cursor))
        .is_err());
    assert_eq!(events(&store)?, 1);
    assert!(store.require_v1_mail_context(&scope).is_err());
    Ok(())
}
#[test]
fn nmv2_commit_veto_rolls_back_all_healthy_admissions() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let scope = scope()?;
    let messages = [input("healthy")?];
    let gaps = [gap()?];
    let cursor = gaps[0].cursor().clone();
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let _fault = commit_fault::CommitFault::install(
        &mut store,
        rusqlite::hooks::Action::SQLITE_INSERT,
        "event",
        0,
    )?;
    assert!(store
        .record_remote_mail_page(record(&scope, &messages, &gaps, &cursor))
        .is_err());
    assert_eq!(count(&store)?, 0);
    assert_eq!(events(&store)?, 0);
    Ok(())
}
#[test]
fn nmv2_foreign_scope_stale_writer_and_legacy_context_keep_no_effects() -> TestResult {
    let db = OwnedDb::new("v2-stale")?;
    let mut first = db.open()?;
    let scope = scope()?;
    let gaps = [gap()?];
    let cursor = gaps[0].cursor().clone();
    let messages = [input("healthy")?];
    first.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let mut second = db.open()?;
    second.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let attempted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = attempted.clone();
    first.update_hook(Some(
        move |_action, _database: &str, table: &str, _rowid| {
            if table == "mailbox_message" {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        },
    ))?;
    assert!(first
        .record_remote_mail_page(record(&scope, &messages, &gaps, &cursor))
        .is_err());
    assert_eq!(
        attempted.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "stale writer attempted a mailbox write before its fence"
    );
    assert_eq!(count(&first)?, 0);
    assert_eq!(events(&first)?, 0);
    let mut foreign = messages.clone();
    foreign[0].scope.principal = SourcePrincipalId::try_new("other")?;
    assert!(second
        .record_remote_mail_page(record(&scope, &foreign, &gaps, &cursor))
        .is_err());
    assert_eq!(count(&second)?, 0);
    let mut legacy = EventStore::open_in_memory()?;
    let ack = legacy.admit_remote_mailbox(&scope, &messages[0])?.ack;
    assert!(legacy
        .prepare_remote_mail_progress(&scope, MailTransportVersion::V2)
        .is_err());
    assert!(legacy
        .remote_mailbox_message(&scope, ack.local_message())?
        .delivery_pending());
    assert_eq!(count(&legacy)?, 1);
    Ok(())
}
#[test]
fn nmv2_corrupt_progress_or_missing_healthy_reference_refuses_recovery() -> TestResult {
    for corruption in [
        "UPDATE event SET data='{}' WHERE aggregate_family='mail'",
        "DELETE FROM mailbox_message",
    ] {
        let db = OwnedDb::new("v2-corrupt")?;
        let mut store = db.open()?;
        let scope = scope()?;
        let messages = [input("healthy")?];
        let gaps = [gap()?];
        let cursor = gaps[0].cursor().clone();
        store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
        store.record_remote_mail_page(record(&scope, &messages, &gaps, &cursor))?;
        drop(store);
        let hostile = rusqlite::Connection::open(db.path())?;
        hostile.execute(corruption, [])?;
        drop(hostile);
        let store = db.open()?;
        assert!(store
            .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 64, 262144)
            .is_err());
    }
    Ok(())
}

#[test]
fn nmv2_progress_must_extend_exact_durable_prefix() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let scope = scope()?;
    let gaps = [gap()?];
    let cursor = gaps[0].cursor().clone();
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    store.record_remote_mail_page(record(&scope, &[], &gaps, &cursor))?;
    let next = ExclusiveSourceCursor::try_new("ref1.next")?;
    let next_gap = [RefusedMailRow::new(
        None,
        12,
        24,
        RefusedRowReason::MalformedEnvelope,
        next.clone(),
    )
    .ok_or("gap")?];
    assert!(store
        .record_remote_mail_page(record(&scope, &[], &next_gap, &next))
        .is_err());
    assert_eq!(events(&store)?, 1);
    let mut input = record(&scope, &[], &next_gap, &next);
    input.previous = Some(&cursor);
    store.record_remote_mail_page(input)?;
    assert_eq!(events(&store)?, 2);
    Ok(())
}

#[test]
fn nmv2_recovery_rejects_forged_reference_missing_epoch_and_canonical_drift() -> TestResult {
    for mode in ["reference", "epoch", "identity", "canonical"] {
        let db = OwnedDb::new("v2-ref-proof")?;
        let mut store = db.open()?;
        let scope = scope()?;
        let messages = [input("healthy")?];
        let gaps = [gap()?];
        let cursor = gaps[0].cursor().clone();
        store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
        store.record_remote_mail_page(record(&scope, &messages, &gaps, &cursor))?;
        drop(store);
        let hostile = rusqlite::Connection::open(db.path())?;
        match mode {
            "reference" => {
                let raw: String = hostile.query_row("SELECT data FROM event", [], |r| r.get(0))?;
                let mut value: serde_json::Value = serde_json::from_str(&raw)?;
                value["admissions"][0]["digest"] = serde_json::json!("different-digest");
                let text = grapt_event::canonical_string(&value);
                let id = format!("evt_{}", grapt_event::canonical_hash_hex(&value));
                hostile.execute(
                    "UPDATE event SET data=?1,id=?2",
                    rusqlite::params![text, id],
                )?;
            }
            "epoch" => {
                hostile.execute("UPDATE event SET writer_epoch=NULL", [])?;
            }
            "identity" => {
                hostile.execute("UPDATE event SET id='evt_wrong'", [])?;
            }
            _ => {
                hostile.execute("UPDATE event SET data=' '||data", [])?;
            }
        }
        drop(hostile);
        let store = db.open()?;
        assert!(
            store
                .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 64, 262144)
                .is_err(),
            "accepted {mode}"
        );
    }
    Ok(())
}
#[test]
fn nmv2_recovery_charges_referenced_record_bytes_and_entry_count() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let scope = scope()?;
    let messages = [input("healthy")?];
    let gaps = [gap()?];
    let cursor = gaps[0].cursor().clone();
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    store.record_remote_mail_page(record(&scope, &messages, &gaps, &cursor))?;
    let page = store
        .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 64, 262144)?
        .ok_or("page")?;
    let error = store
        .remote_mail_progress_page(
            &scope,
            MailTransportVersion::V2,
            None,
            64,
            page.decoded_bytes - 1,
        )
        .err()
        .ok_or("reference byte overflow accepted")?;
    assert!(matches!(error, WriteError::RemoteMailProgressBudget));
    assert!(store
        .remote_mail_progress_page(
            &scope,
            MailTransportVersion::V2,
            Some(grapt_types::EventSeq::new(u64::MAX)),
            64,
            262144,
        )
        .is_err());
    assert!(store
        .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 1, 262144)
        .is_err());
    Ok(())
}

#[test]
fn local_decode_failure_is_atomic_restartable_and_never_an_admission_or_ack() -> TestResult {
    let db = OwnedDb::new("unknown-kind-progress")?;
    let mut store = db.open()?;
    let scope = scope()?;
    let messages = [input("healthy")?];
    let cursor = ExclusiveSourceCursor::try_new("opaque-unknown-kind")?;
    let failures = [MailDecodeFailure::unknown_kind(cursor.clone()).ok_or("failure")?];
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let writer = store.fencing_token(&AggregateId::Mail(stream_id(
        &scope,
        MailTransportVersion::V2,
    )?))?;
    {
        let tx = store.governed_transaction()?;
        let mut page = record(&scope, &messages, &[], &cursor);
        page.decode_failures = &failures;
        EventStore::record_remote_mail_page_within(&tx, page, writer)?;
    }
    assert_eq!((count(&store)?, events(&store)?), (0, 0));
    let mut page = record(&scope, &messages, &[], &cursor);
    page.decode_failures = &failures;
    let acks = store.record_remote_mail_page(page)?;
    assert_eq!(acks.len(), 1);
    drop(store);
    let mut store = db.open()?;
    let recovered = store
        .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 64, 262144)?
        .ok_or("page")?;
    assert_eq!(recovered.recorded.fact.decode_failures(), failures);
    assert!(recovered.recorded.fact.gaps().is_empty());
    assert_eq!(
        store.remote_mail_progress_acks(&scope, &recovered.recorded.fact)?,
        acks
    );
    assert_eq!((count(&store)?, events(&store)?), (1, 1));
    assert!(store
        .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 1, 262144)
        .is_err());
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let next = ExclusiveSourceCursor::try_new("next-unknown-kind")?;
    let failures = [MailDecodeFailure::unknown_kind(next.clone()).ok_or("failure")?];
    let mut page = record(&scope, &[], &[], &next);
    page.previous = Some(&cursor);
    page.decode_failures = &failures;
    assert!(store.record_remote_mail_page(page)?.is_empty());
    assert_eq!((count(&store)?, events(&store)?), (1, 2));
    Ok(())
}

#[test]
fn local_decode_failure_does_not_accept_unobserved_checkpoint_or_partial_transaction() -> TestResult
{
    let mut store = EventStore::open_in_memory()?;
    let scope = scope()?;
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let messages = [input("healthy")?];
    let cursor = ExclusiveSourceCursor::try_new("unknown-kind")?;
    let failures = [MailDecodeFailure::unknown_kind(cursor.clone()).ok_or("failure")?];
    let unobserved = ExclusiveSourceCursor::try_new("unobserved")?;
    let mut page = record(&scope, &messages, &[], &unobserved);
    page.decode_failures = &failures;
    assert!(store.record_remote_mail_page(page).is_err());
    assert_eq!((count(&store)?, events(&store)?), (0, 0));
    let _fault = commit_fault::CommitFault::install(
        &mut store,
        rusqlite::hooks::Action::SQLITE_INSERT,
        "event",
        0,
    )?;
    let mut page = record(&scope, &messages, &[], &cursor);
    page.decode_failures = &failures;
    assert!(store.record_remote_mail_page(page).is_err());
    assert_eq!((count(&store)?, events(&store)?), (0, 0));
    Ok(())
}
