//! Recipient binding authority and per-row source provenance are separate.
use super::*;

fn origin_message(
    source: &str,
    id: &str,
    bind: &str,
) -> Result<BoundRemoteSeatMailMessageV1, Box<dyn std::error::Error>> {
    Ok(BoundRemoteSeatMailMessageV1::try_new(
        origin_parts(source, id, bind)?,
        &limits(8, 512).settle(),
    )?)
}
fn origin_parts(
    source: &str,
    id: &str,
    bind: &str,
) -> Result<BoundRemoteSeatMailMessageParts, Box<dyn std::error::Error>> {
    Ok(BoundRemoteSeatMailMessageParts {
        protocol: protocol()?,
        source_binding: SourceBindingId::try_new(bind)?,
        projects: scope()?
            .for_origin(SourceProjectId::try_new(source)?)
            .projects,
        source_message: RemoteMailboxMessageId::try_new(id)?,
        source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{source}-{id}-{bind}"))?,
        sender: SourcePrincipalId::try_new("pilot")?,
        recipient: scope()?.recipient,
        kind: RemoteSeatMailKind::Handoff,
        body: MailboxBody::new("bounded origin body")?,
        refs: BoundedOrderedMailRefs::empty(),
        supersession: SourceSupersession::none(),
        archive: SourceArchiveDisposition::Active,
        source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
        attention: MailAttention::Wake,
    })
}
fn origin_ack(
    mail: &BoundRemoteSeatMailMessageV1,
) -> Result<DeliveryAdmissionAck, Box<dyn std::error::Error>> {
    let row = input(&scope()?, mail);
    Ok(EventStore::open_in_memory()?
        .admit_remote_mailbox(&row.scope, &row)?
        .ack)
}
fn all_origins(
    inbox: &Arc<Mutex<Inbox>>,
) -> Result<Vec<grapt_store::RemoteMailboxRecord>, Box<dyn std::error::Error>> {
    Ok(inbox
        .lock()
        .map_err(|_| "inbox")?
        .store()
        .read_remote_mailbox_for_binding(&scope()?, MailboxCursor::START, 64, 4194304)?
        .records)
}

#[tokio::test]
async fn mixed_origin_equal_ids_admit_ack_and_present_once() -> ResultTest {
    let inbox = new_inbox()?;
    let one = origin_message("origin-one", "same-id", "original")?;
    let two = origin_message("origin-two", "same-id", "original")?;
    let a = origin_ack(&one)?;
    // Admission ordinals are recipient-wide, so predict the second in the same store.
    let mut prediction = EventStore::open_in_memory()?;
    let first = input(&scope()?, &one);
    prediction.admit_remote_mailbox(&first.scope, &first)?;
    let second = input(&scope()?, &two);
    let b = prediction.admit_remote_mailbox(&second.scope, &second)?.ack;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![one, two]))],
            acks: vec![Ok(ack_receipt(&a)), Ok(ack_receipt(&b))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(owner.advance_slice(&signal).await?.live);
    let rows = all_origins(&inbox)?;
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].ack().local_message(), rows[1].ack().local_message());
    assert_eq!(rows[0].ack(), &a);
    assert_eq!(rows[1].ack(), &b);
    assert!(rows.iter().all(|row| !row.delivery_pending()));
    let scan = owner.prepare_notifications(
        &"ses_mixed_origin".parse()?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        notification_binding()?,
    )?;
    let batch = scan.batch.ok_or("mixed-origin batch")?;
    assert_eq!(batch.count(), 2);
    assert!(batch.content().contains("source_project: \"origin-one\""));
    assert!(batch.content().contains("source_project: \"origin-two\""));
    let fact = batch.presentation(
        &grapt_types::PhysicalAttemptId::new("origin-attempt").ok_or("attempt")?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
    )?;
    assert_eq!(fact.messages().len(), 2);
    assert_eq!(
        fact.messages()[0].source_message,
        fact.messages()[1].source_message
    );
    assert_ne!(
        fact.messages()[0].local_message,
        fact.messages()[1].local_message
    );
    let acknowledgements: Vec<_> = owner
        .port
        .calls()
        .iter()
        .filter_map(|call| match call {
            PortCall::AcknowledgeDelivery { ack } => Some(ack.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(acknowledgements, vec![&a, &b]);
    Ok(())
}

#[tokio::test]
async fn cross_origin_reconnect_repairs_original_debt_before_new_snapshot() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = origin_message("outside-origin", "one", "original")?;
    let a = origin_ack(&mail)?;
    let mut first = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![mail]))],
            acks: vec![Err(DeliveryAckFailure::AcknowledgementConflict)],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(first.advance_slice(&signal).await.is_err());
    assert_eq!(all_origins(&inbox)?[0].ack(), &a);
    assert!(all_origins(&inbox)?[0].delivery_pending());
    drop(first);
    let replay = origin_message("outside-origin", "one", "replacement")?;
    let mut second = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("replacement")?)],
            subscribe: vec![Ok(armed(2, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![replay]))],
            acks: vec![Ok(ack_receipt(&a)), Ok(ack_receipt(&a))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    assert!(second.advance_slice(&signal).await?.live);
    let rows = all_origins(&inbox)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ack(), &a);
    assert!(!rows[0].delivery_pending());
    assert_eq!(second.validated_checkpoint()?, Some(a.source_cursor()));
    assert_eq!(
        second.port.calls()[1],
        PortCall::AcknowledgeDelivery {
            ack: Box::new(a.clone())
        }
    );
    assert!(matches!(
        second.port.calls()[2],
        PortCall::SubscribeAndSnapshot { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn foreign_page_coordinates_refuse_the_whole_page_before_admission() -> ResultTest {
    let mut foreign = Vec::new();
    let mut target = origin_parts("legal-origin", "bad-target", "original")?;
    target.projects = ProjectPair::new(
        target.projects.source().clone(),
        TargetProjectId::try_new("elsewhere")?,
    );
    foreign.push(target);
    let mut recipient = origin_parts("legal-origin", "bad-recipient", "original")?;
    recipient.recipient = SeatRecipientId::try_new("delta")?;
    foreign.push(recipient);
    let mut protocol = origin_parts("legal-origin", "bad-protocol", "original")?;
    protocol.protocol = ProtocolVersion::new("foreign-protocol/v1").ok_or("protocol")?;
    foreign.push(protocol);
    foreign.push(origin_parts(
        "legal-origin",
        "bad-binding",
        "other-binding",
    )?);
    for parts in foreign {
        let inbox = new_inbox()?;
        let bad = BoundRemoteSeatMailMessageV1::try_new(parts, &limits(8, 512).settle())?;
        let valid = origin_message("legal-origin", "valid-first", "original")?;
        let ack = origin_ack(&valid)?;
        let mut owner = runtime(
            inbox.clone(),
            TransportScript {
                open: vec![Ok(binding("original")?)],
                subscribe: vec![Ok(armed(1, "origin")?)],
                pages: vec![Ok(RemoteMailPage::final_page(vec![valid, bad]))],
                acks: vec![Ok(ack_receipt(&ack)), Ok(ack_receipt(&ack))],
                ..Default::default()
            },
            limits(8, 512),
        )?;
        let (_sender, signal) = cancel();
        assert!(matches!(
            owner.advance_slice(&signal).await,
            Err(RemoteMailboxError::ForeignReceipt)
        ));
        assert_eq!(row_count(&inbox)?, 0);
        assert_eq!(
            owner
                .port
                .count(|call| matches!(call, PortCall::AcknowledgeDelivery { .. })),
            0
        );
    }
    Ok(())
}
