use grapt_inbox::{AdmitRequest, Inbox, InboxError, PromoteScope};
use grapt_store::{compaction, AggregateId, EventStore};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
    CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, ContextWindowId,
    DeliveryMode, EventId, EventSeq, NextRequestRef, RetainedHistory, RetentionPolicyVersion,
    SessionId, StepNumber,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn boundary(inbox: &mut Inbox, session: &SessionId, n: u64) -> Result<(EventId, EventSeq)> {
    let artifact = ArtifactRef::new(
        ArtifactId::new("scc-artifact").ok_or("artifact")?,
        12,
        ArtifactDigest::new("digest").ok_or("digest")?,
    );
    let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new(format!("scc-{n}")).ok_or("compaction")?,
        source_session: session.clone(),
        source_epoch: ContextEpoch::new(n),
        target_window: ContextWindowId::new(format!("scc-window-{n}")).ok_or("window")?,
        target_epoch: ContextEpoch::new(n + 1),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new("scc-v1").ok_or("policy")?,
        summary: CompactionSummary::ClientReadable {
            artifact: artifact.clone(),
        },
        retained: RetainedHistory::DeterministicSelection {
            descriptor: artifact.clone(),
        },
        baseline: artifact.clone(),
        next_request: NextRequestRef::new(StepNumber::new(1).ok_or("step")?, artifact),
    })
    .map_err(|failure| format!("descriptor {failure:?}"))?;
    let event: EventId = format!("evt_scc_boundary{n}").parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let held = inbox.store().held_writers();
    let tx = inbox.store_mut().governed_transaction()?;
    let outcome = compaction::record(
        &tx,
        &event,
        held.token(&aggregate)?,
        100,
        compaction::CompactionAttempt::Settled(&descriptor),
        &mut [],
    )?;
    tx.commit()?.discard();
    match outcome {
        compaction::CompactionOutcome::Settled { seq } => Ok((event, EventSeq::new(seq))),
        other => Err(format!("boundary did not commit: {other:?}").into()),
    }
}

#[test]
fn scc_promoted_cut_is_inclusive_bounded_and_generation_bound() -> Result {
    let session: SessionId = "ses_scc_cut".parse()?;
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    for n in 0..3 {
        inbox.admit(AdmitRequest {
            event_id: &format!("evt_scc_admit{n}").parse()?,
            session: &session,
            input: &format!("msg_scc_input{n}").parse()?,
            text: "retained input",
            delivery: DeliveryMode::Steer,
            resume: false,
            created_ms: 10,
        })?;
        inbox.promote_next(
            &session,
            PromoteScope::InputAtIdle,
            &mut || {
                format!("evt_scc_promote{n}")
                    .parse()
                    .unwrap_or_else(|_| panic!("fixture event"))
            },
            11,
        )?;
    }
    let all = inbox.promoted_messages(&session, 3)?;
    assert_eq!(all.len(), 3);
    assert_eq!(
        inbox.promoted_input_before(&session, EventSeq::new(all[0].seq))?,
        None
    );
    assert_eq!(
        inbox.promoted_input_before(&session, EventSeq::new(all[1].seq))?,
        Some(all[0].input.clone())
    );
    assert_eq!(
        inbox.promoted_input_before(&session, EventSeq::new(all[2].seq + 1))?,
        Some(all[2].input.clone())
    );
    assert_eq!(
        inbox.promoted_input_before(&"ses_foreign".parse()?, EventSeq::new(all[2].seq + 1))?,
        None
    );
    assert!(inbox
        .promoted_input_before(&session, EventSeq::new(u64::MAX))
        .is_err());
    assert!(inbox.promoted_messages(&session, 2).is_err());
    let (generation, committed) = boundary(&mut inbox, &session, 1)?;
    let cut = EventSeq::new(all[1].seq);
    let selected = inbox.promoted_messages_from(&session, cut, &generation, 2)?;
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].input, all[1].input);
    assert_eq!(selected[1].input, all[2].input);
    assert!(inbox
        .promoted_messages_from(&session, cut, &generation, 1)
        .is_err());
    assert!(inbox
        .promoted_messages_from(&session, committed, &generation, 0)?
        .is_empty());
    assert!(matches!(
        inbox.promoted_messages_from(&"ses_foreign".parse()?, cut, &generation, 2),
        Err(InboxError::HistoryBoundary { .. })
    ));
    assert!(inbox
        .promoted_messages_from(
            &session,
            EventSeq::new(committed.value() + 1),
            &generation,
            2
        )
        .is_err());
    let (new_generation, _) = boundary(&mut inbox, &session, 2)?;
    assert!(matches!(
        inbox.promoted_messages_from(&session, cut, &generation, 2),
        Err(InboxError::HistoryBoundary { .. })
    ));
    assert_eq!(
        inbox
            .promoted_messages_from(&session, cut, &new_generation, 2)?
            .len(),
        2
    );
    // Invalid selected projection data is not silently dropped by the cut.
    inbox.store().connection().execute(
        "UPDATE session_message SET id = 'bad id' WHERE id = ?1",
        [all[1].input.as_str()],
    )?;
    assert!(inbox
        .promoted_messages_from(&session, cut, &new_generation, 2)
        .is_err());
    assert!(inbox
        .promoted_input_before(&session, EventSeq::new(all[2].seq))
        .is_err());
    Ok(())
}

#[test]
fn scc_boundary_reopens_and_corrupt_descriptor_refuses() -> Result {
    let root = std::env::temp_dir().join(format!("grapt-scc-boundary-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let database = root.join("events.sqlite3");
    let session: SessionId = "ses_scc_reopen".parse()?;
    let mut store = EventStore::open(&database, "scc-boundary")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    let (generation, _) = boundary(&mut inbox, &session, 3)?;
    drop(inbox);
    let inbox = Inbox::new(EventStore::open(&database, "scc-reopened")?)?;
    assert!(inbox
        .promoted_messages_from(&session, EventSeq::FIRST, &generation, 0)?
        .is_empty());
    let external = rusqlite::Connection::open(&database)?;
    external.execute(
        "UPDATE event SET data = '{}' WHERE id = ?1",
        [generation.as_str()],
    )?;
    assert!(matches!(
        inbox.promoted_messages_from(&session, EventSeq::FIRST, &generation, 0),
        Err(InboxError::Compaction(_))
    ));
    drop(external);
    drop(inbox);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
