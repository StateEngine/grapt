use super::*;
use crate::dogfood::native_mail::tests::{context, http::serve};
use grapt_api::u3a::{ServerConnectedEvent, SseLimitValues, SseLimits};
use grapt_types::Timestamp;
use tokio::{net::TcpListener, sync::oneshot};
type TestResult = Result<(), Box<dyn std::error::Error>>;
mod convergence;
mod preparation;

#[derive(Debug)]
pub(in crate::dogfood::native_mail) struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1760000000123)
    }
}
fn owners(
    context: &NativeMailContext,
) -> Result<Arc<NativeMailOwners>, Box<dyn std::error::Error>> {
    let limits = SseLimits::try_new(SseLimitValues {
        subscribers: 2,
        pending_events: 16,
        event_bytes: grapt_api::u3a::MAX_SSE_EVENT_BYTES,
        frame_bytes: grapt_api::u3a::MAX_SSE_FRAME_BYTES,
    })?;
    let connected =
        ServerConnectedEvent::new(WireId::parse("evt_native_custody".into()).ok_or("event")?);
    let hub = Arc::new(DogfoodEventHub::try_new(limits, connected)?);
    Ok(NativeMailOwners::new(
        context.inbox.clone(),
        context.custody.clone(),
        context.ids.clone(),
        Arc::new(Clock),
        hub,
        std::path::Path::new("/fixture"),
    ))
}
async fn plan_at(
    owners: &NativeMailOwners,
    context: &NativeMailContext,
    listener: &TcpListener,
) -> Result<NativeMailPlan, Box<dyn std::error::Error>> {
    let plan = owners.plan(&context.session)?;
    plan.owner
        .context
        .lock()
        .await
        .use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    Ok(plan)
}

pub(super) async fn drive_to_ready(owners: &NativeMailOwners, session: &SessionId) -> TestResult {
    let owner = owners
        .snapshot()?
        .into_iter()
        .find(|(id, _)| id == session)
        .ok_or("retained owner")?
        .1;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let ready = async {
                loop {
                    if owner.context.try_lock().is_ok_and(|context| {
                        context.runtime.as_ref().is_some_and(HttpMailbox::is_live)
                    }) {
                        return;
                    }
                    tokio::task::yield_now().await;
                }
            };
            tokio::select! {
                _ = owners.drive_one(session.clone(), owner.clone()) => {},
                () = ready => return,
            }
        }
    })
    .await?;
    Ok(())
}

async fn drive_until_polled(owners: &NativeMailOwners, poll: oneshot::Receiver<()>) -> TestResult {
    tokio::select! {
        _ = owners.drive() => Err("service stopped before advisory wait".into()),
        observed = poll => { observed?; Ok(()) },
    }
}

#[tokio::test]
async fn service_retains_open_binding_after_prompt_abort_and_joins_close() -> TestResult {
    let context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    let owners = owners(&context)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut plan = plan_at(&owners, &context, &listener).await?;
    let (entered_tx, entered) = oneshot::channel();
    let (poll_tx, poll) = oneshot::channel();
    let (server, witness) = tokio::join!(serve(listener, Some(poll_tx)), async {
        drive_until_polled(&owners, poll).await?;
        let task = tokio::spawn(async move {
            let refreshed = plan
                .refresh(StepNumber::new(1).unwrap_or_else(|| panic!("step")))
                .await;
            let _closed = entered_tx.send(refreshed.is_ok());
            // Owns the prepared context lease until the task is aborted.
            pending::<()>().await;
        });
        let result = async {
            assert!(entered.await?, "prompt never prepared its actual binding");
            task.abort();
            assert!(task.await.is_err(), "named abandonment did not happen");
            let held = owners.snapshot()?;
            let context = held.first().ok_or("retained owner")?.1.context.lock().await;
            assert!(
                context.runtime.is_some(),
                "prompt drop lost the remote binding's service custody"
            );
            drop(context);
            Ok::<(), Box<dyn std::error::Error>>(())
        }
        .await;
        let closed = owners.close().await;
        result?;
        closed?;
        Ok::<(), Box<dyn std::error::Error>>(())
    });
    server?;
    witness
}

#[tokio::test]
async fn existing_service_driver_observes_idle_mail_and_drops_poll_before_joined_close(
) -> TestResult {
    let context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    let owners = owners(&context)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    drop(plan_at(&owners, &context, &listener).await?);
    let (poll_tx, poll) = oneshot::channel();
    let (stop_tx, stop) = oneshot::channel();
    let (_mailbox, mut driver) = crate::dogfood::prompt_driver::PromptMailbox::new();
    driver.attach_native_mail(owners.clone());
    let (server, run, witness) = tokio::join!(
        serve(listener, Some(poll_tx)),
        driver.run_until(async {
            let _closed = stop.await;
        }),
        async {
            let result = async {
                poll.await?;
                let frames = owners.hydrate();
                assert_eq!(frames.len(), 1);
                let frame = serde_json::to_value(&frames[0])?;
                assert_eq!(frame["type"], "session.mail.attention");
                assert_eq!(frame["data"]["sessionID"], context.session.as_str());
                assert_eq!(
                    frame["data"]["status"],
                    serde_json::json!({"type":"ready","pending":1,"incomplete":false})
                );
                let bytes = frame.to_string();
                assert!(!bytes.contains("fixture-secret") && !bytes.contains("full-body detail"));
                // Replacing even identical capability bytes invalidates cached
                // readback immediately; source/body status is not authoritative.
                context
                    .custody
                    .lock()
                    .map_err(|_| "custody")?
                    .put(context.session.clone(), "fixture-secret")?;
                owners.invalidate(&context.session);
                let frame = serde_json::to_value(&owners.hydrate()[0])?;
                assert_eq!(frame["data"]["status"]["type"], "unavailable");
                Ok::<(), Box<dyn std::error::Error>>(())
            }
            .await;
            let _closed = stop_tx.send(());
            result
        }
    );
    server?;
    run?;
    witness
}

#[tokio::test]
async fn anonymous_sessions_do_not_spend_connection_capacity_or_invent_zero_attention() -> TestResult
{
    let context = context(None, false)?;
    let owners = owners(&context)?;
    let mut plan = owners.plan(&context.session)?;
    plan.refresh(StepNumber::new(1).ok_or("step")?)
        .await
        .map_err(|e| e.to_string())?;
    assert!(plan.take_batch().map_err(|e| e.to_string())?.is_none());
    plan.shutdown().await?;
    assert!(owners.snapshot()?.is_empty());
    assert!(owners.hydrate().is_empty());
    owners.close().await?;
    Ok(())
}

#[tokio::test]
async fn external_host_bound_session_has_no_legacy_mail_consumer() -> TestResult {
    let context = context(
        Some(
            r#"{"scope":"project","project_id":"secode","agent_id":"fixture-seat","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
        ),
        true,
    )?;
    let standalone = owners(&context)?;
    let external = NativeMailOwners::with_external_host(
        context.inbox.clone(),
        context.custody.clone(),
        context.ids.clone(),
        Arc::new(Clock),
        standalone.hub.clone(),
        std::path::Path::new("/fixture"),
        true,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut plan = plan_at(&external, &context, &listener).await?;
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        plan.refresh(StepNumber::new(1).ok_or("step")?),
    )
    .await?
    .map_err(|error| error.to_string())?;
    assert!(plan
        .decode_warning()
        .map_err(|error| error.to_string())?
        .is_none());
    assert!(plan
        .take_batch()
        .map_err(|error| error.to_string())?
        .is_none());
    plan.finish(true).await?;
    plan.shutdown().await?;
    assert!(external.snapshot()?.is_empty());
    tokio::select! {
        _ = external.drive() => return Err("external host drove the legacy consumer".into()),
        accepted = listener.accept() => {
            accepted?;
            return Err("external host opened a legacy mail connection".into());
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
    }
    external.close().await?;
    Ok(())
}

#[tokio::test]
async fn lost_subscription_restores_snapshot_with_one_admission_and_original_ack() -> TestResult {
    for local_shutdown in [false, true] {
        let context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
        let owners = owners(&context)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        drop(plan_at(&owners, &context, &listener).await?);
        let scope = context
            .current_binding()
            .map_err(|e| e.to_string())?
            .ok_or("binding")?
            .scope;
        let (renewed_tx, renewed) = oneshot::channel();
        let (stop_tx, stop) = oneshot::channel();
        let (_mailbox, mut driver) = crate::dogfood::prompt_driver::PromptMailbox::new();
        driver.attach_native_mail(owners.clone());
        let (server, run, witness) = tokio::join!(
            crate::dogfood::native_mail::tests::http::serve_reconnecting(
                listener,
                renewed_tx,
                local_shutdown
            ),
            driver.run_until(async {
                let _closed = stop.await;
            }),
            async {
                let result = async {
                    tokio::time::timeout(std::time::Duration::from_secs(15), renewed).await??;
                    let frame = serde_json::to_value(&owners.hydrate()[0])?;
                    assert_eq!(
                        frame["data"]["status"],
                        serde_json::json!({"type":"ready","pending":1,"incomplete":false})
                    );
                    let held = context.inbox.lock().map_err(|_| "inbox")?;
                    let page = held.store().read_remote_mailbox(
                        &scope,
                        grapt_types::MailboxCursor::START,
                        64,
                        1048576,
                    )?;
                    assert_eq!(page.records.len(), 2, "replay must not admit another copy");
                    assert!(page
                        .records
                        .iter()
                        .all(|r| !r.delivery_pending() && !r.consumed()));
                    assert!(page
                        .records
                        .iter()
                        .all(|r| r.ack().source_binding().as_str() == "bind-native"));
                    Ok::<(), Box<dyn std::error::Error>>(())
                }
                .await;
                let _closed = stop_tx.send(());
                result
            }
        );
        server?;
        run?;
        witness?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_close_failure_cannot_be_reported_as_a_confirmed_service_close() -> TestResult
{
    let context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    let owners = owners(&context)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let mut plan = plan_at(&owners, &context, &listener).await?;
    let (poll_tx, poll) = oneshot::channel();
    let (server, client) = tokio::join!(
        crate::dogfood::native_mail::tests::http::serve_refused_close(listener, poll_tx),
        async {
            drive_until_polled(&owners, poll).await?;
            // A stalled earlier stage has no lease and cannot close the live
            // service transport, even while the service context is busy.
            let owner = plan.owner.clone();
            let held = owner.context.lock().await;
            tokio::time::timeout(std::time::Duration::from_millis(50), plan.finish(true)).await??;
            assert!(held.has_live_runtime());
            assert!(held.blocked_revision.is_none());
            drop(held);
            plan.refresh(StepNumber::new(1).ok_or("step")?)
                .await
                .map_err(|e| e.to_string())?;
            assert!(
                plan.finish(true).await.is_err(),
                "source's out-of-bound close was accepted"
            );
            // Retirement may release capacity, but cannot erase failed cleanup.
            owners.retire(&context.session);
            assert!(
                owners.close().await.is_err(),
                "retirement lost the unconfirmed close result after cancellation"
            );
            Ok::<(), Box<dyn std::error::Error>>(())
        }
    );
    server?;
    client
}

#[tokio::test]
async fn unrelated_drive_cancellation_retains_the_owned_poll_until_service_close() -> TestResult {
    let context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    let owners = owners(&context)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    drop(plan_at(&owners, &context, &listener).await?);
    let (entered_tx, entered) = oneshot::channel();
    let (dropped_tx, dropped) = oneshot::channel();
    let (retained_tx, retained) = oneshot::channel();
    let (server, client) = tokio::join!(
        crate::dogfood::native_mail::tests::http::serve_retained_poll(
            listener,
            entered_tx,
            dropped,
            retained_tx
        ),
        async {
            let mut drive = Box::pin(owners.drive());
            tokio::select! {
                result = entered => result?,
                _ = &mut drive => return Err("idle drive ended before its owned poll".into()),
            }
            drop(drive);
            let mut preparation = owners.plan(&context.session)?;
            preparation
                .refresh(StepNumber::new(1).ok_or("step")?)
                .await
                .map_err(|error| error.to_string())?;
            preparation.finish(false).await?;
            dropped_tx.send(()).map_err(|_| "drop witness gone")?;
            let result = retained.await;
            let closed = owners.close().await;
            result?;
            closed?;
            Ok::<(), Box<dyn std::error::Error>>(())
        }
    );
    server?;
    client
}

#[tokio::test]
async fn received_signal_survives_cancelled_drive_while_context_is_busy() -> TestResult {
    let context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    let owners = owners(&context)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let plan = plan_at(&owners, &context, &listener).await?;
    let owner = plan.owner.clone();
    drop(plan);
    let (entered_tx, entered) = oneshot::channel();
    let (release_tx, release) = oneshot::channel();
    let (rescanned_tx, rescanned) = oneshot::channel();
    let (server, client) = tokio::join!(
        crate::dogfood::native_mail::tests::http::serve_received_signal(
            listener,
            entered_tx,
            release,
            rescanned_tx
        ),
        async {
            let mut drive = Box::pin(owners.drive());
            tokio::select! {
                result = entered => result?,
                _ = &mut drive => return Err("idle drive ended before its owned poll".into()),
            }
            let held = owner.context.lock().await;
            release_tx.send(()).map_err(|_| "signal witness gone")?;
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                poll_fn(|cx| {
                    assert!(
                        drive.as_mut().poll(cx).is_pending(),
                        "busy context must retain the received signal"
                    );
                    match &*owner.wait.lock().unwrap_or_else(|_| panic!("wait custody")) {
                        Some(MailWait::Received(Ok(_))) => Poll::Ready(()),
                        Some(MailWait::Received(Err(_))) => {
                            panic!("fixture did not deliver a valid typed signal")
                        }
                        _ => Poll::Pending,
                    }
                }),
            )
            .await?;
            drop(drive);
            drop(held);
            owners.drive().await?;
            let result = {
                let held = owner.context.lock().await;
                assert!(
                    held.runtime.as_ref().is_some_and(|r| !r.is_live()),
                    "retained signal did not request its snapshot rescan"
                );
                Ok::<(), Box<dyn std::error::Error>>(())
            };
            let mut resumed = Box::pin(owners.drive());
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::select! {
                    result = rescanned => result.map_err(|_| "rescan witness gone"),
                    _ = &mut resumed => Err("drive ended before the retained signal's rescan"),
                }
            })
            .await??;
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut resumed)
                    .await
                    .is_err(),
                "one retained signal did not settle into an owned idle poll"
            );
            drop(resumed);
            {
                let held = owner.context.lock().await;
                assert!(held.runtime.as_ref().is_some_and(HttpMailbox::is_live));
                assert!(
                    !held.wake.pending(),
                    "the completed rescan left duplicate work"
                );
                let scope = held
                    .current_binding()
                    .map_err(|error| error.to_string())?
                    .ok_or("binding")?
                    .scope;
                assert_rescan_preserves_mail(&context, &scope)?;
            }
            let closed = owners.close().await;
            result?;
            closed?;
            Ok::<(), Box<dyn std::error::Error>>(())
        }
    );
    server?;
    client
}

fn assert_rescan_preserves_mail(
    context: &NativeMailContext,
    scope: &grapt_store::RemoteMailboxScope,
) -> TestResult {
    let inbox = context.inbox.lock().map_err(|_| "inbox")?;
    let page =
        inbox
            .store()
            .read_remote_mailbox(scope, grapt_types::MailboxCursor::START, 64, 1048576)?;
    assert_eq!(page.records.len(), 2, "rescan duplicated admitted mail");
    assert_eq!(
        page.records.iter().filter(|row| row.actionable()).count(),
        1
    );
    assert!(page
        .records
        .iter()
        .all(|row| !row.delivery_pending() && !row.consumed()));
    Ok(())
}

#[tokio::test]
async fn mail_status_returns_cached_stale_state_while_the_source_lease_is_busy() -> TestResult {
    let context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    let owners = owners(&context)?;
    let plan = owners.plan(&context.session)?;
    let owner = plan.owner.clone();
    drop(plan);
    let mut lease = owner.context.clone().lock_owned().await;
    lease.verified_revision = Some(owner.revision()?);
    lease.attention = MailAttentionInfo::Ready {
        pending: grapt_api::u3a::MailPendingCount::new(3).ok_or("pending")?,
        incomplete: false,
        decode_failures: Default::default(),
    };
    owner.cache_attention(&lease);
    let reader = owners.clone();
    let task = tokio::task::spawn_blocking(move || reader.hydrate());
    let frames = tokio::time::timeout(std::time::Duration::from_millis(250), task).await??;
    let frame = serde_json::to_value(frames.first().ok_or("mail status")?)?;
    assert_eq!(frame["data"]["status"]["pending"], 3);
    assert_eq!(frame["data"]["staleSince"], 1760000000123_f64);
    owners.invalidate(&context.session);
    let frame = serde_json::to_value(owners.hydrate().first().ok_or("invalidated status")?)?;
    assert_eq!(frame["data"]["status"]["type"], "unavailable");
    assert!(frame["data"]["status"].get("pending").is_none());
    drop(lease);
    owners.close().await?;
    Ok(())
}

#[tokio::test]
async fn a_degraded_step_takes_no_mail_and_still_closes_like_a_cancelled_one() -> TestResult {
    let mut context = context(Some(r#"{"project_id":"secode","agent_id":"coder"}"#), true)?;
    context.attention = MailAttentionInfo::Ready {
        pending: grapt_api::u3a::MailPendingCount::new(1).ok_or("pending")?,
        incomplete: false,
        decode_failures: Default::default(),
    };
    let mut plan = NativeMailPlan::private(context);
    plan.degrade(E::Transport);
    assert!(matches!(plan.decode_warning(), Ok(None)));
    assert!(matches!(plan.take_batch(), Err(E::Transport)));
    plan.finish(false).await?;
    // The failed preparation's transport is closed as a cancelled step's is.
    let context = plan.owner.context.clone().lock_owned().await;
    assert_eq!(context.attention, MailAttentionInfo::Unavailable);
    Ok(())
}
