//! The visible count covers row origins under the same receiving authority.
use super::*;
use grapt_store::RemoteMailboxInput;
use grapt_types::{MailboxBody, ProtocolVersion, SeatMessageKind};

fn admitted(
    context: &NativeMailContext,
    scope: RemoteMailboxScope,
    id: &str,
    attention: MailAttention,
) -> Result<grapt_types::remote_mail::ack::DeliveryAdmissionAck, Box<dyn std::error::Error>> {
    let input = RemoteMailboxInput {
        scope,
        protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
        source_message: RemoteMailboxMessageId::try_new(id)?,
        source_binding: SourceBindingId::try_new("original-binding")?,
        source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{id}"))?,
        sender: SourcePrincipalId::try_new("source-author")?,
        kind: SeatMessageKind::Handoff,
        body: MailboxBody::new("bounded mail")?,
        source_created: SourceTimestamp::try_new("2026-10-14T00:00:00.000Z")?,
        refs: Vec::new(),
        supersedes: None,
        superseded_by: None,
        archive: SourceArchiveDisposition::Active,
        attention,
    };
    let mut inbox = context.inbox.lock().map_err(|_| "inbox")?;
    let ack = inbox
        .store_mut()
        .admit_remote_mailbox(&input.scope, &input)?
        .ack;
    inbox.store_mut().settle_remote_mailbox_delivery(
        &input.scope,
        &ack,
        Some(SecondaryProjectionState::Posted),
    )?;
    Ok(ack)
}

#[test]
fn native_attention_counts_mixed_origins_without_crossing_recipient_authority() -> TestResult {
    let mut context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    context.binding = context
        .current_binding()
        .map_err(|error| error.to_string())?;
    let binding = context.binding.as_ref().ok_or("binding")?.scope.clone();
    let one = binding.for_origin(SourceProjectId::try_new("origin-one")?);
    let two = binding.for_origin(SourceProjectId::try_new("origin-two")?);
    let first = admitted(&context, one.clone(), "same-id", MailAttention::Wake)?;
    let second = admitted(&context, two, "same-id", MailAttention::Wake)?;
    assert_ne!(first.local_message(), second.local_message());
    admitted(&context, one.clone(), "quiet", MailAttention::Quiet)?;

    let mut principal = one.clone();
    principal.principal = SourcePrincipalId::try_new("foreign-principal")?;
    let mut recipient = one.clone();
    recipient.recipient = SeatRecipientId::try_new("foreign-recipient")?;
    let mut target = one.clone();
    target.projects = ProjectPair::new(
        one.projects.source().clone(),
        TargetProjectId::try_new("foreign-target")?,
    );
    for scope in [principal, recipient, target] {
        admitted(&context, scope, "foreign", MailAttention::Wake)?;
    }
    assert_eq!(
        context
            .attention_snapshot(true)
            .map_err(|error| error.to_string())?,
        MailAttentionInfo::Ready {
            pending: MailPendingCount::new(2).ok_or("count")?,
            incomplete: false,
            decode_failures: Default::default(),
        }
    );
    context
        .inbox
        .lock()
        .map_err(|_| "inbox")?
        .store_mut()
        .consume_remote_mailbox(&one, first.local_message())?;
    assert_eq!(
        context
            .attention_snapshot(true)
            .map_err(|error| error.to_string())?,
        MailAttentionInfo::Ready {
            pending: MailPendingCount::new(1).ok_or("count")?,
            incomplete: false,
            decode_failures: Default::default(),
        }
    );
    assert_eq!(
        context
            .attention_snapshot(false)
            .map_err(|error| error.to_string())?,
        MailAttentionInfo::Unavailable
    );
    Ok(())
}

#[test]
fn native_attention_mixed_origin_overflow_stays_bounded_and_incomplete() -> TestResult {
    let mut context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    context.binding = context
        .current_binding()
        .map_err(|error| error.to_string())?;
    let binding = context.binding.as_ref().ok_or("binding")?.scope.clone();
    for i in 0..65 {
        let origin = if i % 2 == 0 {
            "origin-one"
        } else {
            "origin-two"
        };
        admitted(
            &context,
            binding.for_origin(SourceProjectId::try_new(origin)?),
            &format!("overflow-{i}"),
            MailAttention::Wake,
        )?;
    }
    assert_eq!(
        context
            .attention_snapshot(true)
            .map_err(|error| error.to_string())?,
        MailAttentionInfo::Ready {
            pending: MailPendingCount::new(64).ok_or("count")?,
            incomplete: true,
            decode_failures: Default::default(),
        }
    );
    Ok(())
}
