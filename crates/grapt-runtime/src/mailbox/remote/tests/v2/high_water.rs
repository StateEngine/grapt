//! The v2 twins of the high-water witnesses: a recorded page that re-reports
//! a quiet row as wake, and a progress recovery of an older actionable row.
use super::*;

#[tokio::test]
async fn a_v2_page_reporting_a_quiet_row_again_as_wake_below_the_mark_is_presented() -> ResultTest {
    let inbox = new_inbox()?;
    let old = message("old", "bind-1", MailAttention::Quiet)?;
    let newer = message("newer", "bind-1", MailAttention::Quiet)?;
    let newer_cursor = newer.source_cursor().clone();
    let old_ack = predicted_ack(&old)?;
    let newer_ack = predicted_ack(&newer)?;
    let mut owner = owner(
        inbox,
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?), Ok(subscribed(Some(newer_cursor))?)],
            pages: vec![
                Ok(page(vec![old, newer], vec![], false)?),
                Ok(page(
                    vec![message("old", "bind-1", MailAttention::Wake)?],
                    vec![],
                    false,
                )?),
            ],
            acks: vec![
                Ok(ack_receipt(&old_ack)),
                Ok(ack_receipt(&newer_ack)),
                Ok(ack_receipt(&old_ack)),
            ],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_cancel, signal) = watch::channel(None);
    assert!(owner.advance_slice(&signal).await?.live);
    let session = "ses_notification".parse()?;
    let step = grapt_types::StepNumber::new(1).ok_or("step")?;
    // A completed pass over the quiet rows raises the mark above both.
    let first = owner.prepare_notifications(&session, step, notification_binding()?)?;
    assert!(first.batch.is_none() && !first.pending);
    assert!(owner.observe_change(ChangeSignal::new(SubscriptionGeneration::FIRST)));
    let slice = owner.advance_slice(&signal).await?;
    assert!(slice.live);
    assert_eq!(slice.delivered_from, Some(old_ack.local_admission()));
    let woken = owner
        .prepare_notifications(&session, step, notification_binding()?)?
        .batch
        .ok_or("the re-reported wake row is presented")?;
    assert!(woken.content().contains("id: \"old\""));
    Ok(())
}

#[tokio::test]
async fn a_v2_progress_recovery_of_an_older_row_reaches_the_wake_scan() -> ResultTest {
    let inbox = new_inbox()?;
    let old = message("old", "bind-1", MailAttention::Wake)?;
    let last = old.source_cursor().clone();
    let ack = predicted_ack(&old)?;
    let mut first = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(page(vec![old], vec![], false)?)],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    // The page commits but its delivery is never witnessed.
    first.port.lose_next_ack_response();
    let (_cancel, signal) = watch::channel(None);
    assert!(first.advance_slice(&signal).await.is_err());
    drop(first);
    let mut resumed = owner(
        inbox,
        TransportScript {
            open: vec![Ok(bound("bind-2")?)],
            subscribe: vec![Ok(subscribed(Some(last))?)],
            pages: vec![Ok(page(vec![], vec![], false)?)],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    // Progress recovery settles the older row's delivery; the product's wake
    // scan, which keeps its own mark, learns where to read from.
    let slice = resumed.advance_slice(&signal).await?;
    assert!(slice.live);
    assert_eq!(slice.delivered_from, Some(ack.local_admission()));
    Ok(())
}
