//! The sealed fake port drives real admission, ACK and reconnect boundaries.
use super::*;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn seat03_fake_v2_burst_and_warm_reconnect_preserve_one_canonical_wake() -> ResultTest {
    let inbox = new_inbox()?;
    let session: grapt_types::SessionId = "ses_notification".parse()?;
    {
        let mut held = inbox.lock().map_err(|_| "inbox")?;
        held.store_mut()
            .elect_writer(&grapt_store::AggregateId::Session(session.clone()))?;
        held.store().connection().execute_batch(
            "CREATE TABLE instruction_entry(session_id TEXT,key TEXT,value TEXT,removed INTEGER);
             INSERT INTO instruction_entry VALUES('ses_notification','stateengine-identity','bound-coder',0);",
        )?;
    }
    let first = message("first", "bind-1", MailAttention::Wake)?;
    let second = message("second", "bind-1", MailAttention::Wake)?;
    let quiet = message("quiet", "bind-1", MailAttention::Quiet)?;
    let acks = [&first, &second, &quiet]
        .into_iter()
        .map(|row| predicted_ack(row).map(|ack| Ok(ack_receipt(&ack))))
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let mut connected = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(page(vec![first, second, quiet], vec![], false)?)],
            acks,
            closes: vec![Ok(ShutdownReceipt::new(0, 1))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_cancel, signal) = watch::channel(None);
    assert!(connected.advance_slice(&signal).await?.live);
    let checkpoint = connected.validated_checkpoint()?.cloned();
    assert!(checkpoint.is_some());
    {
        let mut held = inbox.lock().map_err(|_| "inbox")?;
        let claim = held
            .store()
            .prepare_remote_mail_wake(
                &session,
                &scope()?,
                notification_binding()?,
                MailboxCursor::START,
            )?
            .claim
            .ok_or("coalesced wake")?;
        let names = claim
            .source_messages()
            .map(|id| id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["first", "second"], "quiet mail entered the wake");
        let metadata = claim.metadata();
        let event = "evt_native_wake_fake".parse()?;
        let input = "msg_native_wake_fake".parse()?;
        held.admit_native_mail_wake(
            &claim,
            grapt_inbox::SyntheticAdmitRequest {
                event_id: &event,
                session: &session,
                input: &input,
                text: &names.join("\n"),
                description: Some("Native inbox wake"),
                metadata: Some(&metadata),
                delivery: grapt_types::DeliveryMode::Queue,
                resume: true,
                created_ms: 17,
            },
        )?;
        assert_eq!(
            held.pending_inputs(&session)?.len(),
            1,
            "burst produced multiple inputs"
        );
    }
    connected.close().await?;
    let mut reconnected = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-2")?)],
            subscribe: vec![Ok(subscribed(checkpoint)?)],
            pages: vec![Ok(page(vec![], vec![], false)?)],
            closes: vec![Ok(ShutdownReceipt::new(0, 1))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    assert!(reconnected.advance_slice(&signal).await?.live);
    {
        let held = inbox.lock().map_err(|_| "inbox")?;
        let recovered = held
            .pending_native_mail_wake(&session)?
            .ok_or("recoverable wake")?;
        assert_eq!(recovered.input.as_str(), "msg_native_wake_fake");
        assert_eq!(recovered.text, "first\nsecond");
        assert!(
            held.store()
                .prepare_remote_mail_wake(
                    &session,
                    &scope()?,
                    notification_binding()?,
                    MailboxCursor::START,
                )?
                .claim
                .is_none(),
            "reconnect injected a second wake"
        );
    }
    reconnected.close().await?;
    Ok(())
}
