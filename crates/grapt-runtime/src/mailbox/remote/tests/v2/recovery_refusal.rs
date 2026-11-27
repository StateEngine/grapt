//! Reopen with original durable ACK debt and a provider refusing its replay.
use super::*;

#[tokio::test]
async fn original_ack_refusal_stays_actionable_without_losing_debt_or_resuming() -> ResultTest {
    let inbox = new_inbox()?;
    let message = message("healthy", "bind-1", MailAttention::Quiet)?;
    let ack = predicted_ack(&message)?;
    let mut first = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(page(vec![message], vec![], false)?)],
            acks: vec![Ok(ack_receipt(&ack))],
            closes: vec![Ok(ShutdownReceipt::new(0, 1))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    first.port.lose_next_ack_response();
    let (_cancel, signal) = watch::channel(None);
    assert!(first.advance_slice(&signal).await.is_err());
    first.close().await?;
    let mut resumed = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-2")?)],
            acks: vec![Err(DeliveryAckFailure::MessageNeverEnumerated)],
            closes: vec![Ok(ShutdownReceipt::new(0, 1))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let error = resumed
        .advance_slice(&signal)
        .await
        .err()
        .ok_or("original ACK was unexpectedly accepted")?;
    assert!(
        error.to_string().contains("MessageNeverEnumerated"),
        "specific recovery refusal was erased: {error}"
    );
    assert!(!resumed.is_live());
    assert_eq!(counts(&inbox)?, (1, 1));
    {
        let held = inbox.lock().map_err(|_| "inbox")?;
        let record = held
            .store()
            .remote_mailbox_message(&scope()?, ack.local_message())?;
        assert_eq!(record.ack(), &ack);
        assert!(record.delivery_pending() || record.repair_pending());
        assert!(!record.consumed());
    }
    resumed.close().await?;
    assert_eq!(
        resumed.port.calls(),
        &[
            PortCall::Open,
            PortCall::AcknowledgeDelivery { ack: Box::new(ack) },
            PortCall::Close,
        ]
    );
    Ok(())
}
