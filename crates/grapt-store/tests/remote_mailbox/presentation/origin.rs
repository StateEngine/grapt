//! Captured history binds recipient authority without replacing row provenance.
use super::*;

fn origin_reference(
    store: &mut EventStore,
    origin: &str,
) -> Result<PresentedMail, Box<dyn std::error::Error>> {
    let mut row = input("same-id")?;
    row.scope = row.scope.for_origin(SourceProjectId::try_new(origin)?);
    let ack = store.admit_remote_mailbox(&row.scope, &row)?.ack;
    store.settle_remote_mailbox_delivery(
        &row.scope,
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

#[test]
fn captured_mixed_origins_survive_reopen_without_resurrection() -> TestResult {
    let db = OwnedDb::new("mixed-origin-capture")?;
    let mut store = db.open()?;
    prepare(&mut store)?;
    let one = origin_reference(&mut store, "origin-one")?;
    let two = origin_reference(&mut store, "origin-two")?;
    let fact = make_fact("mixed-origin-attempt", 1, vec![one.clone(), two.clone()])?;
    capture(&mut store, &fact, "evt_mixed_origin", false)?;
    assert_eq!(event_count(&store)?, 0);
    let rows = store
        .read_remote_mailbox_for_binding(&scope()?, MailboxCursor::START, 64, 262144)?
        .records;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| !row.consumed()));
    capture(&mut store, &fact, "evt_mixed_origin", true)?;
    drop(store);
    let mut store = db.open()?;
    let replay = store.remote_mail_presentation(&scope()?, &fact)?;
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].scope().projects.source().as_str(), "origin-one");
    assert_eq!(replay[1].scope().projects.source().as_str(), "origin-two");
    assert_eq!(replay[0].message().body().as_str(), "bounded complete mail");
    assert!(replay.iter().all(|row| row.consumed() && !row.actionable()));
    assert!(replay.iter().all(|row| !row.delivery_pending()));
    assert_ne!(
        replay[0].ack().local_message(),
        replay[1].ack().local_message()
    );
    store.elect_writer(&AggregateId::Session(session()?))?;
    assert!(capture(
        &mut store,
        &make_fact("mixed-origin-repeat", 2, vec![one, two])?,
        "evt_mixed_repeat",
        true
    )
    .is_err());
    assert_eq!(event_count(&store)?, 1);
    Ok(())
}

#[test]
fn foreign_recipient_authority_cannot_enter_mixed_origin_capture() -> TestResult {
    let mut row = input("foreign-origin")?;
    row.scope = row
        .scope
        .for_origin(SourceProjectId::try_new("origin-one")?);
    let mut principal = row.clone();
    principal.scope.principal = SourcePrincipalId::try_new("delta")?;
    let mut recipient = row.clone();
    recipient.scope.recipient = SeatRecipientId::try_new("delta")?;
    let mut target = row.clone();
    target.scope.projects = ProjectPair::new(
        row.scope.projects.source().clone(),
        TargetProjectId::try_new("foreign-target")?,
    );
    for row in [principal, recipient, target] {
        let mut store = EventStore::open_in_memory()?;
        prepare(&mut store)?;
        let ack = store.admit_remote_mailbox(&row.scope, &row)?.ack;
        store.settle_remote_mailbox_delivery(
            &row.scope,
            &ack,
            Some(SecondaryProjectionState::Posted),
        )?;
        let reference = PresentedMail {
            local_message: ack.local_message().clone(),
            source_message: ack.source_message().clone(),
            local_admission: ack.local_admission(),
            digest: ack.canonical_message_digest().clone(),
        };
        let fact = make_fact("foreign-origin-attempt", 1, vec![reference])?;
        assert!(capture(&mut store, &fact, "evt_foreign_origin", true).is_err());
        assert_eq!(event_count(&store)?, 0);
        assert!(!store
            .remote_mailbox_message(&row.scope, ack.local_message())?
            .consumed());
        assert!(store.attempt_receipt(&fact.attempt, ceiling()?)?.is_none());
    }
    Ok(())
}
