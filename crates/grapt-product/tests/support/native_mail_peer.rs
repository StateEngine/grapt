//! A bounded v2 loopback peer; no production listener or capability is used.
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, Arc};

#[path = "native_mail_peer/framing.rs"]
mod framing;
pub(crate) use framing::{request, FramingSlots};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub(crate) fn row(id: &str, attention: &str) -> Value {
    json!({"protocol":"remote-seat-mail-transport/v1","source_binding":"bind-native","projects":{"source":"secode","target":"secode"},"source_message":id,"source_cursor":format!("cursor-{id}"),"sender":"pilot","recipient":"coder","kind":"handoff","body":format!("{id} full-body detail {}", "🦀".repeat(100)),"refs":[],"supersession":{"supersedes":null,"superseded_by":null},"archive":{"disposition":"active","reason":null},"source_created":"2026-10-13T15:00:00.000Z","attention":attention})
}

pub(crate) async fn serve(
    listener: TcpListener,
    poll_entered: Option<tokio::sync::oneshot::Sender<()>>,
    version: u8,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    serve_controlled(listener, poll_entered, version, None, None, false, 10).await
}

pub(crate) struct Refusals {
    pub operation: &'static str,
    pub cause: &'static str,
    pub remaining: usize,
    pub skip: usize,
    pub attempts: Arc<std::sync::atomic::AtomicUsize>,
}

pub(crate) async fn serve_refusing(
    listener: TcpListener,
    refusals: Refusals,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    serve_controlled(listener, None, 2, None, Some(refusals), false, 10).await
}

pub(crate) async fn serve_unauthenticated(
    listener: TcpListener,
    refusals: Refusals,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    serve_controlled(listener, None, 2, None, Some(refusals), true, 10).await
}

pub(crate) async fn serve_with_advisory(
    listener: TcpListener,
    poll_entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
    rescanned: tokio::sync::oneshot::Sender<()>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    serve_controlled(
        listener,
        Some(poll_entered),
        2,
        Some(Advisory::Refresh {
            release,
            completed: rescanned,
        }),
        None,
        false,
        10,
    )
    .await
}

pub(crate) async fn serve_quiet(
    listener: TcpListener,
    poll_entered: tokio::sync::oneshot::Sender<()>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    serve_controlled(listener, Some(poll_entered), 2, None, None, true, 10).await
}

pub(crate) async fn serve_renewing(
    listener: TcpListener,
    polls: Arc<std::sync::atomic::AtomicUsize>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    serve_controlled(
        listener,
        None,
        2,
        None,
        Some(Refusals {
            operation: "next_change",
            cause: "Deadline",
            remaining: 0,
            skip: 0,
            attempts: polls,
        }),
        true,
        12,
    )
    .await
}

pub(crate) async fn serve_restarted(
    listener: TcpListener,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    // A replacement may start just after a slow retry; wait for the next one.
    serve_controlled(listener, None, 2, None, None, false, 45).await
}

#[derive(Clone, Copy)]
pub(crate) enum PollLoss {
    Http(u16),
    Malformed,
    Disconnected,
}

enum Advisory {
    Refresh {
        release: tokio::sync::oneshot::Receiver<()>,
        completed: tokio::sync::oneshot::Sender<()>,
    },
    Loss {
        kind: PollLoss,
        failed: tokio::sync::oneshot::Sender<()>,
        mail_sent: tokio::sync::oneshot::Receiver<()>,
    },
}

pub(crate) async fn serve_poll_loss(
    listener: TcpListener,
    kind: PollLoss,
    failed: tokio::sync::oneshot::Sender<()>,
    mail_sent: tokio::sync::oneshot::Receiver<()>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    serve_controlled(
        listener,
        None,
        2,
        Some(Advisory::Loss {
            kind,
            failed,
            mail_sent,
        }),
        None,
        true,
        10,
    )
    .await
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn serve_controlled(
    listener: TcpListener,
    mut poll_entered: Option<tokio::sync::oneshot::Sender<()>>,
    version: u8,
    mut advisory: Option<Advisory>,
    mut refusals: Option<Refusals>,
    mut quiet: bool,
    accept_seconds: u64,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut rescanned = None;
    let mut poll: Option<AdvisoryWork> = None;
    let mut framing = FramingSlots::new();
    let profile: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/stateengine-native/stateengine-mail-profile.json"
    ))?;
    let mut calls = Vec::new();
    let mut opened = false;
    // A lost poll closes its subscription before resubscribing; serve on.
    let mut reopen_after_close = false;
    let mut retry_request = None;
    for _ in 0..64 {
        let (mut socket, framed) = tokio::select! {
            biased;
            result = async { poll.as_mut().ok_or("missing advisory work")?.await }, if poll.is_some() => {
                let (completed, new_mail) = result?;
                rescanned = completed;
                if new_mail { quiet = false; }
                poll = None;
                continue;
            }
            accepted = tokio::time::timeout(std::time::Duration::from_secs(accept_seconds), framing.next(&listener, version)) => accepted??,
        };
        let incoming = match framed {
            Ok(request) => request,
            // Dropping a retained poll can close its connection before framing.
            Err(error) if error.to_string() == "request framing" => continue,
            Err(error) => return Err(error),
        };
        let operation = incoming["operation"].as_str().ok_or("operation")?;
        calls.push(operation.to_owned());
        if operation != "open" && !opened {
            // A restarted producer has no old binding, including for close.
            let body = serde_json::to_vec(
                &json!({"ok":false,"failure":{"family":"TransportFailure","tag":"Disconnected","effect":"none"}}),
            )?;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
            socket.write_all(&body).await?;
            socket.shutdown().await?;
            continue;
        }
        if let Some(fault) = refusals
            .as_mut()
            .filter(|fault| fault.operation == operation)
        {
            fault.attempts.fetch_add(1, Ordering::SeqCst);
            if let Some(previous) = retry_request.take() {
                assert_eq!(
                    incoming, previous,
                    "recovery changed the exact failed operation"
                );
            }
            if fault.skip > 0 {
                fault.skip -= 1;
            } else if fault.remaining > 0 {
                fault.remaining -= 1;
                // A delivery-contract refusal closes before its slow retry.
                reopen_after_close |= fault.cause == "AcknowledgementDigestMismatch";
                retry_request = Some(incoming.clone());
                if fault.cause == "Deadline" {
                    // No synthetic error tag: let the actual five-second
                    // operation budget cancel this first subscribe request.
                    let mut byte = [0];
                    assert_eq!(
                        tokio::time::timeout(
                            std::time::Duration::from_secs(7),
                            socket.read(&mut byte)
                        )
                        .await??,
                        0,
                        "subscribe deadline did not cancel its owned HTTP request"
                    );
                    continue;
                }
                let open_failure =
                    matches!(fault.cause, "Unauthenticated" | "ForeignProjectMismatch");
                let (tag, effect) = if open_failure {
                    (fault.cause, "none")
                } else if fault.cause == "SubscriptionRefusedNoEffect" {
                    ("SubscriptionRefused", "none")
                } else {
                    (fault.cause, "unknown-retry-exact")
                };
                let body = serde_json::to_vec(
                    &json!({"ok":false,"failure":{"family":if open_failure {"OpenFailure"} else if operation == "acknowledge_delivery" {"DeliveryAckFailure"} else {"TransportFailure"},"tag":tag,"effect":effect,"reason":"capability-required"}}),
                )?;
                socket.write_all(format!("HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
                socket.write_all(&body).await?;
                socket.shutdown().await?;
                continue;
            }
        }
        let value = match operation {
            "open" => {
                opened = true;
                assert_eq!(
                    incoming["args"]["request"]["requested_protocol"],
                    format!("remote-seat-mail-transport/v{version}")
                );
                json!({"binding_id":"bind-native","protocol":format!("remote-seat-mail-transport/v{version}"),"adapter_version":format!("stateengine-native-mail/{version}"),"projects":{"source":"secode","target":"secode"},"verified_principal":"coder","local_recipient":"coder","mapping_version":"exact-seat-id/v1","takeover":{"generation":1,"displaced":null},"limits":profile["limits"],"capabilities":{"atomic_subscribe_and_snapshot":true,"durable_delivery_ack":true,"bulk_dismiss":true,"explicit_project_pair":true,"opaque_authenticated_cursors":true,"advisory_payload_bytes":0,"coalesced_pending_signals":1}})
            }
            "subscribe_and_snapshot" => {
                json!({"subscription_id":"sub-native","generation":1,"snapshot":{"snapshot_id":"snap-native","barrier":"barrier-native","start_after":incoming["args"]["resume"]["from_checkpoint"],"retained_origin":"retained-origin/v1"}})
            }
            "fetch_snapshot_page" => {
                let messages = if quiet {
                    Vec::new()
                } else if incoming["args"]["after"].is_null() {
                    vec![
                        row("mail-a", "wake"),
                        row("quiet", "quiet"),
                        row("mail-b", "wake"),
                    ]
                } else {
                    assert_eq!(incoming["args"]["after"], "cursor-mail-b");
                    Vec::new()
                };
                let last = (!messages.is_empty()).then_some("cursor-mail-b");
                let mut page = json!({"messages":messages,"next_after":null});
                if version == 2 {
                    page["refused_rows"] = json!([]);
                    page["last_cursor"] = json!(last);
                }
                page
            }
            "acknowledge_delivery" => {
                let ack = &incoming["args"]["ack"];
                json!({"tag":"Recorded","receipt":{"ack_id":ack["ack_id"],"source_message":ack["source_message"],"projects":ack["projects"]},"primary_delivery":{"witnessed":true},"floor_projection":{"state":"unarmed","outcome":"refused","disposition":null,"reason":"secondary-unarmed","outcome_record":{"state":"unreadable"},"retry_required":false,"retry":null}})
            }
            "next_change" => {
                if let Some(entered) = poll_entered.take() {
                    entered.send(()).map_err(|_| "poll witness disappeared")?;
                }
                if let Some(completed) = rescanned.take() {
                    completed
                        .send(())
                        .map_err(|_| "rescan witness disappeared")?;
                }
                if poll.is_some() {
                    return Err("duplicate owned advisory request".into());
                }
                let next = advisory.take();
                reopen_after_close |= matches!(next, Some(Advisory::Loss { .. }));
                poll = Some(Box::pin(serve_advisory(socket, next)));
                continue;
            }
            "close" => json!({"joined_tasks":0,"ticks_used":1}),
            _ => return Err("unexpected fake transport operation".into()),
        };
        if operation == "close" {
            if let Some(work) = poll.take() {
                let (completed, new_mail) = work.await?;
                rescanned = completed;
                if new_mail {
                    quiet = false;
                }
            }
        }
        let body = serde_json::to_vec(&json!({"ok":true,"value":value}))?;
        let reply = async {
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
            socket.write_all(&body).await?;
            socket.shutdown().await
        };
        if operation == "close" && std::mem::take(&mut reopen_after_close) {
            // The outage held this close past its bound; its client may be gone.
            let _ = reply.await;
            continue;
        }
        reply.await?;
        if operation == "close" {
            return Ok(calls);
        }
    }
    Err("fake mail transport exceeded its operation bound".into())
}

// One retained advisory may coexist with one serial ordinary operation, as it
// does in the normal producer. Its completion is joined before close responds.
type AdvisoryWork = std::pin::Pin<
    Box<
        dyn std::future::Future<
            Output = Result<
                (Option<tokio::sync::oneshot::Sender<()>>, bool),
                Box<dyn std::error::Error>,
            >,
        >,
    >,
>;
async fn serve_advisory(
    mut socket: tokio::net::TcpStream,
    advisory: Option<Advisory>,
) -> Result<(Option<tokio::sync::oneshot::Sender<()>>, bool), Box<dyn std::error::Error>> {
    match advisory {
        Some(Advisory::Refresh { release, completed }) => {
            release.await?;
            let body =
                serde_json::to_vec(&json!({"ok":true,"value":{"generation":1,"token":null}}))?;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await?;
            socket.write_all(&body).await?;
            socket.shutdown().await?;
            Ok((Some(completed), false))
        }
        Some(Advisory::Loss {
            kind,
            failed,
            mail_sent,
        }) => {
            match kind {
                PollLoss::Http(status) => socket.write_all(format!("HTTP/1.1 {status} Temporary\r\nContent-Type: text/plain\r\nContent-Length: 1024\r\n\r\n").as_bytes()).await?,
                PollLoss::Malformed => socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1\r\nConnection: close\r\n\r\n{").await?,
                PollLoss::Disconnected => {}
            }
            drop(socket);
            failed.send(()).map_err(|_| "failure receiver gone")?;
            mail_sent.await?;
            Ok((None, true))
        }
        None => {
            let mut byte = [0];
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(12), socket.read(&mut byte))
                    .await??,
                0,
                "owned poll was not dropped before shutdown"
            );
            Ok((None, false))
        }
    }
}
