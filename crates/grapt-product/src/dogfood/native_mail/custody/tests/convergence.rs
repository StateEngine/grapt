//! Delayed HTTP work belongs to its session across service-driver selections.
use super::*;
use crate::dogfood::native_mail::tests::http::{row, FramingSlots};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug)]
struct SecondIds;
impl IdAuthority for SecondIds {
    fn mint(&self) -> grapt_types::IdToken {
        grapt_types::IdToken::new("0000000000000002").unwrap_or_else(|| panic!("fixture id"))
    }
}

fn second_context(
    first: &NativeMailContext,
    raw: &str,
) -> Result<NativeMailContext, Box<dyn std::error::Error>> {
    let authorities = grapt_types::DeterministicAuthorities::new(&Clock, &SecondIds);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_native_fixture").ok_or("project")?,
        "/fixture",
        "second native session",
    )?;
    let session = {
        let mut inbox = first.inbox.lock().map_err(|_| "inbox")?;
        let session =
            grapt_session::create_session(inbox.store_mut(), &authorities, &seed, &mut [])?.session;
        InstructionService::put_entry(
            inbox.store_mut(),
            &session,
            &InstructionKey::new("stateengine-identity").ok_or("key")?,
            raw,
            1,
        )?;
        session
    };
    first
        .custody
        .lock()
        .map_err(|_| "capability")?
        .put(session.clone(), "fixture-secret")?;
    NativeMailContext::new(
        session,
        first.inbox.clone(),
        first.custody.clone(),
        Arc::new(SecondIds),
        Arc::new(Clock),
    )
    .map_err(|e| e.to_string().into())
}

async fn reply(socket: &mut TcpStream, value: Value) -> TestResult {
    let body = serde_json::to_vec(&json!({"ok":true,"value":value}))?;
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
    socket.write_all(&body).await?;
    socket.shutdown().await?;
    Ok(())
}

struct Delay {
    operation: &'static str,
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
    shutdown: bool,
    cancel_preparation: bool,
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn peer(
    listener: TcpListener,
    version: u8,
    recipient: &'static str,
    delay: Delay,
    poll_entered: oneshot::Sender<()>,
    poll_release: Option<oneshot::Receiver<()>>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let profile: Value = serde_json::from_str(include_str!(
        "../../../../../../../fixtures/stateengine-native/stateengine-mail-profile.json"
    ))?;
    let delayed_operation = delay.operation;
    let shutdown = delay.shutdown;
    let cancel_preparation = delay.cancel_preparation;
    let mut reopen_after_close = cancel_preparation;
    let recovering = delayed_operation == "fetch_snapshot_page" && !shutdown;
    let mut refuse_subscription = recovering;
    let mut delay = Some(delay);
    let mut poll_entered = Some(poll_entered);
    let mut poll_release = poll_release;
    let mut calls = Vec::new();
    let mut framing = FramingSlots::new();
    type PollWork = std::pin::Pin<Box<dyn std::future::Future<Output = TestResult>>>;
    let mut poll: Option<PollWork> = None;
    for _ in 0..16 {
        let (mut socket, incoming) = tokio::select! {
            biased;
            result = async { poll.as_mut().ok_or("missing poll")?.await }, if poll.is_some() => {
                result?;
                poll = None;
                continue;
            }
            result = tokio::time::timeout(Duration::from_secs(3), framing.next(&listener, version)) => result??,
        };
        let incoming = match incoming {
            Ok(value) => value,
            Err(error) if error.to_string() == "request framing" => continue,
            Err(error) => return Err(error),
        };
        let operation = incoming["operation"].as_str().ok_or("operation")?;
        calls.push(operation.to_owned());
        if operation == "subscribe_and_snapshot" && refuse_subscription {
            refuse_subscription = false;
            let body = json!({"ok":false,"failure":{"family":"TransportFailure",
                "tag":"SubscriptionRefused","effect":"none"}})
            .to_string();
            socket.write_all(format!("HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
            socket.shutdown().await?;
            continue;
        }
        if delay
            .as_ref()
            .is_some_and(|delay| delay.operation == operation)
        {
            let delay = delay.take().ok_or("delay")?;
            delay.entered.send(()).map_err(|_| "delay observer gone")?;
            let mut byte = [0];
            tokio::select! {
                biased;
                released = delay.release => released?,
                read = socket.read(&mut byte) => {
                    if cancel_preparation {
                        assert_eq!(read?, 0, "cancelled preparation did not join its HTTP request");
                        continue;
                    }
                    return Err(format!("{recipient} {operation} was cancelled before its delayed response: {} bytes", read?).into());
                }
            }
        }
        if delay.is_none() && operation == delayed_operation && shutdown {
            let mut byte = [0];
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await??,
                0,
                "shutdown did not cancel and join its pending operation"
            );
            continue;
        }
        let value = match operation {
            "open" => {
                json!({"binding_id":"bind-native","protocol":format!("remote-seat-mail-transport/v{version}"),"adapter_version":format!("stateengine-native-mail/{version}"),"projects":{"source":"secode","target":"secode"},"verified_principal":recipient,"local_recipient":recipient,"mapping_version":"exact-seat-id/v1","takeover":{"generation":1,"displaced":null},"limits":profile["limits"],"capabilities":{"atomic_subscribe_and_snapshot":true,"durable_delivery_ack":true,"bulk_dismiss":true,"explicit_project_pair":true,"opaque_authenticated_cursors":true,"advisory_payload_bytes":0,"coalesced_pending_signals":1}})
            }
            "subscribe_and_snapshot" => {
                json!({"subscription_id":"sub-native","generation":1,"snapshot":{"snapshot_id":"snap-native","barrier":"barrier-native","start_after":incoming["args"]["resume"]["from_checkpoint"],"retained_origin":"retained-origin/v1"}})
            }
            "fetch_snapshot_page" => {
                let mut message = row("delayed-mail", "wake");
                message["recipient"] = json!(recipient);
                let resumed = !incoming["args"]["after"].is_null();
                let messages = if resumed { Vec::new() } else { vec![message] };
                let mut page = json!({"messages":messages,"next_after":null});
                if version == 2 {
                    page["refused_rows"] = json!([]);
                    page["last_cursor"] = json!((!resumed).then_some("cursor-delayed-mail"));
                }
                page
            }
            "acknowledge_delivery" => {
                let ack = &incoming["args"]["ack"];
                assert_eq!(ack["recipient"], recipient);
                assert_eq!(ack["source_message"], "delayed-mail");
                json!({"tag":"Recorded","receipt":{"ack_id":ack["ack_id"],"source_message":ack["source_message"],"projects":ack["projects"]},"primary_delivery":{"witnessed":true},"floor_projection":{"state":"unarmed","outcome":"refused","disposition":null,"reason":"secondary-unarmed","outcome_record":{"state":"unreadable"},"retry_required":false,"retry":null}})
            }
            "next_change" => {
                if let Some(entered) = poll_entered.take() {
                    entered.send(()).map_err(|_| "poll observer gone")?;
                }
                if poll.is_some() {
                    return Err("duplicate retained poll".into());
                }
                let release = poll_release.take();
                poll = Some(Box::pin(async move {
                    if let Some(release) = release {
                        release.await?;
                        reply(&mut socket, json!({"generation":1,"token":null})).await?;
                    } else {
                        let mut byte = [0];
                        assert_eq!(
                            tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
                                .await??,
                            0,
                            "shutdown did not join the retained advisory request"
                        );
                    }
                    Ok(())
                }));
                continue;
            }
            "close" => json!({"joined_tasks":0,"ticks_used":1}),
            _ => return Err("unexpected transport operation".into()),
        };
        if operation == "close" {
            if let Some(work) = poll.take() {
                work.await?;
            }
        }
        reply(&mut socket, value).await?;
        if operation == "close" {
            if std::mem::take(&mut reopen_after_close) {
                continue;
            }
            return Ok(calls);
        }
    }
    Err("per-session request bound exceeded".into())
}

macro_rules! delayed_case {
    ($name:ident, $version:literal, $operation:literal, $shutdown:literal) => {
        #[tokio::test]
        async fn $name() -> TestResult {
            delayed_pair($version, $operation, $shutdown, PreparationEnd::Complete).await
        }
    };
}
delayed_case!(delayed_initial_owner_open_v1, 1, "open", false);
delayed_case!(delayed_initial_owner_open_v2, 2, "open", false);
delayed_case!(
    delayed_initial_owner_subscribe_v1,
    1,
    "subscribe_and_snapshot",
    false
);
delayed_case!(
    delayed_initial_owner_subscribe_v2,
    2,
    "subscribe_and_snapshot",
    false
);
delayed_case!(delayed_two_owner_fetch_v1, 1, "fetch_snapshot_page", false);
delayed_case!(delayed_two_owner_fetch_v2, 2, "fetch_snapshot_page", false);
delayed_case!(delayed_two_owner_ack_v1, 1, "acknowledge_delivery", false);
delayed_case!(delayed_two_owner_ack_v2, 2, "acknowledge_delivery", false);
delayed_case!(
    delayed_two_owner_fetch_shutdown_v1,
    1,
    "fetch_snapshot_page",
    true
);
delayed_case!(
    delayed_two_owner_fetch_shutdown_v2,
    2,
    "fetch_snapshot_page",
    true
);
delayed_case!(
    delayed_two_owner_ack_shutdown_v1,
    1,
    "acknowledge_delivery",
    true
);
delayed_case!(
    delayed_two_owner_ack_shutdown_v2,
    2,
    "acknowledge_delivery",
    true
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum PreparationEnd {
    Complete,
    CancelHeld,
    CancelReleased,
}

#[tokio::test]
async fn cancelled_preparation_during_live_delivery_releases_connection_only_hold() -> TestResult {
    delayed_pair(2, "acknowledge_delivery", false, PreparationEnd::CancelHeld).await
}

#[tokio::test]
async fn cancelled_preparation_after_release_preserves_service_delivery() -> TestResult {
    delayed_pair(
        2,
        "acknowledge_delivery",
        false,
        PreparationEnd::CancelReleased,
    )
    .await
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn delayed_pair(
    version: u8,
    delayed_operation: &'static str,
    shutdown: bool,
    preparation_end: PreparationEnd,
) -> TestResult {
    let cancel_preparation = preparation_end == PreparationEnd::CancelHeld;
    let raw = |recipient| {
        json!({"project_id":"secode","agent_id":recipient,
        "mail_transport_protocol":format!("remote-seat-mail-transport/v{version}")})
        .to_string()
    };
    let first = context(Some(&raw("recipient-one")), true)?;
    let second = second_context(&first, &raw("recipient-two"))?;
    assert_ne!(first.session, second.session);
    let owners = owners(&first)?;
    let listener_a = TcpListener::bind("127.0.0.1:0").await?;
    let listener_b = TcpListener::bind("127.0.0.1:0").await?;
    let port_b = NonZeroU16::new(listener_b.local_addr()?.port()).ok_or("port")?;
    drop(plan_at(&owners, &first, &listener_a).await?);
    let (mailbox, mut driver) = crate::dogfood::prompt_driver::PromptMailbox::new();
    driver.attach_native_mail(owners.clone());
    let (entered_a, held_a) = oneshot::channel();
    let (entered_b, held_b) = oneshot::channel();
    let (release_a, response_a) = oneshot::channel();
    let (release_b, response_b) = oneshot::channel();
    let (poll_a, ready_a) = oneshot::channel();
    let (poll_b, ready_b) = oneshot::channel();
    let (signal, signal_received) = oneshot::channel();
    let (stop, stopped) = oneshot::channel();
    let witness = async {
        let result = async {
            held_a.await?;
            let admitted = owners.plan(&second.session)?;
            admitted.owner.context.lock().await.use_test_port(port_b);
            drop(admitted);
            held_b.await?;
            let second_owner = owners
                .snapshot()?
                .into_iter()
                .find(|(session, _)| session == &second.session)
                .ok_or("second owner")?
                .1;
            // A retained transport operation must not spend the prompt budget.
            use crate::dogfood::production::{tests::projection_targets, ProductPlan};
            use grapt_provider::{PromptCacheKey, ProtocolFamily};
            use grapt_runtime::{AttemptPlan, HistoryGeneration};
            let step = StepNumber::new(1).ok_or("step")?;
            let targets = projection_targets(ProtocolFamily::OpenAiResponses)?;
            let prompt = owners.plan(&second.session)?;
            let owner = prompt.owner.clone();
            let mut preparation = ProductPlan::transient(
                ProtocolFamily::OpenAiResponses,
                "current task".into(),
                PromptCacheKey::for_session(&second.session)?,
                targets.issue()?,
                targets,
                Arc::new(crate::dogfood::mcp::PublishedTools::default()),
                Vec::new(),
                step,
            )
            .with_mail_owner(prompt);
            tokio::time::timeout(
                Duration::from_millis(100),
                preparation.refresh_context(step, HistoryGeneration::new(1)),
            )
            .await
            .map_err(|_| "held mail exhausted the prompt preparation budget")?
            .map_err(|error| format!("{error:?}"))?;
            let prepared = preparation
                .plan(step, 1, HistoryGeneration::new(1))
                .map_err(|error| error.detail)?;
            assert!(
                prepared.mail.is_none(),
                "unfinished mail entered the provider plan"
            );
            if cancel_preparation {
                // The prepared plan still owns its lease. Cancellation joins
                // the held request before service convergence can reconnect.
                *owner.wake_blocked.lock().map_err(|_| "wake custody")? = Some(BlockedNativeWake {
                    input: "msg_connection_held_input".parse()?,
                    revision: owner.revision()?,
                    retry_after_connection: true,
                });
                preparation.finish_context(true).await?;
            } else {
                preparation.finish_context(false).await?;
                if preparation_end == PreparationEnd::CancelReleased {
                    // Without a lease, cleanup must neither wait for nor close
                    // the retained service's delayed delivery operation.
                    tokio::time::timeout(
                        Duration::from_millis(100),
                        preparation.finish_context(true),
                    )
                    .await??;
                }
            }
            if !cancel_preparation {
                let held = owner.context.lock().await;
                assert_eq!(held.attention, MailAttentionInfo::Unavailable);
                assert!(held.owns_runtime(), "preparation discarded retained work");
                if preparation_end == PreparationEnd::CancelReleased {
                    assert!(held.subscription_open && held.has_live_runtime());
                }
                let scope = held
                    .current_binding()
                    .map_err(|error| error.to_string())?
                    .ok_or("scope")?
                    .scope;
                let inbox = held.inbox.lock().map_err(|_| "inbox")?;
                let page = inbox.store().read_remote_mailbox(
                    &scope,
                    grapt_types::MailboxCursor::START,
                    64,
                    1048576,
                )?;
                assert_eq!(
                    page.records.len(),
                    usize::from(delayed_operation == "acknowledge_delivery")
                );
                assert!(page
                    .records
                    .iter()
                    .all(|record| record.delivery_pending() && !record.consumed()));
            }
            drop(preparation);
            let retry_before = second_owner
                .recovery
                .lock()
                .map_err(|_| "recovery")?
                .attempts();
            if shutdown {
                release_a
                    .send(())
                    .map_err(|_| "first shutdown request lost")?;
                release_b
                    .send(())
                    .map_err(|_| "second shutdown request lost")?;
                return Ok(());
            }
            release_a.send(()).map_err(|_| "first request lost")?;
            ready_a.await?;
            // Completing one owner's observation must not cancel the other's work.
            signal.send(()).map_err(|_| "first advisory lost")?;
            for index in 0..3 {
                let (done, joined) = oneshot::channel();
                mailbox.reserve()?.submit(
                    grapt_types::ExecutionId::new(format!("exe_unrelated_{index}"))
                        .ok_or("job id")?,
                    Box::pin(async move {
                        let _receiver_gone = done.send(());
                    }),
                    Arc::new(|| {}),
                    Arc::new(|| panic!("unrelated job failed")),
                );
                joined.await?;
                owners.changed.notify_one();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if delayed_operation == "fetch_snapshot_page" {
                assert!(retry_before > 0, "the HTTP refusal did not enter recovery");
                assert_eq!(
                    second_owner
                        .recovery
                        .lock()
                        .map_err(|_| "recovery")?
                        .attempts(),
                    retry_before,
                    "another session or job replaced the in-flight retry"
                );
            }
            if !cancel_preparation {
                release_b.send(()).map_err(|_| "second request lost")?;
            }
            ready_b.await?;
            assert!(
                owner
                    .wake_blocked
                    .lock()
                    .map_err(|_| "wake custody")?
                    .is_none(),
                "fresh authenticated connection stranded the held native input"
            );
            let targets = projection_targets(ProtocolFamily::OpenAiResponses)?;
            let mut settled = ProductPlan::transient(
                ProtocolFamily::OpenAiResponses,
                "current task".into(),
                PromptCacheKey::for_session(&second.session)?,
                targets.issue()?,
                targets,
                Arc::new(crate::dogfood::mcp::PublishedTools::default()),
                Vec::new(),
                step,
            )
            .with_mail_owner(owners.plan(&second.session)?);
            settled
                .refresh_context(step, HistoryGeneration::new(1))
                .await
                .map_err(|error| format!("{error:?}"))?;
            let prepared = settled
                .plan(step, 1, HistoryGeneration::new(1))
                .map_err(|error| error.detail)?;
            let batch = prepared
                .mail
                .ok_or("settled delivery missing from provider plan")?;
            assert_eq!(batch.count(), 1);
            assert!(batch.content().contains("delayed-mail"));
            settled.finish_context(false).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        }
        .await;
        let _joined = stop.send(());
        result
    };
    let (a, b, run, observed) = tokio::time::timeout(Duration::from_secs(12), async {
        tokio::join!(
            peer(
                listener_a,
                version,
                "recipient-one",
                Delay {
                    operation: delayed_operation,
                    entered: entered_a,
                    release: response_a,
                    shutdown,
                    cancel_preparation: false,
                },
                poll_a,
                Some(signal_received)
            ),
            peer(
                listener_b,
                version,
                "recipient-two",
                Delay {
                    operation: delayed_operation,
                    entered: entered_b,
                    release: response_b,
                    shutdown,
                    cancel_preparation,
                },
                poll_b,
                None
            ),
            driver.run_until(async {
                let _stopped = stopped.await;
            }),
            witness,
        )
    })
    .await?;
    observed.map_err(|error| {
        format!("delayed {delayed_operation} v{version}, shutdown={shutdown}: {error}; peers=({a:?}, {b:?}); driver={run:?}")
    })?;
    for (index, calls) in [a?, b?].into_iter().enumerate() {
        for operation in [
            "open",
            "subscribe_and_snapshot",
            "fetch_snapshot_page",
            "acknowledge_delivery",
            "close",
        ] {
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| call.as_str() == operation)
                    .count(),
                if (cancel_preparation && index == 1)
                    || (operation == "subscribe_and_snapshot"
                        && delayed_operation == "fetch_snapshot_page"
                        && !shutdown)
                {
                    2
                } else {
                    usize::from(
                        !(shutdown
                            && delayed_operation == "fetch_snapshot_page"
                            && operation == "acknowledge_delivery"),
                    )
                },
                "operation duplicated for one session: {calls:?}"
            );
        }
        assert_eq!(calls.last().map(String::as_str), Some("close"));
    }
    run?;
    for context in [&first, &second] {
        let scope = context
            .current_binding()
            .map_err(|e| e.to_string())?
            .ok_or("scope")?
            .scope;
        let inbox = context.inbox.lock().map_err(|_| "inbox")?;
        let page = inbox.store().read_remote_mailbox(
            &scope,
            grapt_types::MailboxCursor::START,
            64,
            1048576,
        )?;
        assert_eq!(
            page.records.len(),
            usize::from(!(shutdown && delayed_operation == "fetch_snapshot_page")),
            "delivery progress was lost or duplicated"
        );
        if let Some(record) = page.records.first() {
            assert!(
                record.actionable() && !record.consumed(),
                "delivery consumed the user's pending work"
            );
            assert_eq!(
                record.delivery_pending(),
                shutdown,
                "delivery debt did not match the completed ACK"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn coalesced_observation_rechecks_close_block_and_retirement() -> TestResult {
    for fence in 0..4 {
        let raw = r#"{"project_id":"secode","agent_id":"recipient"}"#;
        let context = context(Some(raw), true)?;
        let owners = owners(&context)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let plan = plan_at(&owners, &context, &listener).await?;
        let owner = plan.owner.clone();
        drop(plan);
        {
            let mut held = owner.context.lock().await;
            held.binding = held.current_binding().map_err(|error| error.to_string())?;
            held.wake.actionable_change();
            assert!(held.wake.take());
            held.wake.actionable_change();
        }
        let mut work = Box::pin(owners.drive_one(context.session.clone(), owner.clone()));
        poll_fn(|cx| {
            assert!(work.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        {
            let mut held = owner.context.lock().await;
            held.wake = Default::default();
            held.wake.actionable_change();
            match fence {
                0 => held.close_pending = true,
                1 => held.blocked_revision = Some(owner.revision()?),
                2 => held.require_rebind(),
                _ => owners.invalidate(&context.session),
            }
        }
        let done = tokio::time::timeout(Duration::from_secs(2), work).await?;
        assert!(matches!(done, DriveState::Idle));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err(),
            "fenced observation reached HTTP"
        );
        owners.close().await?;
    }
    Ok(())
}
