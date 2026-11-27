//! Real canonical ordering/reopen witnesses; no transport/native claim.
use super::*;
use grapt_event::mail_presented::{MailPresented, PresentedMail};
use grapt_store::{
    AppendInput, AttemptRecordCeiling, MailBindingExpectation, RemoteMailboxInput,
    RemoteMailboxScope, ReserveInput, TerminalCoordinates,
};
use grapt_types::remote_mail::*;
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, EventVersion,
    InstructionKey, MailboxBody, ProtocolVersion, RequestSentMarker, ReservedMarker,
    SeatMessageKind, SeatRecipientId,
};
use std::num::NonZeroU32;
type TestResult = Result<(), Box<dyn std::error::Error>>;
#[expect(clippy::too_many_lines, reason = "legacy")]
fn capture(
    fixture: &Fixture,
    id: &str,
    step: u32,
) -> Result<(u64, RemoteMailboxInput), Box<dyn std::error::Error>> {
    let expected = RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("source")?,
            TargetProjectId::try_new("secode")?,
        ),
        principal: SourcePrincipalId::try_new("coder")?,
        recipient: SeatRecipientId::try_new("coder")?,
    };
    let incoming = RemoteMailboxInput {
        scope: expected.clone(),
        protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
        source_message: RemoteMailboxMessageId::try_new(id)?,
        source_binding: SourceBindingId::try_new("original")?,
        source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{id}"))?,
        sender: SourcePrincipalId::try_new("pilot")?,
        kind: SeatMessageKind::Handoff,
        body: MailboxBody::new("Compact notification; read the bounded full body on demand.")?,
        source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
        refs: Vec::new(),
        supersedes: None,
        superseded_by: None,
        archive: SourceArchiveDisposition::Active,
        attention: MailAttention::Wake,
    };
    let mut inbox = lock(fixture.inbox()?);
    let store = inbox.store_mut();
    let key = InstructionKey::new("stateengine-identity").ok_or("key")?;
    grapt_instruction::InstructionService::put_entry(
        store,
        &fixture.session,
        &key,
        "bound-coder",
        1,
    )?;
    let binding = MailBindingExpectation::new(key, "bound-coder".into())?;
    let ack = store.admit_remote_mailbox(&expected, &incoming)?.ack;
    store.settle_remote_mailbox_delivery(
        &expected,
        &ack,
        Some(SecondaryProjectionState::Posted),
    )?;
    let attempt = PhysicalAttemptId::new(format!("attempt-{id}")).ok_or("attempt")?;
    let step = StepNumber::new(step).ok_or("step")?;
    let maximum = NonZeroU32::new(4096).ok_or("ceiling")?;
    let ceiling = AttemptRecordCeiling::new(maximum);
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(attempt.clone(), maximum).map_err(|e| format!("{e:?}"))?,
        BoundedSessionId::new(fixture.session.clone(), maximum).map_err(|e| format!("{e:?}"))?,
        step,
        AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(1)),
    );
    let event: EventId = format!("evt_mail_{}", step.value()).parse()?;
    let terminal = TerminalCoordinates::new(format!("evt_terminal_{}", step.value()).parse()?, 3);
    let aggregate = AggregateId::Session(fixture.session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let data = MailPresented::new(
        attempt.clone(),
        step,
        expected.projects.clone(),
        expected.principal.clone(),
        expected.recipient.clone(),
        vec![PresentedMail {
            local_message: ack.local_message().clone(),
            source_message: ack.source_message().clone(),
            local_admission: ack.local_admission(),
            digest: ack.canonical_message_digest().clone(),
        }],
    )?
    .encode();
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
        &attempt,
        writer,
        ceiling,
        RequestSentMarker::new(Timestamp::from_millis(2)),
    )?;
    let appended = EventStore::append_remote_mail_presentation_within(
        &tx,
        &expected,
        &binding,
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
    tx.commit()?.discard();
    let grapt_store::Appended::Committed { seq } = appended else {
        return Err("expected new mail capture".into());
    };
    // The permit is ambiguous after a process loss; resolve it through the
    // existing recovery owner before building a subsequent request.
    let tx = store.governed_transaction()?;
    EventStore::settle_attempt_process_loss_within(
        &tx,
        &attempt,
        writer,
        ceiling,
        grapt_types::SettledMarker::new(Timestamp::from_millis(3)),
        &mut [],
    )?;
    tx.commit()?.discard();
    Ok((seq, incoming))
}
#[test]
fn captured_mail_replays_after_tool_results_with_stable_prior_input() -> TestResult {
    let mut fixture = Fixture::new("captured-mail-order")?;
    fixture.record_exchange()?;
    let before = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)?;
    let (_, mut original) = capture(&fixture, "source-order", 2)?;
    let after = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)?;
    assert_eq!(&after[..before.len()], before.as_slice());
    assert_eq!(after.len(), before.len() + 1);
    let Some(CodexInputItem::Message(notification)) = after.last() else {
        return Err("mail tail missing".into());
    };
    assert_eq!(notification.role, MessageRole::User);
    assert!(notification.content.contains("id: \"source-order\""));
    original.archive = SourceArchiveDisposition::Archived(SourceArchiveReason::RecipientDismissed);
    original.attention = MailAttention::Quiet;
    lock(fixture.inbox()?)
        .store_mut()
        .admit_remote_mailbox(&original.scope, &original)?;
    fixture.close();
    assert_eq!(
        fixture
            .reopen(fixture.input.clone())?
            .request_input("use the tool", true)?,
        after
    );
    Ok(())
}
#[test]
fn mail_capture_honors_context_window_and_later_user_ceiling() -> TestResult {
    let fixture = Fixture::new("captured-mail-window")?;
    fixture.record_exchange()?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let before = continuation.request_input("use the tool", true)?;
    let (seq, _) = capture(&fixture, "window-one", 2)?;
    let cut = grapt_types::EventSeq::new(seq);
    assert_eq!(
        continuation
            .request_context("use the tool", true, Some(cut), false)?
            .into_input(),
        before
    );
    let captured = continuation.request_input("use the tool", true)?;
    let next: MessageId = "msg_mail_next_user".parse()?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "new work")?;
    capture(&fixture, "window-later", 3)?;
    assert_eq!(continuation.request_input("use the tool", true)?, captured);
    Ok(())
}
#[test]
fn corrupt_captured_body_refuses_continuation_without_silent_omission() -> TestResult {
    let mut fixture = Fixture::new("captured-mail-corruption")?;
    fixture.record_exchange()?;
    capture(&fixture, "corrupt", 2)?;
    fixture.close();
    rusqlite::Connection::open(&fixture.database)?.execute("DELETE FROM mailbox_message", [])?;
    let result = fixture
        .reopen(fixture.input.clone())?
        .request_input("use the tool", true);
    assert!(result.is_err());
    Ok(())
}
