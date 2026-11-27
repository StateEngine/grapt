use super::*;
use crate::dogfood::native_mail::tests::context;
use std::future::Future;
use std::task::Poll;
mod service;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fence() -> Result<TransportWakeFence, Box<dyn std::error::Error>> {
    let mut context = context(
        Some(
            r#"{"scope":"project","project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
        ),
        true,
    )?;
    context.binding = context
        .current_binding()
        .map_err(|error| error.to_string())?;
    let capability = context
        .binding
        .as_ref()
        .ok_or("binding")?
        .capability
        .clone();
    let owner = Owner::new(context, Arc::new(Notify::new()));
    owner.wake_scheduled.store(true, Ordering::SeqCst);
    Ok(TransportWakeFence {
        revision: owner.revision()?,
        owner,
        capability,
        input: "msg_native_fence".parse()?,
        notify: true,
    })
}

#[tokio::test]
async fn seat03_identical_capability_bytes_still_revoke_a_frozen_wake() -> TestResult {
    let fence = fence()?;
    // These authority-only fixtures have no connected runtime. That absence
    // must be unavailable even when the capability itself is current.
    assert_eq!(
        fence.validate_after_wait().await,
        Err(NativeWakeRefusal::Connection)
    );
    {
        let context = fence.owner.context.lock().await;
        context
            .custody
            .lock()
            .map_err(|_| "custody")?
            .put(context.session.clone(), "fixture-secret")?;
    }
    assert_eq!(
        fence.validate_after_wait().await,
        Err(NativeWakeRefusal::Binding),
        "new capability custody authorized an old wake"
    );
    Ok(())
}

#[tokio::test]
async fn seat03_a_busy_wait_rechecks_the_capability_before_any_execution() -> TestResult {
    let fence = fence()?;
    let session = fence.owner.context.lock().await.session.clone();
    let slots = crate::dogfood::execution::WakeSlots::default();
    let (started, _accepted) = crate::dogfood::prompt_driver::Started::channel();
    let (_cancel, cancelled) = tokio::sync::watch::channel(false);
    let active = slots
        .reserve(&session)?
        .enter(&started, cancelled.clone())
        .await?
        .ok_or("active custody")?;
    let reservation = slots.reserve(&session)?;
    let mut queued = Box::pin(reservation.enter(&started, cancelled));
    std::future::poll_fn(|cx| {
        assert!(
            queued.as_mut().poll(cx).is_pending(),
            "native input bypassed the active Session"
        );
        Poll::Ready(())
    })
    .await;
    {
        let context = fence.owner.context.lock().await;
        context
            .custody
            .lock()
            .map_err(|_| "custody")?
            .put(context.session.clone(), "replacement-secret")?;
    }
    drop(active);
    let _acquired = queued.await?.ok_or("queued custody")?;
    assert_eq!(
        fence.validate_after_wait().await,
        Err(NativeWakeRefusal::Binding),
        "queued native input ran after capability replacement"
    );
    Ok(())
}

#[tokio::test]
async fn seat03_rebinding_revokes_even_an_unchanged_capability() -> TestResult {
    let fence = fence()?;
    *fence.owner.revision.lock().map_err(|_| "revision")? = Arc::new(());
    assert_eq!(
        fence.validate_after_wait().await,
        Err(NativeWakeRefusal::Binding),
        "old revision retained native wake authority"
    );
    Ok(())
}

#[tokio::test]
async fn seat03_abandonment_releases_the_single_queued_wake_slot() -> TestResult {
    let fence = fence()?;
    let owner = fence.owner.clone();
    assert!(owner.wake_scheduled.load(Ordering::SeqCst));
    drop(fence);
    assert!(
        !owner.wake_scheduled.load(Ordering::SeqCst),
        "abandoned native wake retained its slot"
    );
    tokio::time::timeout(std::time::Duration::from_secs(1), owner.changed.notified()).await?;
    Ok(())
}

#[tokio::test]
async fn seat03_a_known_remote_refusal_blocks_a_previously_admitted_wake() -> TestResult {
    let fence = fence()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    fence
        .owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let (server, witness) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(service::fake::serve(listener, None, 2), async {
            {
                let mut context = fence.owner.context.lock().await;
                context
                    .refresh(StepNumber::new(1).ok_or("step")?)
                    .await
                    .map_err(|error| error.to_string())?;
                let binding = context.binding.as_ref().ok_or("binding")?;
                let expectation = MailBindingExpectation::new(
                    InstructionKey::new("stateengine-identity").ok_or("key")?,
                    binding.raw.clone(),
                )?;
                let mut inbox = context.inbox.lock().map_err(|_| "inbox")?;
                let claim = inbox
                    .store()
                    .prepare_remote_mail_wake(
                        &context.session,
                        &binding.scope,
                        expectation,
                        grapt_types::MailboxCursor::START,
                    )?
                    .claim
                    .ok_or("verified native wake claim")?;
                let metadata = claim.metadata();
                inbox.admit_native_mail_wake(
                    &claim,
                    SyntheticAdmitRequest {
                        event_id: &"evt_native_fence_pending".parse()?,
                        session: &context.session,
                        input: &fence.input,
                        text: "Read mail-a and mail-b through the authenticated inbox",
                        description: Some("Native inbox wake"),
                        metadata: Some(&metadata),
                        delivery: DeliveryMode::Queue,
                        resume: true,
                        created_ms: 1,
                    },
                )?;
            }
            // Prove the pre-refusal fence really passes with a live runtime;
            // missing runtime must not supply a false-positive refusal.
            fence.validate_after_wait().await?;
            fence.owner.context.lock().await.blocked_revision = Some(fence.revision.clone());
            assert_eq!(
                fence.validate_after_wait().await,
                Err(NativeWakeRefusal::Connection),
                "known remote revocation did not fence queued work"
            );
            fence.owner.context.lock().await.finish().await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    })
    .await?;
    server?;
    witness
}

#[test]
fn seat03_reconnect_does_not_reauthorize_an_explicitly_cancelled_input() -> TestResult {
    let fence = fence()?;
    *fence
        .owner
        .wake_blocked
        .lock()
        .map_err(|_| "wake custody")? = Some(BlockedNativeWake {
        input: fence.input.clone(),
        revision: fence.revision.clone(),
        retry_after_connection: false,
    });
    fence.owner.connection_restored()?;
    assert!(
        fence
            .owner
            .wake_blocked
            .lock()
            .map_err(|_| "wake custody")?
            .is_some(),
        "reconnect reauthorized cancelled work without a new work signal"
    );
    *fence
        .owner
        .wake_blocked
        .lock()
        .map_err(|_| "wake custody")? = Some(BlockedNativeWake {
        input: fence.input.clone(),
        revision: fence.revision.clone(),
        retry_after_connection: true,
    });
    fence.owner.connection_restored()?;
    assert!(
        fence
            .owner
            .wake_blocked
            .lock()
            .map_err(|_| "wake custody")?
            .is_none(),
        "authenticated reconnect failed to release a connection-only hold"
    );
    Ok(())
}
