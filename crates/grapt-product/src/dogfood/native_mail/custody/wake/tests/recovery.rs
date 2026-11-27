//! Exercise the real idle driver and HTTP decoder, without a provider or live seat.
use super::*;
use std::sync::atomic::AtomicUsize;

#[tokio::test]
async fn refused_subscriptions_retry_the_same_binding_and_wake_idle_mail() -> TestResult {
    recovered("subscribe_and_snapshot", "SubscriptionRefused", false).await
}

#[tokio::test]
async fn first_subscribe_deadline_delivers_later_mail_on_the_same_session() -> TestResult {
    recovered("subscribe_and_snapshot", "Deadline", false).await
}

#[tokio::test]
async fn confirmed_subscription_refusal_retries_without_replacing_its_binding() -> TestResult {
    recovered(
        "subscribe_and_snapshot",
        "SubscriptionRefusedNoEffect",
        false,
    )
    .await
}

#[tokio::test]
async fn resumed_mail_owner_recovers_the_refusal_without_another_prompt() -> TestResult {
    recovered("subscribe_and_snapshot", "SubscriptionRefused", true).await
}

#[tokio::test]
async fn unknown_delivery_retries_the_original_ack_and_injects_only_once() -> TestResult {
    recovered("acknowledge_delivery", "MessageNeverEnumerated", false).await
}

async fn recovered(operation: &'static str, cause: &'static str, resumed: bool) -> TestResult {
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
    let refusal_count = if cause == "Deadline" { 1 } else { 2 };
    let stop = std::future::poll_fn(|_| {
        match application.lock_inbox().pending_native_mail_wake(&session) {
            Ok(Some(_)) => Poll::Ready(()),
            Ok(None) => Poll::Pending,
            Err(error) => panic!("native injection evidence unavailable: {error}"),
        }
    });
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(
            fake::serve_refusing(
                listener,
                fake::Refusals {
                    operation,
                    cause,
                    remaining: refusal_count,
                    skip: 0,
                    attempts: attempts.clone()
                }
            ),
            async {
                if resumed {
                    native.drive_one(session.clone(), owner.clone()).await;
                    let mut preparation = native.plan(&session)?;
                    preparation
                        .refresh(StepNumber::new(1).ok_or("step")?)
                        .await
                        .map_err(|e| e.to_string())?;
                    preparation.finish(false).await?;
                    assert_eq!(attempts.load(Ordering::SeqCst), 1);
                    // Reattaching a resumed session must reuse the failed owner's
                    // runtime and pending backoff, rather than opening a takeover.
                    let reattached = native.plan(&session)?;
                    assert!(Arc::ptr_eq(&reattached.owner, &owner));
                    assert!(owner
                        .recovery
                        .lock()
                        .map_err(|_| "retry custody")?
                        .waiting());
                    drop(reattached);
                }
                driver.run_until(stop).await
            }
        )
    })
    .await?;
    let calls = transport?;
    run?;
    assert_eq!(
        calls.iter().filter(|call| call.as_str() == "open").count(),
        1,
        "refusal replaced the binding"
    );
    assert_eq!(calls.last().map(String::as_str), Some("close"));
    let inbox = application.lock_inbox();
    let pending = inbox.pending_inputs(&session)?;
    assert_eq!(pending.len(), 1, "retries duplicated canonical wake input");
    assert_settled_wake_coverage(&inbox, &pending[0])?;
    assert!(!pending[0].text.contains("fixture-secret"));
    assert!(
        attempts.load(Ordering::SeqCst) > refusal_count,
        "fixture refusal was never exercised"
    );
    eprintln!("native-mail fixture delivery session={session} first_failure={cause} subscribe_attempts={} open_count=1 pending_inputs=1", attempts.load(Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn persistent_subscription_refusal_keeps_attention_offline_without_waking_mail() -> TestResult
{
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
    let stop = std::future::poll_fn(|_| {
        if attempts.load(Ordering::SeqCst) >= 3
            && owner
                .context
                .try_lock()
                .is_ok_and(|context| context.attention == MailAttentionInfo::Retrying)
        {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let (transport, run) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            fake::serve_refusing(
                listener,
                fake::Refusals {
                    operation: "subscribe_and_snapshot",
                    cause: "SubscriptionRefused",
                    remaining: 64,
                    skip: 0,
                    attempts: attempts.clone()
                }
            ),
            driver.run_until(stop)
        )
    })
    .await?;
    transport?;
    run?;
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    let frame = serde_json::to_value(native.hydrate().first().ok_or("offline attention")?)?;
    assert_eq!(frame["data"]["status"]["type"], "unavailable");
    assert!(application
        .lock_inbox()
        .pending_native_mail_wake(&session)?
        .is_none());
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn a_queued_wake_waits_for_exact_recovery_of_an_already_open_subscription() -> TestResult {
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
    let recovered = Arc::new(AtomicBool::new(false));
    let witness = async {
        crate::dogfood::native_mail::custody::tests::drive_to_ready(native, &session).await?;
        while application
            .lock_inbox()
            .pending_native_mail_wake(&session)?
            .is_none()
        {
            native.drive_one(session.clone(), owner.clone()).await;
            native.enqueue_ready();
        }
        let mut plan = native.plan(&session)?;
        plan.refresh(StepNumber::new(1).ok_or("step")?)
            .await
            .map_err(|error| error.to_string())?;
        plan.finish(false).await?;
        native.enqueue_ready();
        let input = application
            .lock_inbox()
            .pending_native_mail_wake(&session)?
            .ok_or("original wake input")?;
        assert!(owner.wake_scheduled.load(Ordering::SeqCst));
        // The service owns the rescan; preparation reads its settled failure.
        owner.cancel_wait()?;
        owner
            .context
            .lock()
            .await
            .runtime
            .as_mut()
            .ok_or("live runtime")?
            .request_snapshot();
        native.drive_one(session.clone(), owner.clone()).await;
        plan.refresh(StepNumber::new(2).ok_or("step")?)
            .await
            .map_err(|error| error.to_string())?;
        plan.finish(false).await?;
        let held = owner.context.lock().await;
        assert!(
            held.subscription_open,
            "fixture never established a live subscription"
        );
        assert!(held
            .runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.is_live()));
        let probe = TransportWakeFence {
            owner: owner.clone(),
            revision: owner.revision()?,
            capability: held
                .binding
                .as_ref()
                .ok_or("retained binding")?
                .capability
                .clone(),
            input: input.input.clone(),
            notify: false,
        };
        drop(held);
        assert_eq!(
            probe.validate_after_wait().await,
            Err(NativeWakeRefusal::Connection),
            "queued work crossed a known failed mailbox observation"
        );
        *owner.wake_blocked.lock().map_err(|_| "wake block")? = Some(BlockedNativeWake {
            input: input.input.clone(),
            revision: owner.revision()?,
            retry_after_connection: true,
        });
        let stop = std::future::poll_fn(|_| {
            let live = owner.context.try_lock().is_ok_and(|context| {
                context
                    .runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.is_live())
            });
            let unblocked = owner
                .wake_blocked
                .lock()
                .is_ok_and(|blocked| blocked.is_none());
            if live && unblocked {
                recovered.store(true, Ordering::SeqCst);
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        });
        driver.run_until(stop).await?;
        assert!(recovered.load(Ordering::SeqCst));
        let pending = application.lock_inbox().pending_inputs(&session)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].input, input.input,
            "recovery replaced the original canonical input"
        );
        assert!(
            crate::dogfood::test_provider::server(&application)?
                .request()?
                .is_none(),
            "offline queued input reached provider effects"
        );
        // Keep the probe alive until the real driver has settled its own fence;
        // dropping a test probe must not release the actual queued job early.
        drop(probe);
        Ok::<(), Box<dyn std::error::Error>>(())
    };
    let (transport, result) = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        tokio::join!(
            fake::serve_refusing(
                listener,
                fake::Refusals {
                    operation: "subscribe_and_snapshot",
                    cause: "SubscriptionRefused",
                    remaining: 2,
                    skip: 1,
                    attempts: attempts.clone()
                }
            ),
            witness
        )
    })
    .await?;
    let calls = transport?;
    result?;
    assert_eq!(
        calls.iter().filter(|call| call.as_str() == "open").count(),
        1
    );
    assert_eq!(calls.last().map(String::as_str), Some("close"));
    assert!(attempts.load(Ordering::SeqCst) >= 4);
    Ok(())
}
