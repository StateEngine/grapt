//! A delivery-contract refusal shows stopped at once and heals by a slow retry.
use super::*;
use std::sync::atomic::AtomicUsize;

fn recovered_wake(application: &ServerApplication, session: &SessionId) -> bool {
    let inbox = application.lock_inbox();
    let Some(input) = inbox
        .pending_native_mail_wake(session)
        .unwrap_or_else(|error| panic!("native injection unavailable: {error}"))
    else {
        return false;
    };
    assert_eq!(
        inbox
            .pending_inputs(session)
            .unwrap_or_else(|error| panic!("pending input unavailable: {error}"))
            .len(),
        1,
        "slow retry duplicated canonical wake input"
    );
    assert_settled_wake_coverage(&inbox, &input)
        .unwrap_or_else(|error| panic!("settled wake coverage: {error}"));
    true
}

#[tokio::test]
async fn delivery_contract_refusal_shows_stopped_then_heals_by_slow_retry() -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let attempts = Arc::new(AtomicUsize::new(0));
    let stop = async {
        let mut stopped = false;
        loop {
            stopped |= owner
                .context
                .try_lock()
                .is_ok_and(|context| context.attention == MailAttentionInfo::Stopped);
            if recovered_wake(&application, &session) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(stopped, "a delivery-contract refusal never showed stopped");
        // Checked before the driver shuts down, which closes mail as unavailable.
        assert!(
            matches!(
                owner.context.lock().await.attention,
                MailAttentionInfo::Ready { .. }
            ),
            "slow retry did not clear stopped attention"
        );
    };
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(
            fake::serve_refusing(
                listener,
                fake::Refusals {
                    operation: "acknowledge_delivery",
                    cause: "AcknowledgementDigestMismatch",
                    remaining: 1,
                    skip: 0,
                    attempts: attempts.clone()
                }
            ),
            driver.run_until(stop)
        )
    })
    .await?;
    let calls = transport?;
    run?;
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "the refused acknowledgement was never retried: {calls:?}"
    );
    Ok(())
}

#[tokio::test]
async fn prompt_origin_delivery_refusal_stays_stopped_through_cancel_then_heals() -> TestResult {
    let (_root, application, mut driver, session) = fixture(Some(r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#)).await?;
    let native = application.native_mail.as_ref().ok_or("native owner")?;
    let owner = native.snapshot()?.first().ok_or("owner")?.1.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let attempts = Arc::new(AtomicUsize::new(0));
    let stop = async {
        loop {
            if recovered_wake(&application, &session) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            matches!(
                owner.context.lock().await.attention,
                MailAttentionInfo::Ready { .. }
            ),
            "slow retry did not clear a prompt-origin stop"
        );
    };
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(
            fake::serve_refusing(
                listener,
                fake::Refusals {
                    operation: "acknowledge_delivery",
                    cause: "AcknowledgementDigestMismatch",
                    remaining: 1,
                    skip: 0,
                    attempts: attempts.clone()
                }
            ),
            async {
                // The service settles the refusal before preparation observes it.
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while attempts.load(Ordering::SeqCst) == 0 {
                        native.drive_one(session.clone(), owner.clone()).await;
                    }
                })
                .await
                .map_err(|error| error.to_string())?;
                let mut preparation = native.plan(&session)?;
                preparation
                    .refresh(StepNumber::new(1).ok_or("step")?)
                    .await
                    .map_err(|e| e.to_string())?;
                preparation.finish(false).await?;
                assert_eq!(
                    owner.context.lock().await.attention,
                    MailAttentionInfo::Stopped
                );
                assert!(owner
                    .recovery
                    .lock()
                    .map_err(|_| "retry custody")?
                    .waiting());
                // A prompt cancelled during the slow wait keeps the stop visible.
                let mut cancelled = native.plan(&session)?;
                cancelled
                    .refresh(StepNumber::new(2).ok_or("step")?)
                    .await
                    .map_err(|e| e.to_string())?;
                cancelled.finish(true).await?;
                assert_eq!(
                    owner.context.lock().await.attention,
                    MailAttentionInfo::Stopped
                );
                driver.run_until(stop).await
            }
        )
    })
    .await?;
    let calls = transport?;
    run?;
    assert!(
        attempts.load(Ordering::SeqCst) >= 2,
        "the prompt-origin refusal was never retried: {calls:?}"
    );
    Ok(())
}
