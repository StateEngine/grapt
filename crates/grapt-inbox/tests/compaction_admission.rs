//! User-requested compaction admission (oracle `SessionPending.admitCompaction`).
use grapt_inbox::{
    AdmitRequest, CompactionAdmission, CompactionAdmitRequest, CompactionFailRequest,
    CompactionFailure, Inbox, InboxError, InboxProjector, PromoteScope,
};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{DeliveryMode, EventId, EventVersion, MessageId, SessionId};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn inbox(session: &SessionId) -> Result<Inbox, Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    Ok(Inbox::new(store)?)
}

fn compact(
    inbox: &mut Inbox,
    event: &str,
    session: &SessionId,
    input: &str,
) -> Result<CompactionAdmission, Box<dyn std::error::Error>> {
    let event: EventId = event.parse()?;
    let input: MessageId = input.parse()?;
    Ok(inbox.admit_compaction(CompactionAdmitRequest {
        event_id: &event,
        session,
        input: &input,
        created_ms: 1_789_560_000_000,
        instructions: None,
    })?)
}

fn steer(inbox: &mut Inbox, session: &SessionId, input: &str) -> TestResult {
    let event: EventId = format!("evt_{input}").parse()?;
    let input: MessageId = input.parse()?;
    inbox.admit(AdmitRequest {
        event_id: &event,
        session,
        input: &input,
        text: "hello",
        delivery: DeliveryMode::Steer,
        resume: false,
        created_ms: 1_789_560_000_000,
    })?;
    Ok(())
}

/// Appends one durable fact through the inbox projector, as the writer that
/// owns it would.
fn append(
    inbox: &mut Inbox,
    session: &SessionId,
    id: &str,
    kind: &str,
    version: u32,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let id: EventId = id.parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let held = inbox.store().held_writers();
    let tx = inbox.store_mut().governed_transaction()?;
    EventStore::append_within(
        &tx,
        AppendInput {
            id: &id,
            aggregate: aggregate.clone(),
            writer: held.token(&aggregate)?,
            kind_name: kind,
            version: EventVersion::new(version).ok_or("version")?,
            created_ms: 1_789_560_000_001,
            data,
        },
        &mut [&mut InboxProjector],
    )?;
    tx.commit()?.discard();
    Ok(())
}

fn events(inbox: &Inbox, kind: &str) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = ?1",
        [kind],
        |row| row.get(0),
    )?)
}

fn promote(inbox: &mut Inbox, session: &SessionId) -> Result<bool, Box<dyn std::error::Error>> {
    let promotion: EventId = "evt_compact_promotion".parse()?;
    let mut mint = || promotion.clone();
    Ok(inbox
        .promote_next(session, PromoteScope::InputAtIdle, &mut mint, 1)?
        .is_some())
}

#[test]
fn guidance_is_bounded_and_cannot_change_an_already_admitted_request() -> TestResult {
    let session: SessionId = "ses_guided_compact".parse()?;
    let mut inbox = inbox(&session)?;
    let event: EventId = "evt_guided_compact".parse()?;
    let input: MessageId = "msg_guided_compact".parse()?;
    let submit = |inbox: &mut Inbox, instructions: &str| {
        inbox.admit_compaction(CompactionAdmitRequest {
            event_id: &event,
            session: &session,
            input: &input,
            created_ms: 1,
            instructions: Some(instructions),
        })
    };
    for invalid in [String::new(), " \n".to_owned(), "x".repeat(16 * 1024 + 1)] {
        assert!(submit(&mut inbox, &invalid).is_err());
    }
    assert!(inbox.pending_compaction(&session)?.is_none());
    assert_eq!(events(&inbox, "session.compaction.admitted.1")?, 0);
    let first = submit(&mut inbox, "Preserve the design decision")?;
    assert_eq!(
        inbox.pending_compaction(&session)?.as_ref(),
        Some(first.pending())
    );
    assert!(matches!(
        submit(&mut inbox, "Preserve the design decision")?,
        CompactionAdmission::AlreadyPending(_)
    ));
    assert!(matches!(
        submit(&mut inbox, "Replace the instruction"),
        Err(InboxError::Conflict {
            differs: "instructions",
            ..
        })
    ));
    assert_eq!(events(&inbox, "session.compaction.admitted.1")?, 1);
    Ok(())
}

#[test]
fn one_compaction_is_pending_per_session_until_its_manual_terminal() -> TestResult {
    let session: SessionId = "ses_compact_one".parse()?;
    let mut inbox = inbox(&session)?;
    steer(&mut inbox, &session, "msg_compact_steer")?;
    let first = compact(&mut inbox, "evt_compact_1", &session, "msg_compact_1")?;
    assert!(matches!(first, CompactionAdmission::Admitted(_)));
    assert_eq!(
        inbox.pending_compaction(&session)?.as_ref(),
        Some(first.pending())
    );

    // A replay of the same id, and another request while one is pending, both
    // answer with the pending compaction and append nothing.
    let replay = compact(&mut inbox, "evt_compact_2", &session, "msg_compact_1")?;
    let second = compact(&mut inbox, "evt_compact_3", &session, "msg_compact_2")?;
    assert_eq!(
        replay,
        CompactionAdmission::AlreadyPending(first.pending().clone())
    );
    assert_eq!(
        second,
        CompactionAdmission::AlreadyPending(first.pending().clone())
    );
    assert_eq!(events(&inbox, "session.compaction.admitted.1")?, 1);

    // The compaction is not an input and never holds one: it runs inside the
    // next promoted input's first Step, so it stays pending past promotion.
    assert_eq!(inbox.pending_inputs(&session)?.len(), 1);
    assert!(
        promote(&mut inbox, &session)?,
        "a pending compaction held back the input it must run with"
    );
    assert!(inbox.pending_compaction(&session)?.is_some());

    // An automatic terminal does not settle it; the manual one does.
    append(
        &mut inbox,
        &session,
        "evt_compact_auto",
        "session.compaction.failed",
        1,
        &json!({"sessionID": session.as_str(), "reason": "auto",
                "error": {"type": "x", "message": "y"}}),
    )?;
    assert!(inbox.pending_compaction(&session)?.is_some());
    append(
        &mut inbox,
        &session,
        "evt_compact_manual",
        "session.compaction.failed",
        1,
        &json!({"sessionID": session.as_str(), "reason": "manual",
                "error": {"type": "x", "message": "y"}, "inputID": "msg_compact_1"}),
    )?;
    assert_eq!(inbox.pending_compaction(&session)?, None);

    // A settled request's id never runs a second compaction.
    let rerun = compact(&mut inbox, "evt_compact_4", &session, "msg_compact_1");
    assert!(
        matches!(
            rerun
                .as_ref()
                .map_err(|error| error.downcast_ref::<InboxError>()),
            Err(Some(InboxError::CompactionSettled { .. }))
        ),
        "{rerun:?}"
    );
    Ok(())
}

#[test]
fn the_rust_descriptor_settles_only_a_user_requested_compaction() -> TestResult {
    let session: SessionId = "ses_compact_descriptor".parse()?;
    let mut inbox = inbox(&session)?;
    compact(&mut inbox, "evt_compact_d", &session, "msg_compact_d")?;
    // The inbox reads only `sourceSession` and `trigger`; the store's own
    // codec owns the rest of the descriptor.
    for (id, trigger, settles) in [
        ("evt_descriptor_auto", "contextOverflow", false),
        ("evt_descriptor_user", "userRequested", true),
    ] {
        let decoded = grapt_inbox::InboxEvent::decode(
            "session.compaction.ended",
            2,
            &json!({"sourceSession": session.as_str(), "trigger": trigger}),
        )?;
        assert_eq!(decoded.is_some(), settles, "{id}");
    }
    Ok(())
}

#[test]
fn a_compaction_id_cannot_reuse_an_input_and_a_second_admission_is_refused_atomically() -> TestResult
{
    let session: SessionId = "ses_compact_conflict".parse()?;
    let mut inbox = inbox(&session)?;
    steer(&mut inbox, &session, "msg_compact_taken")?;
    let reused = compact(
        &mut inbox,
        "evt_compact_taken",
        &session,
        "msg_compact_taken",
    );
    assert!(
        matches!(
            reused
                .as_ref()
                .map_err(|error| error.downcast_ref::<InboxError>()),
            Err(Some(InboxError::Conflict {
                differs: "type",
                ..
            }))
        ),
        "{reused:?}"
    );

    compact(&mut inbox, "evt_compact_a", &session, "msg_compact_a")?;
    // A second admission written past the inbox is refused by its projection,
    // and the refusal rolls the append back with it.
    let bypass = append(
        &mut inbox,
        &session,
        "evt_compact_bypass",
        "session.compaction.admitted",
        1,
        &json!({"sessionID": session.as_str(), "inputID": "msg_compact_b"}),
    );
    assert!(bypass.is_err(), "a second pending compaction was projected");
    assert_eq!(events(&inbox, "session.compaction.admitted.1")?, 1);
    assert_eq!(
        inbox
            .pending_compaction(&session)?
            .map(|pending| pending.input),
        Some("msg_compact_a".parse()?)
    );
    Ok(())
}

/// A failed or interrupted run settles the request with the oracle's manual
/// failure, once; with nothing pending it appends nothing.
#[test]
fn a_failed_compaction_settles_once_with_the_oracle_error() -> TestResult {
    let session: SessionId = "ses_compact_fail".parse()?;
    let mut inbox = inbox(&session)?;
    compact(
        &mut inbox,
        "evt_compact_fail_1",
        &session,
        "msg_compact_fail",
    )?;
    let fail = |inbox: &mut Inbox, event: &str| -> Result<_, Box<dyn std::error::Error>> {
        let event: EventId = event.parse()?;
        Ok(inbox.fail_compaction(CompactionFailRequest {
            event_id: &event,
            session: &session,
            failure: CompactionFailure::Aborted,
            created_ms: 2,
        })?)
    };
    let settled = fail(&mut inbox, "evt_compact_fail_2")?;
    assert_eq!(
        settled.map(|pending| pending.input.as_str().to_owned()),
        Some("msg_compact_fail".to_owned())
    );
    assert_eq!(inbox.pending_compaction(&session)?, None);
    let data: String = inbox.store().connection().query_row(
        "SELECT data FROM event WHERE type = 'session.compaction.failed.1'",
        [],
        |row| row.get(0),
    )?;
    let data: Value = serde_json::from_str(&data)?;
    assert_eq!(
        data,
        json!({"sessionID": session.as_str(), "reason": "manual",
               "error": {"type": "aborted", "message": "Compaction cancelled"},
               "inputID": "msg_compact_fail"})
    );
    assert_eq!(fail(&mut inbox, "evt_compact_fail_3")?, None);
    assert_eq!(events(&inbox, "session.compaction.failed.1")?, 1);
    Ok(())
}

/// A compaction admitted before a queued input is not itself offered as that
/// input: the idle boundary promotes the queued message and leaves the
/// compaction pending for that message's first Step.
#[test]
fn an_earlier_compaction_is_never_promoted_as_a_queued_input() -> TestResult {
    let session: SessionId = "ses_compact_order".parse()?;
    let mut inbox = inbox(&session)?;
    compact(
        &mut inbox,
        "evt_compact_order_1",
        &session,
        "msg_compact_order",
    )?;
    let event: EventId = "evt_compact_order_queued".parse()?;
    let input: MessageId = "msg_compact_order_queued".parse()?;
    inbox.admit(AdmitRequest {
        event_id: &event,
        session: &session,
        input: &input,
        text: "after the compaction",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1_789_560_000_001,
    })?;
    let promotion: EventId = "evt_compact_order_promotion".parse()?;
    let mut mint = || promotion.clone();
    let promoted = inbox
        .promote_next(&session, PromoteScope::InputAtIdle, &mut mint, 2)?
        .ok_or("the queued input was not promoted")?;
    assert_eq!(promoted.input, input);
    assert!(inbox.pending_compaction(&session)?.is_some());
    Ok(())
}

fn conflict<T: std::fmt::Debug>(
    outcome: &Result<T, Box<dyn std::error::Error>>,
) -> Option<&'static str> {
    match outcome
        .as_ref()
        .map_err(|error| error.downcast_ref::<InboxError>())
    {
        Err(Some(InboxError::Conflict { differs, .. })) => Some(differs),
        _ => None,
    }
}

fn user(
    inbox: &mut Inbox,
    session: &SessionId,
    event: &str,
    input: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let event: EventId = event.parse()?;
    let input: MessageId = input.parse()?;
    inbox.admit(AdmitRequest {
        event_id: &event,
        session,
        input: &input,
        text: "hello",
        delivery: DeliveryMode::Queue,
        resume: false,
        created_ms: 1_789_560_000_002,
    })?;
    Ok(())
}

/// One MessageId has one
/// durable meaning. A compaction's id stays reserved after it settles and its
/// pending row is gone, across Sessions and input types, in both directions.
#[test]
fn a_settled_compaction_id_is_never_reused_in_another_session_or_as_input() -> TestResult {
    let a: SessionId = "ses_compact_identity_a".parse()?;
    let b: SessionId = "ses_compact_identity_b".parse()?;
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(a.clone()))?;
    store.elect_writer(&AggregateId::Session(b.clone()))?;
    let mut inbox = Inbox::new(store)?;
    compact(&mut inbox, "evt_identity_1", &a, "msg_identity_compact")?;
    append(
        &mut inbox,
        &a,
        "evt_identity_settle",
        "session.compaction.failed",
        1,
        &json!({"sessionID": a.as_str(), "reason": "manual",
                "error": {"type": "x", "message": "y"}, "inputID": "msg_identity_compact"}),
    )?;
    assert_eq!(inbox.pending_compaction(&a)?, None);

    // The same Session replaying the settled id: settled, never a second run.
    let replay = compact(&mut inbox, "evt_identity_2", &a, "msg_identity_compact");
    assert!(
        matches!(
            replay
                .as_ref()
                .map_err(|error| error.downcast_ref::<InboxError>()),
            Err(Some(InboxError::CompactionSettled { .. }))
        ),
        "{replay:?}"
    );
    // Another Session's compaction under that id: a session conflict.
    let other = compact(&mut inbox, "evt_identity_3", &b, "msg_identity_compact");
    assert_eq!(conflict(&other), Some("session"), "{other:?}");
    // A user input under that id, in either Session: refused, not resurrected.
    let as_user = user(&mut inbox, &a, "evt_identity_4", "msg_identity_compact");
    assert!(conflict(&as_user).is_some(), "{as_user:?}");
    let elsewhere = user(&mut inbox, &b, "evt_identity_5", "msg_identity_compact");
    assert_eq!(conflict(&elsewhere), Some("session"), "{elsewhere:?}");
    assert!(inbox.pending_inputs(&a)?.is_empty() && inbox.pending_inputs(&b)?.is_empty());
    assert_eq!(inbox.pending_compaction(&b)?, None);
    assert_eq!(events(&inbox, "session.compaction.admitted.1")?, 1);
    assert_eq!(events(&inbox, "session.input.admitted.1")?, 0);

    // The other direction: a promoted user input's id cannot become a
    // compaction, in its own Session or another.
    user(&mut inbox, &a, "evt_identity_6", "msg_identity_user")?;
    let mut mint = {
        let promotion: EventId = "evt_identity_promotion".parse()?;
        move || promotion.clone()
    };
    inbox
        .promote_next(&a, PromoteScope::InputAtIdle, &mut mint, 3)?
        .ok_or("the user input was not promoted")?;
    let own = compact(&mut inbox, "evt_identity_7", &a, "msg_identity_user");
    assert_eq!(conflict(&own), Some("type"), "{own:?}");
    let foreign = compact(&mut inbox, "evt_identity_8", &b, "msg_identity_user");
    assert_eq!(conflict(&foreign), Some("session"), "{foreign:?}");

    // A genuinely new id still admits.
    let fresh = compact(&mut inbox, "evt_identity_9", &b, "msg_identity_fresh")?;
    assert!(matches!(fresh, CompactionAdmission::Admitted(_)));
    Ok(())
}

/// Writes one history row with no projector: corrupt history the inbox's own
/// writers can never produce, for the bounded identity probe to refuse.
fn raw_history(
    inbox: &mut Inbox,
    session: &SessionId,
    id: &str,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let id: EventId = id.parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let held = inbox.store().held_writers();
    let tx = inbox.store_mut().governed_transaction()?;
    EventStore::append_within(
        &tx,
        AppendInput {
            id: &id,
            aggregate: aggregate.clone(),
            writer: held.token(&aggregate)?,
            kind_name: "session.compaction.admitted",
            version: EventVersion::new(1).ok_or("version")?,
            created_ms: 1_789_560_000_003,
            data,
        },
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok(())
}

fn corruption<T: std::fmt::Debug>(outcome: &Result<T, Box<dyn std::error::Error>>) -> String {
    match outcome
        .as_ref()
        .map_err(|error| error.downcast_ref::<InboxError>())
    {
        Err(Some(InboxError::Corruption { reason, .. })) => reason.clone(),
        other => panic!("expected typed corruption, got {other:?}"),
    }
}

/// The identity probe is bounded.
/// Duplicated or oversized compaction history is typed corruption, never a
/// fresh admission under that id.
#[test]
fn corrupt_compaction_history_is_refused_not_resurrected() -> TestResult {
    let session: SessionId = "ses_compact_corrupt".parse()?;
    let mut inbox = inbox(&session)?;
    let duplicate = json!({"sessionID": session.as_str(), "inputID": "msg_corrupt_dup"});
    raw_history(&mut inbox, &session, "evt_corrupt_dup_1", &duplicate)?;
    raw_history(&mut inbox, &session, "evt_corrupt_dup_2", &duplicate)?;
    let as_user = user(&mut inbox, &session, "evt_corrupt_user", "msg_corrupt_dup");
    assert!(
        corruption(&as_user).contains("more than one compaction admission"),
        "{as_user:?}"
    );
    let again = compact(&mut inbox, "evt_corrupt_again", &session, "msg_corrupt_dup");
    assert!(corruption(&again).contains("more than one"), "{again:?}");

    let oversized = json!({"sessionID": "s".repeat(513), "inputID": "msg_corrupt_big"});
    raw_history(&mut inbox, &session, "evt_corrupt_big", &oversized)?;
    let big = user(
        &mut inbox,
        &session,
        "evt_corrupt_big_user",
        "msg_corrupt_big",
    );
    assert!(corruption(&big).contains("malformed"), "{big:?}");
    assert!(
        inbox.pending_inputs(&session)?.is_empty(),
        "corrupt history was resurrected"
    );
    assert_eq!(events(&inbox, "session.input.admitted.1")?, 0);
    Ok(())
}
