//! Rows below a reader's mark become presentable again only through paths
//! that lower the mark: a wake re-report in place, or a recovered delivery.
use super::*;

fn settled(
    inbox: &Arc<Mutex<Inbox>>,
    mail: &BoundRemoteSeatMailMessageV1,
) -> Result<DeliveryAdmissionAck, Box<dyn std::error::Error>> {
    let mut held = inbox.lock().map_err(|_| "inbox")?;
    let ack = held
        .store_mut()
        .admit_remote_mailbox(&scope()?, &input(&scope()?, mail))?
        .ack;
    held.store_mut()
        .settle_remote_mailbox_delivery(&scope()?, &ack, None)?;
    Ok(ack)
}

async fn live(
    owner: &mut RemoteMailboxRuntime<DeterministicTransport>,
) -> Result<RemoteMailboxSlice, Box<dyn std::error::Error>> {
    let (_send, cancel) = cancel();
    for _ in 0..4 {
        let slice = owner.advance_slice(&cancel).await?;
        if slice.live {
            return Ok(slice);
        }
    }
    Err("fixture did not converge".into())
}

#[tokio::test]
async fn a_quiet_row_reported_again_as_wake_below_the_mark_is_presented() -> ResultTest {
    let inbox = new_inbox()?;
    let old = settled(&inbox, &message("old", "original", MailAttention::Quiet)?)?;
    settled(&inbox, &message("newer", "original", MailAttention::Quiet)?)?;
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?), Ok(armed(2, "origin")?)],
            pages: vec![
                Ok(RemoteMailPage::final_page(vec![])),
                Ok(RemoteMailPage::final_page(vec![message(
                    "old",
                    "original",
                    MailAttention::Wake,
                )?])),
            ],
            acks: vec![Ok(ack_receipt(&old))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    live(&mut owner).await?;
    let session = "ses_notification".parse()?;
    let step = grapt_types::StepNumber::new(1).ok_or("step")?;
    // A completed pass over quiet rows raises the mark above both of them.
    let first = owner.prepare_notifications(&session, step, notification_binding()?)?;
    assert!(first.batch.is_none() && !first.pending);
    assert!(owner.observe_change(ChangeSignal::new(SubscriptionGeneration::FIRST)));
    let slice = live(&mut owner).await?;
    assert_eq!(slice.delivered_from, Some(old.local_admission()));
    let woken = owner
        .prepare_notifications(&session, step, notification_binding()?)?
        .batch
        .ok_or("the re-reported wake row is presented")?;
    assert!(woken.content().contains("id: \"old\""));
    Ok(())
}

#[tokio::test]
async fn a_recovered_delivery_of_an_older_row_reaches_the_wake_scan() -> ResultTest {
    let inbox = new_inbox()?;
    let old = inbox
        .lock()
        .map_err(|_| "inbox")?
        .store_mut()
        .admit_remote_mailbox(
            &scope()?,
            &input(&scope()?, &message("old", "original", MailAttention::Wake)?),
        )?
        .ack;
    settled(&inbox, &message("newer", "original", MailAttention::Quiet)?)?;
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            acks: vec![Ok(ack_receipt(&old))],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![]))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    // Recovery settles the older row's delivery; the product's wake scan,
    // which keeps its own mark, learns where to read from.
    let (_send, cancel) = cancel();
    let slice = owner.advance_slice(&cancel).await?;
    assert_eq!(slice.delivered_from, Some(old.local_admission()));
    assert!(slice.rescan_notifications);
    Ok(())
}
