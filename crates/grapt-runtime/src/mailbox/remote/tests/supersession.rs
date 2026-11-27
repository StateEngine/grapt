//! Supersession reaches a later model handoff without changing prior captures.
use super::*;
use grapt_event::AggregateId;
use grapt_store::{AppendInput, AttemptRecordCeiling, ReserveInput, TerminalCoordinates};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, EventVersion,
    PhysicalAttemptId, RequestSentMarker, ReservedMarker, SessionId, StepNumber, Timestamp,
};

fn mail(
    name: &str,
    supersedes: Option<&str>,
) -> Result<grapt_store::RemoteMailboxInput, Box<dyn std::error::Error>> {
    let mut row = input(&scope()?, &message(name, "original", MailAttention::Wake)?);
    row.supersedes = supersedes
        .map(RemoteMailboxMessageId::try_new)
        .transpose()?;
    Ok(row)
}

fn admit(
    owner: &RemoteMailboxRuntime<DeterministicTransport>,
    row: &grapt_store::RemoteMailboxInput,
) -> ResultTest {
    let mut inbox = owner.inbox.lock().map_err(|_| "inbox")?;
    let ack = inbox.store_mut().admit_remote_mailbox(&row.scope, row)?.ack;
    inbox
        .store_mut()
        .settle_remote_mailbox_delivery(&row.scope, &ack, None)?;
    Ok(())
}

fn step(value: u32) -> Result<StepNumber, Box<dyn std::error::Error>> {
    Ok(StepNumber::new(value).ok_or("step")?)
}

fn capture(store: &mut EventStore, batch: &NativeMailBatch, number: u32) -> ResultTest {
    let session: SessionId = "ses_notification".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let maximum = NonZeroU32::new(4096).ok_or("ceiling")?;
    let ceiling = AttemptRecordCeiling::new(maximum);
    let attempt = PhysicalAttemptId::new(format!("mail-attempt-{number}")).ok_or("attempt")?;
    let fact = batch.presentation(&attempt, step(number)?)?;
    let receipt = DurableAttemptReceipt::reserved(
        BoundedAttemptId::new(attempt.clone(), maximum).map_err(|error| format!("{error:?}"))?,
        BoundedSessionId::new(session, maximum).map_err(|error| format!("{error:?}"))?,
        step(number)?,
        AttemptOrdinal::new(1).ok_or("ordinal")?,
        ReservedMarker::new(Timestamp::from_millis(1)),
    );
    let terminal = TerminalCoordinates::new(format!("evt_mail_terminal_{number}").parse()?, 3);
    let event = format!("evt_mail_capture_{number}").parse()?;
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
        &attempt,
        writer,
        ceiling,
        RequestSentMarker::new(Timestamp::from_millis(2)),
    )?;
    batch.capture_within(
        &tx,
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
    Ok(())
}

fn replay(store: &EventStore, number: u32) -> Result<String, Box<dyn std::error::Error>> {
    let facts =
        store.remote_mail_presentations_for_step(&"ses_notification".parse()?, step(number)?)?;
    assert_eq!(facts.len(), 1);
    let records = store.remote_mail_presentation(&scope()?, &facts[0])?;
    Ok(records
        .iter()
        .map(render_notification)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n"))
}

#[tokio::test]
async fn later_superseding_mail_reaches_model_handoff_and_preserves_captured_history() -> ResultTest
{
    let mut owner = notification_owner(vec![mail("instruction-a", None)?]).await?;
    {
        let mut inbox = owner.inbox.lock().map_err(|_| "inbox")?;
        inbox
            .store_mut()
            .elect_writer(&AggregateId::Session("ses_notification".parse()?))?;
        let tx = inbox.store_mut().governed_transaction()?;
        tx.execute_batch(
            "CREATE TABLE instruction_entry(session_id TEXT,key TEXT,value TEXT,removed INTEGER);
             INSERT INTO instruction_entry VALUES('ses_notification','stateengine-identity','bound-coder',0);"
        )?;
        tx.commit()?.discard();
    }
    let session = "ses_notification".parse()?;
    let first = owner
        .prepare_notifications(&session, step(1)?, notification_binding()?)?
        .batch
        .ok_or("first handoff")?;
    assert_eq!(first.count(), 1);
    assert!(!first.content().contains("withdrawn"));
    capture(
        owner.inbox.lock().map_err(|_| "inbox")?.store_mut(),
        &first,
        1,
    )?;
    let old_history = replay(owner.inbox.lock().map_err(|_| "inbox")?.store(), 1)?;
    admit(&owner, &mail("instruction-b", Some("instruction-a"))?)?;
    let second = owner
        .prepare_notifications(&session, step(2)?, notification_binding()?)?
        .batch
        .ok_or("second handoff")?;
    assert_eq!(second.count(), 1);
    assert!(second.content().contains("instruction-a") && second.content().contains("withdrawn"));
    let model_input = grapt_provider::CodexInputItem::Message(grapt_provider::InputMessage {
        role: grapt_provider::MessageRole::User,
        content: second.content().to_owned(),
    });
    let grapt_provider::CodexInputItem::Message(model_message) = model_input else {
        return Err("model handoff".into());
    };
    assert!(
        model_message.content.contains("instruction-b")
            && model_message.content.contains("instruction-a")
    );
    capture(
        owner.inbox.lock().map_err(|_| "inbox")?.store_mut(),
        &second,
        2,
    )?;
    admit(&owner, &mail("instruction-c", Some("instruction-b"))?)?;
    let inbox = owner.inbox.lock().map_err(|_| "inbox")?;
    assert_eq!(replay(inbox.store(), 1)?, old_history);
    assert_eq!(replay(inbox.store(), 2)?, second.content());
    Ok(())
}

#[tokio::test]
async fn either_arrival_order_names_source_identity_and_preserves_other_origins() -> ResultTest {
    for reversed in [false, true] {
        let a = mail("instruction-a", None)?;
        let b = mail("instruction-b", Some("instruction-a"))?;
        let mut rows = vec![a.clone(), b];
        if reversed {
            rows.reverse();
        }
        let mut foreign = a;
        foreign.scope = foreign
            .scope
            .for_origin(SourceProjectId::try_new("independent-origin")?);
        let mut owner = notification_owner(rows).await?;
        admit(&owner, &foreign)?;
        let batch = owner
            .prepare_notifications(
                &"ses_notification".parse()?,
                step(1)?,
                notification_binding()?,
            )?
            .batch
            .ok_or("handoff")?;
        assert_eq!(batch.count(), 2);
        let inbox = owner.inbox.lock().map_err(|_| "inbox")?;
        let records = inbox
            .store()
            .read_remote_mailbox_for_binding(&scope()?, MailboxCursor::START, 64, 4194304)?
            .records;
        let b = records
            .iter()
            .find(|record| record.ack().source_message().as_str() == "instruction-b")
            .ok_or("successor")?;
        let text = render_notification(b)?;
        assert!(text.contains("instruction-a") && text.contains("withdrawn"));
        let expected = scope()?;
        let old = records
            .iter()
            .find(|record| {
                record.scope().projects == expected.projects
                    && record.ack().source_message().as_str() == "instruction-a"
            })
            .ok_or("old row")?;
        assert!(!old.actionable());
        assert!(!text.contains(old.message().id().as_str()));
        let foreign = records
            .iter()
            .find(|record| record.scope().projects.source().as_str() == "independent-origin")
            .ok_or("independent row")?;
        assert!(foreign.actionable());
        assert!(!render_notification(foreign)?.contains("withdrawn"));
    }
    Ok(())
}
