use super::*;
use grapt_event::{
    mail_presented::{MailPresented, PresentedMail},
    AggregateId,
};
use grapt_store::{
    AppendInput, AttemptRecordCeiling, MailBindingExpectation, ReserveInput, TerminalCoordinates,
};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, EventId,
    EventVersion, InstructionKey, PhysicalAttemptId, RequestSentMarker, ReservedMarker, SessionId,
    StepNumber, Timestamp,
};
use std::num::NonZeroU32;
#[path = "presentation/origin.rs"]
mod origin;

fn session() -> Result<SessionId, Box<dyn std::error::Error>> {
    Ok("ses_native_mail".parse()?)
}
fn ceiling() -> Result<AttemptRecordCeiling, Box<dyn std::error::Error>> {
    Ok(AttemptRecordCeiling::new(
        NonZeroU32::new(4096).ok_or("ceiling")?,
    ))
}
fn binding() -> Result<MailBindingExpectation, Box<dyn std::error::Error>> {
    Ok(MailBindingExpectation::new(
        InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound-coder".into(),
    )?)
}
fn prepare(store: &mut EventStore) -> TestResult {
    store.elect_writer(&AggregateId::Session(session()?))?;
    store.connection().execute_batch("CREATE TABLE instruction_entry(session_id TEXT,key TEXT,value TEXT,removed INTEGER); INSERT INTO instruction_entry VALUES('ses_native_mail','stateengine-identity','bound-coder',0);")?;
    Ok(())
}
fn admitted(
    store: &mut EventStore,
    name: &str,
) -> Result<PresentedMail, Box<dyn std::error::Error>> {
    let ack = store.admit_remote_mailbox(&scope()?, &input(name)?)?.ack;
    store.settle_remote_mailbox_delivery(
        &scope()?,
        &ack,
        Some(SecondaryProjectionState::Posted),
    )?;
    Ok(PresentedMail {
        local_message: ack.local_message().clone(),
        source_message: ack.source_message().clone(),
        local_admission: ack.local_admission(),
        digest: ack.canonical_message_digest().clone(),
    })
}
fn make_fact(
    name: &str,
    step: u32,
    references: Vec<PresentedMail>,
) -> Result<MailPresented, Box<dyn std::error::Error>> {
    let expected = scope()?;
    Ok(MailPresented::new(
        PhysicalAttemptId::new(name).ok_or("attempt")?,
        StepNumber::new(step).ok_or("step")?,
        expected.projects,
        expected.principal,
        expected.recipient,
        references,
    )?)
}
fn capture(store: &mut EventStore, fact: &MailPresented, event: &str, commit: bool) -> TestResult {
    let aggregate = AggregateId::Session(session()?);
    let writer = store.fencing_token(&aggregate)?;
    let ceiling = ceiling()?;
    let maximum = NonZeroU32::new(ceiling.get()).ok_or("ceiling")?;
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(fact.attempt.clone(), maximum).map_err(|e| format!("{e:?}"))?,
        BoundedSessionId::new(session()?, maximum).map_err(|e| format!("{e:?}"))?,
        fact.step,
        AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(1)),
    );
    let terminal = TerminalCoordinates::new("evt_native_terminal".parse()?, 3);
    let event: EventId = event.parse()?;
    let data = fact.encode();
    let tx = store.governed_transaction()?;
    EventStore::reserve_attempt_receipt_within(
        &tx,
        &ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling,
        },
    )?;
    EventStore::permit_attempt_send_within(
        &tx,
        &fact.attempt,
        writer,
        ceiling,
        RequestSentMarker::new(Timestamp::from_millis(2)),
    )?;
    EventStore::append_remote_mail_presentation_within(
        &tx,
        &scope()?,
        &binding()?,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.mail.presented",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 2,
            data: &data,
        },
        ceiling,
    )?;
    if commit {
        tx.commit()?.discard();
    }
    Ok(())
}
fn event_count(store: &EventStore) -> Result<i64, rusqlite::Error> {
    store.connection().query_row(
        "SELECT count(*) FROM event WHERE type='session.mail.presented.1'",
        [],
        |r| r.get(0),
    )
}
#[test]
fn capture_before_commit_rolls_back_mail_event_reservation_and_permit() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    prepare(&mut store)?;
    let reference = admitted(&mut store, "rollback")?;
    let fact = make_fact("attempt-rollback", 1, vec![reference.clone()])?;
    capture(&mut store, &fact, "evt_capture_rollback", false)?;
    assert!(!store
        .remote_mailbox_message(&scope()?, &reference.local_message)?
        .consumed());
    assert_eq!(event_count(&store)?, 0);
    assert!(store.attempt_receipt(&fact.attempt, ceiling()?)?.is_none());
    assert!(store.remote_mail_presentation(&scope()?, &fact).is_err());
    capture(&mut store, &fact, "evt_capture_rollback", true)?;
    assert_eq!(event_count(&store)?, 1);
    assert!(store
        .remote_mailbox_message(&scope()?, &reference.local_message)?
        .consumed());
    assert!(store
        .attempt_receipt(&fact.attempt, ceiling()?)?
        .ok_or("receipt")?
        .receipt()
        .request_sent_at()
        .is_some());
    Ok(())
}
#[test]
fn committed_capture_survives_restart_and_source_archive_without_resurrection() -> TestResult {
    let db = OwnedDb::new("capture-restart")?;
    let mut store = db.open()?;
    prepare(&mut store)?;
    let reference = admitted(&mut store, "restart-capture")?;
    let fact = make_fact("attempt-capture", 1, vec![reference.clone()])?;
    capture(&mut store, &fact, "evt_capture_restart", true)?;
    let before = store.remote_mail_presentation(&scope()?, &fact)?;
    drop(store);
    let mut store = db.open()?;
    let mut changed = input("restart-capture")?;
    changed.archive = SourceArchiveDisposition::Archived(SourceArchiveReason::RecipientDismissed);
    changed.attention = MailAttention::Quiet;
    store.admit_remote_mailbox(&scope()?, &changed)?;
    let replay = store.remote_mail_presentation(&scope()?, &fact)?;
    assert_eq!(replay[0].message(), before[0].message());
    assert_eq!(replay[0].ack(), before[0].ack());
    assert!(!replay[0].actionable());
    store.elect_writer(&AggregateId::Session(session()?))?;
    assert!(capture(
        &mut store,
        &make_fact("attempt-new", 2, vec![reference])?,
        "evt_capture_repeat",
        true
    )
    .is_err());
    assert_eq!(event_count(&store)?, 1);
    Ok(())
}
#[test]
fn failed_append_after_consumption_rolls_back_the_whole_handoff() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    prepare(&mut store)?;
    let one = admitted(&mut store, "one-capture")?;
    capture(
        &mut store,
        &make_fact("attempt-one", 1, vec![one])?,
        "evt_collision",
        true,
    )?;
    let two = admitted(&mut store, "two-capture")?;
    let pending = make_fact("attempt-two", 2, vec![two.clone()])?;
    assert!(capture(&mut store, &pending, "evt_collision", true).is_err());
    assert!(!store
        .remote_mailbox_message(&scope()?, &two.local_message)?
        .consumed());
    assert!(store
        .attempt_receipt(&pending.attempt, ceiling()?)?
        .is_none());
    assert_eq!(event_count(&store)?, 1);
    Ok(())
}
#[test]
fn binding_change_refuses_capture_before_consumption() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    prepare(&mut store)?;
    let reference = admitted(&mut store, "binding-change")?;
    store
        .connection()
        .execute("UPDATE instruction_entry SET value='other-principal'", [])?;
    let pending = make_fact("attempt-bound", 1, vec![reference.clone()])?;
    assert!(capture(&mut store, &pending, "evt_binding", true).is_err());
    assert!(!store
        .remote_mailbox_message(&scope()?, &reference.local_message)?
        .consumed());
    assert!(store
        .attempt_receipt(&pending.attempt, ceiling()?)?
        .is_none());
    Ok(())
}
#[test]
fn quiet_archived_and_unacknowledged_rows_cannot_be_captured() -> TestResult {
    for variant in 0..3 {
        let mut store = EventStore::open_in_memory()?;
        prepare(&mut store)?;
        let reference = if variant == 2 {
            let ack = store
                .admit_remote_mailbox(&scope()?, &input("unsettled")?)?
                .ack;
            PresentedMail {
                local_message: ack.local_message().clone(),
                source_message: ack.source_message().clone(),
                local_admission: ack.local_admission(),
                digest: ack.canonical_message_digest().clone(),
            }
        } else {
            let reference = admitted(&mut store, "changed")?;
            let mut changed = input("changed")?;
            if variant == 0 {
                changed.attention = MailAttention::Quiet;
            } else {
                changed.archive =
                    SourceArchiveDisposition::Archived(SourceArchiveReason::RecipientDismissed);
            }
            store.admit_remote_mailbox(&scope()?, &changed)?;
            reference
        };
        assert!(capture(
            &mut store,
            &make_fact("attempt-quiet", 1, vec![reference.clone()])?,
            "evt_quiet",
            true
        )
        .is_err());
        assert!(!store
            .remote_mailbox_message(&scope()?, &reference.local_message)?
            .consumed());
        assert_eq!(event_count(&store)?, 0);
    }
    Ok(())
}
#[test]
fn capture_scope_and_original_coordinates_cannot_be_substituted() -> TestResult {
    for variant in 0..6 {
        let mut store = EventStore::open_in_memory()?;
        prepare(&mut store)?;
        let reference = admitted(&mut store, "original")?;
        let mut pending = make_fact("attempt-forgery", 1, vec![reference.clone()])?;
        match variant {
            0 => pending.principal = SourcePrincipalId::try_new("foreign")?,
            1 => {
                pending.projects = ProjectPair::new(
                    SourceProjectId::try_new("foreign")?,
                    pending.projects.target().clone(),
                )
            }
            2 => pending.recipient = SeatRecipientId::try_new("foreign")?,
            3 => {
                let mut forged = reference.clone();
                forged.digest = MailboxMessageDigest::try_new("forged")?;
                pending = make_fact("attempt-forgery", 1, vec![forged])?;
            }
            4 => {
                let mut forged = reference.clone();
                forged.source_message = RemoteMailboxMessageId::try_new("foreign")?;
                pending = make_fact("attempt-forgery", 1, vec![forged])?;
            }
            _ => {
                let mut forged = reference.clone();
                forged.local_admission =
                    grapt_types::MailboxAdmissionSeq::new(999).ok_or("sequence")?;
                pending = make_fact("attempt-forgery", 1, vec![forged])?;
            }
        }
        assert!(capture(&mut store, &pending, "evt_forgery", true).is_err());
        assert!(!store
            .remote_mailbox_message(&scope()?, &reference.local_message)?
            .consumed());
    }
    Ok(())
}
#[test]
fn physical_retries_share_one_step_notification_budget() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    prepare(&mut store)?;
    let mut refs = Vec::new();
    for i in 0..8 {
        refs.push(admitted(&mut store, &format!("batch-{i}"))?);
    }
    capture(
        &mut store,
        &make_fact("attempt-batch", 1, refs)?,
        "evt_batch",
        true,
    )?;
    let overflow = admitted(&mut store, "overflow")?;
    assert!(capture(
        &mut store,
        &make_fact("attempt-retry", 1, vec![overflow.clone()])?,
        "evt_retry",
        true
    )
    .is_err());
    assert!(!store
        .remote_mailbox_message(&scope()?, &overflow.local_message)?
        .consumed());
    capture(
        &mut store,
        &make_fact("attempt-next-step", 2, vec![overflow])?,
        "evt_next_step",
        true,
    )?;
    assert_eq!(event_count(&store)?, 2);
    Ok(())
}
#[test]
fn historical_capture_refuses_missing_body_consumption_or_corrupt_reference() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    prepare(&mut store)?;
    let reference = admitted(&mut store, "history")?;
    let pending = make_fact("attempt-history", 1, vec![reference.clone()])?;
    assert!(store.remote_mail_presentation(&scope()?, &pending).is_err());
    capture(&mut store, &pending, "evt_history", true)?;
    let mut forged = reference;
    forged.digest = MailboxMessageDigest::try_new("corrupt")?;
    assert!(store
        .remote_mail_presentation(&scope()?, &make_fact("attempt-history", 1, vec![forged])?)
        .is_err());
    let missing = admitted(&mut store, "not-presented")?;
    assert!(store
        .remote_mail_presentation(&scope()?, &make_fact("attempt-missing", 2, vec![missing])?)
        .is_err());
    assert!(grapt_store::decode::decode_row(
        "evt_bad",
        "ses_native_mail",
        1,
        1,
        "session.mail.presented.1",
        "{}"
    )
    .is_err());
    Ok(())
}

#[test]
fn capture_requires_the_exact_permitted_session_step_and_epoch() -> TestResult {
    for variant in 0..4 {
        let mut store = EventStore::open_in_memory()?;
        prepare(&mut store)?;
        let reference = admitted(&mut store, "authority")?;
        let mut pending = make_fact("attempt-authority", 1, vec![reference.clone()])?;
        let aggregate = AggregateId::Session(session()?);
        let writer = store.fencing_token(&aggregate)?;
        let receipt_session = if variant == 2 {
            "ses_foreign".parse()?
        } else {
            session()?
        };
        let receipt_aggregate = AggregateId::Session(receipt_session.clone());
        if variant == 2 {
            store.elect_writer(&receipt_aggregate)?;
        }
        let receipt_writer = store.fencing_token(&receipt_aggregate)?;
        let maximum = NonZeroU32::new(4096).ok_or("maximum")?;
        let receipt = DurableAttemptReceipt::reserved(
            BoundedAttemptId::new(pending.attempt.clone(), maximum)
                .map_err(|e| format!("{e:?}"))?,
            BoundedSessionId::new(receipt_session, maximum).map_err(|e| format!("{e:?}"))?,
            pending.step,
            AttemptOrdinal::new(1).ok_or("ordinal")?,
            ReservedMarker::new(Timestamp::from_millis(1)),
        );
        if variant == 3 {
            pending.step = StepNumber::new(2).ok_or("step")?;
        }
        let terminal = TerminalCoordinates::new("evt_authority_terminal".parse()?, 3);
        let event: EventId = "evt_authority_capture".parse()?;
        let data = pending.encode();
        let tx = store.governed_transaction()?;
        if variant != 0 {
            EventStore::reserve_attempt_receipt_within(
                &tx,
                &ReserveInput {
                    receipt: &receipt,
                    writer: receipt_writer,
                    terminal: &terminal,
                    ceiling: ceiling()?,
                },
            )?;
            if variant != 1 {
                EventStore::permit_attempt_send_within(
                    &tx,
                    &pending.attempt,
                    receipt_writer,
                    ceiling()?,
                    RequestSentMarker::new(Timestamp::from_millis(2)),
                )?;
            }
        }
        assert!(EventStore::append_remote_mail_presentation_within(
            &tx,
            &scope()?,
            &binding()?,
            AppendInput {
                id: &event,
                aggregate,
                writer,
                kind_name: "session.mail.presented",
                version: EventVersion::new(1).ok_or("version")?,
                created_ms: 2,
                data: &data
            },
            ceiling()?
        )
        .is_err());
        drop(tx);
        assert!(!store
            .remote_mailbox_message(&scope()?, &reference.local_message)?
            .consumed());
        assert_eq!(event_count(&store)?, 0);
    }
    Ok(())
}

#[test]
fn historical_capture_refuses_deleted_and_corrupted_canonical_bodies() -> TestResult {
    for delete in [false, true] {
        let db = OwnedDb::new("capture-corruption")?;
        let mut store = db.open()?;
        prepare(&mut store)?;
        let reference = admitted(&mut store, "corrupt-body")?;
        let pending = make_fact("attempt-corrupt-body", 1, vec![reference.clone()])?;
        capture(&mut store, &pending, "evt_corrupt_body", true)?;
        if delete {
            rusqlite::Connection::open(db.path())?.execute(
                "DELETE FROM mailbox_message WHERE message_id=?1",
                [reference.local_message.as_str()],
            )?;
        } else {
            mutate(&db.path(), reference.local_message.as_str(), |bytes| {
                bytes.truncate(7);
            })?;
        }
        assert!(store.remote_mail_presentation(&scope()?, &pending).is_err());
    }
    Ok(())
}

#[test]
fn public_generic_append_cannot_bypass_atomic_mail_capture() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    prepare(&mut store)?;
    let reference = admitted(&mut store, "generic-bypass")?;
    let pending = make_fact("attempt-bypass", 1, vec![reference.clone()])?;
    let aggregate = AggregateId::Session(session()?);
    let writer = store.fencing_token(&aggregate)?;
    let event: EventId = "evt_bypass".parse()?;
    let data = pending.encode();
    let tx = store.governed_transaction()?;
    assert!(EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.mail.presented",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 2,
            data: &data
        },
        &mut []
    )
    .is_err());
    tx.commit()?.discard();
    assert_eq!(event_count(&store)?, 0);
    assert!(!store
        .remote_mailbox_message(&scope()?, &reference.local_message)?
        .consumed());
    Ok(())
}

#[test]
fn a_new_writer_cannot_adopt_the_old_attempts_capture_permission() -> TestResult {
    let db = OwnedDb::new("capture-epoch")?;
    let mut old = db.open()?;
    prepare(&mut old)?;
    let reference = admitted(&mut old, "epoch-change")?;
    let pending = make_fact("attempt-old-epoch", 1, vec![reference.clone()])?;
    let aggregate = AggregateId::Session(session()?);
    let writer = old.fencing_token(&aggregate)?;
    let maximum = NonZeroU32::new(4096).ok_or("maximum")?;
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(pending.attempt.clone(), maximum).map_err(|e| format!("{e:?}"))?,
        BoundedSessionId::new(session()?, maximum).map_err(|e| format!("{e:?}"))?,
        pending.step,
        AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(1)),
    );
    let terminal = TerminalCoordinates::new("evt_old_epoch_terminal".parse()?, 3);
    let tx = old.governed_transaction()?;
    EventStore::reserve_attempt_receipt_within(
        &tx,
        &ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling: ceiling()?,
        },
    )?;
    EventStore::permit_attempt_send_within(
        &tx,
        &pending.attempt,
        writer,
        ceiling()?,
        RequestSentMarker::new(Timestamp::from_millis(2)),
    )?;
    tx.commit()?.discard();
    let mut new = db.open()?;
    new.elect_writer(&aggregate)?;
    let writer = new.fencing_token(&aggregate)?;
    let event: EventId = "evt_new_owner_capture".parse()?;
    let data = pending.encode();
    let tx = new.governed_transaction()?;
    assert!(EventStore::append_remote_mail_presentation_within(
        &tx,
        &scope()?,
        &binding()?,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.mail.presented",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 2,
            data: &data
        },
        ceiling()?
    )
    .is_err());
    drop(tx);
    assert!(!new
        .remote_mailbox_message(&scope()?, &reference.local_message)?
        .consumed());
    assert_eq!(event_count(&new)?, 0);
    Ok(())
}

#[test]
fn unclassifiable_prior_capture_cannot_disappear_from_the_step_budget() -> TestResult {
    let db = OwnedDb::new("capture-unclassifiable")?;
    let mut store = db.open()?;
    prepare(&mut store)?;
    let one = admitted(&mut store, "prior-corrupt")?;
    capture(
        &mut store,
        &make_fact("attempt-prior", 1, vec![one])?,
        "evt_prior",
        true,
    )?;
    rusqlite::Connection::open(db.path())?
        .execute("UPDATE event SET data='{}' WHERE id='evt_prior'", [])?;
    let pending = admitted(&mut store, "after-corrupt")?;
    assert!(capture(
        &mut store,
        &make_fact("attempt-after-corrupt", 2, vec![pending.clone()])?,
        "evt_after_corrupt",
        true
    )
    .is_err());
    assert!(!store
        .remote_mailbox_message(&scope()?, &pending.local_message)?
        .consumed());
    Ok(())
}
#[test]
fn all_rows_validate_before_the_first_consumption_change() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    prepare(&mut store)?;
    let one = admitted(&mut store, "valid-first")?;
    let two = admitted(&mut store, "quiet-second")?;
    let mut changed = input("quiet-second")?;
    changed.attention = MailAttention::Quiet;
    store.admit_remote_mailbox(&scope()?, &changed)?;
    let pending = make_fact("attempt-full-precheck", 1, vec![one.clone(), two])?;
    let aggregate = AggregateId::Session(session()?);
    let writer = store.fencing_token(&aggregate)?;
    let maximum = NonZeroU32::new(4096).ok_or("maximum")?;
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(pending.attempt.clone(), maximum).map_err(|e| format!("{e:?}"))?,
        BoundedSessionId::new(session()?, maximum).map_err(|e| format!("{e:?}"))?,
        pending.step,
        AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(1)),
    );
    let terminal = TerminalCoordinates::new("evt_full_precheck_terminal".parse()?, 3);
    let event: EventId = "evt_full_precheck".parse()?;
    let data = pending.encode();
    let tx = store.governed_transaction()?;
    EventStore::reserve_attempt_receipt_within(
        &tx,
        &ReserveInput {
            receipt: &receipt,
            writer,
            terminal: &terminal,
            ceiling: ceiling()?,
        },
    )?;
    EventStore::permit_attempt_send_within(
        &tx,
        &pending.attempt,
        writer,
        ceiling()?,
        RequestSentMarker::new(Timestamp::from_millis(2)),
    )?;
    assert!(EventStore::append_remote_mail_presentation_within(
        &tx,
        &scope()?,
        &binding()?,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.mail.presented",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 2,
            data: &data
        },
        ceiling()?
    )
    .is_err());
    let consumed: i64 = tx.query_row(
        "SELECT consumed FROM mailbox_message WHERE message_id=?1",
        [one.local_message.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(
        consumed, 0,
        "an invalid later row must be found before changing the first row"
    );
    drop(tx);
    assert!(store
        .attempt_receipt(&pending.attempt, ceiling()?)?
        .is_none());
    Ok(())
}
