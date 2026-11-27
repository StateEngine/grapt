//! Private loopback fixture: the actual HTTP runtime and product planner, no LLM.
use super::*;
use crate::dogfood::production::{tests::projection_targets, ProductPlan};
use grapt_provider::{
    CodexInputItem, InputMessage, MessageRole, PromptCacheKey, ProtocolFamily, ProviderRequest,
};
use grapt_runtime::{AttemptPlan, HistoryGeneration};
use serde_json::json;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub(in crate::dogfood::native_mail) use crate::native_mail_test_peer::{
    request, row, FramingSlots,
};

pub(in crate::dogfood::native_mail) async fn serve(
    listener: TcpListener,
    poll_entered: Option<tokio::sync::oneshot::Sender<()>>,
) -> TestResult {
    serve_cycles(listener, poll_entered, Lifecycle::Normal, None).await
}

pub(in crate::dogfood::native_mail) async fn serve_reconnecting(
    listener: TcpListener,
    renewed: tokio::sync::oneshot::Sender<()>,
    local_shutdown: bool,
) -> TestResult {
    serve_cycles(
        listener,
        Some(renewed),
        Lifecycle::Reconnect(local_shutdown),
        None,
    )
    .await
}

pub(in crate::dogfood::native_mail) async fn serve_preparation_retry(
    listener: TcpListener,
) -> TestResult {
    serve_cycles(listener, None, Lifecycle::PreparationRetry, None).await
}

pub(in crate::dogfood::native_mail) async fn serve_refused_close(
    listener: TcpListener,
    poll_entered: tokio::sync::oneshot::Sender<()>,
) -> TestResult {
    serve_cycles(listener, Some(poll_entered), Lifecycle::RefuseClose, None).await
}

pub(in crate::dogfood::native_mail) async fn serve_retained_poll(
    listener: TcpListener,
    entered: tokio::sync::oneshot::Sender<()>,
    drive_dropped: tokio::sync::oneshot::Receiver<()>,
    retained: tokio::sync::oneshot::Sender<()>,
) -> TestResult {
    serve_cycles(
        listener,
        Some(entered),
        Lifecycle::Normal,
        Some(PollProbe::Retention {
            drive_dropped,
            retained,
        }),
    )
    .await
}

pub(in crate::dogfood::native_mail) async fn serve_received_signal(
    listener: TcpListener,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
    rescanned: tokio::sync::oneshot::Sender<()>,
) -> TestResult {
    serve_cycles(
        listener,
        Some(entered),
        Lifecycle::Normal,
        Some(PollProbe::Respond { release, rescanned }),
    )
    .await
}

enum PollProbe {
    Retention {
        drive_dropped: tokio::sync::oneshot::Receiver<()>,
        retained: tokio::sync::oneshot::Sender<()>,
    },
    Respond {
        release: tokio::sync::oneshot::Receiver<()>,
        rescanned: tokio::sync::oneshot::Sender<()>,
    },
}

#[derive(Clone, Copy)]
enum Lifecycle {
    Normal,
    Reconnect(bool),
    RefuseClose,
    PreparationRetry,
    UnknownKindV1,
    UnknownKindV2,
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn serve_cycles(
    listener: TcpListener,
    poll_entered: Option<tokio::sync::oneshot::Sender<()>>,
    lifecycle: Lifecycle,
    mut probe: Option<PollProbe>,
) -> TestResult {
    let renew = matches!(lifecycle, Lifecycle::Reconnect(_));
    let preparation_retry = matches!(lifecycle, Lifecycle::PreparationRetry);
    let unknown = matches!(
        lifecycle,
        Lifecycle::UnknownKindV1 | Lifecycle::UnknownKindV2
    );
    let version = if matches!(lifecycle, Lifecycle::UnknownKindV2) {
        2
    } else {
        1
    };
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../../../fixtures/stateengine-native/stateengine-mail-profile.json"
    ))?;
    let mut operations = vec![
        "open",
        "subscribe_and_snapshot",
        "fetch_snapshot_page",
        "acknowledge_delivery",
        "acknowledge_delivery",
        "close",
    ];
    let mut poll_entered = poll_entered;
    if poll_entered.is_some() {
        operations.insert(5, "next_change");
    }
    let retained_signal = matches!(probe, Some(PollProbe::Respond { .. }));
    if retained_signal {
        // One retained signal invalidates the snapshot once. The resumed
        // subscription then stays idle until close. V1 replay reuses the exact
        // original delivery receipts rather than admitting another copy.
        operations.splice(
            6..6,
            [
                "subscribe_and_snapshot",
                "fetch_snapshot_page",
                "acknowledge_delivery",
                "acknowledge_delivery",
                "next_change",
            ],
        );
    }
    let mut rescanned = None;
    let mut original_acks = Vec::new();
    for cycle in 0..if renew || preparation_retry { 2 } else { 1 } {
        let binding = if cycle == 0 {
            "bind-native"
        } else {
            "bind-renewed"
        };
        for operation in &operations {
            let operation = *operation;
            if preparation_retry && cycle == 0 && operation == "acknowledge_delivery" {
                continue;
            }
            let (mut socket, _) =
                tokio::time::timeout(Duration::from_secs(2), listener.accept()).await??;
            let request = request(&mut socket, version).await?;
            assert_eq!(request["operation"], operation);
            if preparation_retry && cycle == 0 && operation == "fetch_snapshot_page" {
                // The real preparation watchdog must cancel this stalled read,
                // then confirm close before the next cycle can open.
                let mut byte = [0];
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await??,
                    0,
                    "timed-out preparation did not join its native HTTP request"
                );
                continue;
            }
            if operation == "next_change" {
                if !renew || cycle == 1 {
                    poll_entered
                        .take()
                        .or_else(|| rescanned.take())
                        .ok_or("poll or rescan witness")?
                        .send(())
                        .map_err(|_| "poll witness gone")?;
                }
                if renew && cycle == 0 {
                    // A real lost subscription requires joined reconnect;
                    // a quiet local HTTP deadline only renews its wait.
                    let (family, tag) = if matches!(lifecycle, Lifecycle::Reconnect(true)) {
                        ("SubscriptionClosed", "LocalShutdown")
                    } else {
                        ("TransportFailure", "Disconnected")
                    };
                    let body = serde_json::to_vec(&json!({"ok":false,"failure":{
                        "family":family,"tag":tag,"effect":"none"
                    }}))?;
                    socket.write_all(format!("HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await?;
                    socket.write_all(&body).await?;
                    socket.shutdown().await?;
                    continue;
                }
                let respond = match probe.take() {
                    Some(PollProbe::Retention {
                        drive_dropped,
                        retained,
                    }) => {
                        drive_dropped.await?;
                        let mut byte = [0];
                        assert!(
                            tokio::time::timeout(Duration::from_millis(50), socket.read(&mut byte))
                                .await
                                .is_err(),
                            "cancelling the combined drive dropped this inbox's pending wait"
                        );
                        retained.send(()).map_err(|_| "retention witness gone")?;
                        false
                    }
                    Some(PollProbe::Respond {
                        release,
                        rescanned: witness,
                    }) => {
                        release.await?;
                        rescanned = Some(witness);
                        true
                    }
                    None => false,
                };
                if !respond {
                    let mut byte = [0];
                    let count =
                        tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte))
                            .await??;
                    assert_eq!(
                        count, 0,
                        "service stop must drop the owned idle HTTP poll before close"
                    );
                    continue;
                }
            }
            let value = match operation {
                "open" => {
                    json!({"binding_id":binding,"protocol":format!("remote-seat-mail-transport/v{version}"),"adapter_version":format!("stateengine-native-mail/{version}"),"projects":{"source":"secode","target":"secode"},"verified_principal":"coder","local_recipient":"coder","mapping_version":"exact-seat-id/v1","takeover":{"generation":cycle+1,"displaced":null},"limits":fixture["limits"],"capabilities":{"atomic_subscribe_and_snapshot":true,"durable_delivery_ack":true,"bulk_dismiss":true,"explicit_project_pair":true,"opaque_authenticated_cursors":true,"advisory_payload_bytes":0,"coalesced_pending_signals":1}})
                }
                "subscribe_and_snapshot" => {
                    json!({"subscription_id":"sub-native","generation":1,"snapshot":{"snapshot_id":"snap-native","barrier":"barrier-native","start_after":null,"retained_origin":"retained-origin/v1"}})
                }
                "fetch_snapshot_page" => {
                    let mut wake = row("wake-1", "wake");
                    let mut quiet = row("quiet-2", "quiet");
                    wake["source_binding"] = json!(binding);
                    quiet["source_binding"] = json!(binding);
                    let mut messages = vec![wake, quiet];
                    if unknown {
                        let mut future = row("unknown-kind-row", "wake");
                        future["source_binding"] = json!(binding);
                        // An inadmissible label: a merely unknown one is
                        // admitted as ordinary mail and never skipped.
                        future["kind"] = json!("");
                        future["body"] = json!("future-kind-body-sensitive");
                        messages.push(future);
                    }
                    let mut page = json!({"messages":messages,"next_after":null});
                    if version == 2 {
                        // A source-reported row the transport refused. The
                        // cursor is opaque and only length-bounded, so it may
                        // carry control characters; presenting it must escape
                        // them rather than print them raw to a terminal.
                        page["refused_rows"] = json!([{
                            "msg_id": null, "start": 0, "end": 4,
                            "reason": "MalformedEnvelope",
                            "source_cursor": "esc\u{1b}[31m\r\nforged",
                        }]);
                        page["last_cursor"] = json!(if unknown {
                            "cursor-unknown-kind-row"
                        } else {
                            "cursor-quiet-2"
                        });
                    }
                    page
                }
                "acknowledge_delivery" => {
                    let ack = &request["args"]["ack"];
                    if retained_signal {
                        if original_acks.len() < 2 {
                            original_acks.push(ack.clone());
                        } else {
                            let original = original_acks
                                .iter()
                                .find(|prior| prior["source_message"] == ack["source_message"])
                                .ok_or("replayed source message changed")?;
                            assert_eq!(ack, original, "rescan changed delivery identity");
                        }
                    }
                    assert_eq!(ack.as_object().ok_or("ack")?.len(), 10);
                    assert_eq!(ack["recipient"], "coder");
                    assert_eq!(
                        ack["source_binding"],
                        if preparation_retry {
                            binding
                        } else {
                            "bind-native"
                        },
                        "ACK must retain its original admission coordinates"
                    );
                    json!({"tag":"Recorded","receipt":{"ack_id":ack["ack_id"],"source_message":ack["source_message"],"projects":ack["projects"]},"primary_delivery":{"witnessed":true},"floor_projection":{"state":"unarmed","outcome":"refused","disposition":null,"reason":"secondary-unarmed","outcome_record":{"state":"unreadable"},"retry_required":false,"retry":null}})
                }
                "next_change" => json!({"generation":1,"token":null}),
                "close" => {
                    json!({"joined_tasks":if matches!(lifecycle, Lifecycle::RefuseClose) { 3 } else { 0 },"ticks_used":1})
                }
                _ => return Err("fixture operation".into()),
            };
            let body = serde_json::to_vec(&json!({"ok":true,"value":value}))?;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await?;
            socket.write_all(&body).await?;
            socket.shutdown().await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn authenticated_mail_reaches_the_real_plan_tail_without_consuming_during_preparation(
) -> TestResult {
    for family in [
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::ChatGptCodex,
        ProtocolFamily::AnthropicMessages,
        ProtocolFamily::OpenAiCompatible,
    ] {
        plan_tail(family, Lifecycle::Normal).await?;
    }
    Ok(())
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn plan_tail(family: ProtocolFamily, lifecycle: Lifecycle) -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let unknown = matches!(
        lifecycle,
        Lifecycle::UnknownKindV1 | Lifecycle::UnknownKindV2
    );
    let raw = if matches!(lifecycle, Lifecycle::UnknownKindV2) {
        r#"{"project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#
    } else {
        r#"{"project_id":"secode","agent_id":"coder"}"#
    };
    let mut mail = context(Some(raw), true)?;
    mail.use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let inbox = mail.inbox.clone();
    let scope = mail
        .current_binding()
        .map_err(|e| e.to_string())?
        .ok_or("binding")?
        .scope;
    let step = StepNumber::new(1).ok_or("step")?;
    let targets = projection_targets(family)?;
    let established = vec![
        CodexInputItem::Message(InputMessage {
            role: MessageRole::System,
            content: "fixed system instructions".into(),
        }),
        CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            content: "established conversation".into(),
        }),
        CodexInputItem::ToolOutput {
            call: grapt_types::ToolCallId::new("call_prior_result").ok_or("prior tool call")?,
            output: grapt_provider::CodexToolText::try_new("prior tool result".into())?,
            is_error: false,
        },
    ];
    let cache = PromptCacheKey::for_session(&mail.session)?;
    let published = Arc::new(crate::dogfood::mcp::PublishedTools::default());
    let mut baseline = ProductPlan::transient(
        family,
        "current task".into(),
        cache.clone(),
        targets.issue()?,
        targets.clone(),
        published.clone(),
        established.clone(),
        step,
    );
    let baseline = baseline
        .plan(step, 1, HistoryGeneration::new(1))
        .map_err(|e| e.detail)?
        .request;
    let prefix =
        grapt_provider::prefix_digest(&baseline, established.len() + 1).ok_or("baseline prefix")?;
    let mut plan = ProductPlan::transient(
        family,
        "current task".into(),
        cache,
        targets.issue()?,
        targets,
        published,
        established,
        step,
    )
    .with_native_mail(mail);
    let (server, client) = tokio::join!(serve_cycles(listener, None, lifecycle, None), async {
        // Always join close before returning a client failure to the test.
        let result = async {
            plan.refresh_context(step,HistoryGeneration::new(1)).await.map_err(|e|format!("{e:?}"))?;
            let planned = plan.plan(step,1,HistoryGeneration::new(1)).map_err(|e|e.detail)?;
            let batch = planned.mail.ok_or("missing sealed mail batch")?;
            assert_eq!(batch.count(),1,"quiet mail must remain outside the model tail");
            assert_eq!(grapt_provider::prefix_digest(&planned.request, 4), Some(prefix), "mail changed earlier inputs, model or tool definitions for {family:?}");
            let input = match &planned.request {
                ProviderRequest::GoogleGemini(r) | ProviderRequest::VertexGemini(r) => &r.messages,
                ProviderRequest::OpenAiResponses(r) => &r.input,
                ProviderRequest::ChatGptCodex(r) => &r.input,
                ProviderRequest::AnthropicMessages(r) | ProviderRequest::VertexAnthropic(r) => &r.messages,
                ProviderRequest::OpenAiCompatible(r) | ProviderRequest::WatsonxChat(r) => &r.messages,
        ProviderRequest::BedrockConverse(r) => &r.messages,
                ProviderRequest::CohereChat(r) => &r.messages,
            };
            let tail = input.last().ok_or("tail")?;
            assert!(matches!(tail,CodexInputItem::Message(m) if m.role==MessageRole::User && m.content==batch.content()));
            assert!(batch.content().contains("wake-1"));
            assert!(!batch.content().contains("quiet-2"));
            let prior = 2;
            assert!(matches!(input.get(input.len()-prior),Some(CodexInputItem::Message(m)) if m.content=="current task"));
            if unknown {
                // The source reported a row it could not hand over. That belongs
                // in a diagnostic, not the conversation: the reader cannot act
                // on it, and the admitted mail is unaffected.
                assert!(
                    !input.iter().any(|item| match item {
                        CodexInputItem::Message(m) => m.content.contains("unknown kind")
                            || m.content.contains("undecodable"),
                        _ => false,
                    }),
                    "a source-side decode failure must not enter the conversation"
                );
                let rendered=format!("{input:?}");
                assert!(!rendered.contains("future-kind-body-sensitive"));
                assert!(!rendered.contains("fixture-secret"));
            }
            let held = inbox.lock().map_err(|_|"inbox")?;
            let page = held.store().read_remote_mailbox(&scope,grapt_types::MailboxCursor::START,64,1048576)?;
            assert_eq!(page.records.len(),2);
            assert!(page.records.iter().all(|r|!r.consumed()&&!r.delivery_pending()));
            assert_eq!(page.records.iter().filter(|r|r.actionable()).count(),1);
            Ok::<(),Box<dyn std::error::Error>>(())
        }.await;
        let closed = plan.shutdown().await;
        result?;
        closed?;
        Ok::<(), Box<dyn std::error::Error>>(())
    });
    server?;
    client
}

#[tokio::test]
async fn bound_project_reaches_native_open_and_mismatch_refuses_preparation() -> TestResult {
    for version in [1, 2] {
        let raw = json!({"project_id":"fixture-project","agent_id":"coder",
            "mail_transport_protocol":format!("remote-seat-mail-transport/v{version}")});
        let mut mail = context(Some(&raw.to_string()), true)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        mail.use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
        let mut plan = custody::NativeMailPlan::private(mail);
        let step = StepNumber::new(1).ok_or("step")?;
        let server = async {
            let (mut socket, _) = listener.accept().await?;
            let request = request(&mut socket, version).await?;
            assert_eq!(request["operation"], "open");
            assert_eq!(
                request["args"]["request"]["expected_local_project"], "fixture-project",
                "the binding project was replaced by a product default"
            );
            let body = json!({"ok":false,"failure":{"family":"OpenFailure",
                "tag":"ForeignProjectMismatch","effect":"none"}})
            .to_string();
            socket.write_all(format!("HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
            socket.shutdown().await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        };
        let (served, refused) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(server, plan.refresh(step))
        })
        .await?;
        served?;
        assert_eq!(refused, Err(E::Principal));
        plan.finish(true).await?;
        let again = tokio::time::timeout(Duration::from_millis(100), plan.refresh(step)).await?;
        assert_eq!(
            again,
            Err(E::Principal),
            "prompt preparation forgot the refused binding"
        );
        plan.finish(false).await?;
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err(),
            "a mismatched project was retried without authenticated rebind"
        );
        assert!(E::Principal.message().contains("rebind"));
    }
    Ok(())
}

#[tokio::test]
async fn local_unknown_kind_warning_reaches_real_model_plans_without_mail_authority() -> TestResult
{
    for family in [
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::ChatGptCodex,
        ProtocolFamily::AnthropicMessages,
        ProtocolFamily::OpenAiCompatible,
    ] {
        for lifecycle in [Lifecycle::UnknownKindV1, Lifecycle::UnknownKindV2] {
            plan_tail(family, lifecycle).await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn local_unknown_kind_keeps_ready_attention_and_sealed_healthy_mail() -> TestResult {
    let mut mail = context(
        Some(
            r#"{"project_id":"secode","agent_id":"coder","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
        ),
        true,
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    mail.use_test_port(NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?);
    let (server, witness) = tokio::join!(
        serve_cycles(listener, None, Lifecycle::UnknownKindV2, None),
        async {
            let result = async {
                mail.refresh(StepNumber::new(1).ok_or("step")?)
                    .await
                    .map_err(|e| e.to_string())?;
                assert!(
                    matches!(mail.attention,MailAttentionInfo::Ready{pending,decode_failures,..}
                if pending.get()==1 && decode_failures.get()==1)
                );
                // A source-reported decode failure is a diagnostic, not a notice
                // to the reader: the admitted mail is unaffected, and the reader
                // has nothing to act on.
                assert!(
                    mail.decode_warning().map_err(|e| e.to_string())?.is_none(),
                    "a source-side decode failure must not reach the conversation"
                );
                assert_eq!(
                    mail.take_batch()
                        .map_err(|e| e.to_string())?
                        .ok_or("batch")?
                        .count(),
                    1
                );
                Ok::<(), Box<dyn std::error::Error>>(())
            }
            .await;
            let closed = mail.finish().await;
            result?;
            closed?;
            Ok::<(), Box<dyn std::error::Error>>(())
        }
    );
    server?;
    witness
}

#[test]
fn source_positions_escape_control_characters_before_a_terminal() -> TestResult {
    // A source cursor is opaque and only length-bounded, so a valid one may
    // carry a terminal escape sequence or a newline.
    let hostile = ExclusiveSourceCursor::try_new("esc\u{1b}[31m\r\nforged")?;
    let plain = ExclusiveSourceCursor::try_new("cursor-2")?;
    let rendered = render_positions(&[hostile, plain]);
    assert!(
        !rendered.contains('\u{1b}'),
        "escape byte must not survive: {rendered}"
    );
    assert!(
        !rendered.contains('\n'),
        "newline must not survive: {rendered}"
    );
    assert!(
        !rendered.contains('\r'),
        "carriage return must not survive: {rendered}"
    );
    // Both positions remain identifiable and distinctly bounded.
    assert!(rendered.contains("cursor-2"));
    assert!(rendered.contains("\\u{1b}") || rendered.contains("\\x1b"));
    Ok(())
}
