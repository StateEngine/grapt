//! Public bounded re-observation and later healthy-mail delivery.
use super::*;

async fn review_public_reobserve(snapshot_request: bool) -> ResultTest {
    let inbox = new_inbox()?;
    let first = gap(1)?;
    let first_cursor = first.cursor().clone();
    let later = message("later", "bind-1", MailAttention::Wake)?;
    let last = later.source_cursor().clone();
    let ack = predicted_ack(&later)?;
    let mut owner = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?), Ok(subscribed(Some(first_cursor))?)],
            pages: vec![
                Ok(page(vec![], vec![first], false)?),
                Ok(page(vec![later], vec![], false)?),
            ],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(1, 64),
    )?;
    let (_cancel, signal) = watch::channel(None);
    for _ in 0..4 {
        if owner.advance_slice(&signal).await?.live {
            break;
        }
    }
    assert!(owner.is_live(), "the initial bounded prefix did not finish");
    if snapshot_request {
        owner.request_snapshot();
    } else {
        assert!(owner.observe_change(ChangeSignal::new(SubscriptionGeneration::FIRST)));
    }
    let observed = owner.advance_slice(&signal).await?;
    assert!(
        observed.live,
        "one head read restarted the already verified local walk"
    );
    assert_eq!(
        observed.pages, 1,
        "head observation consumed extra local recovery pages"
    );
    assert_eq!(owner.validated_checkpoint()?, Some(&last));
    assert_eq!(owner.recorded_integrity_gaps(), 1);
    let scan = owner.prepare_notifications(
        &"ses_public_reobserve".parse()?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        notification_binding()?,
    )?;
    assert_eq!(
        scan.batch
            .ok_or("later healthy mail was not deliverable")?
            .count(),
        1
    );
    // Once live, repeated service advances without an advisory do no scan work.
    for _ in 0..4 {
        let quiet = owner.advance_slice(&signal).await?;
        assert!(quiet.live);
        assert_eq!((quiet.pages, quiet.rows, quiet.charged_bytes), (0, 0, 0));
    }
    Ok(())
}

#[tokio::test]
async fn review_public_advisory_observes_only_the_head_and_delivers_later_mail() -> ResultTest {
    review_public_reobserve(false).await
}

#[tokio::test]
async fn review_public_snapshot_request_observes_only_the_head_and_delivers_later_mail(
) -> ResultTest {
    review_public_reobserve(true).await
}
