//! Disposable loopback HTTP witnesses, driven and joined by each test runtime.
use super::{tests::*, *};
#[path = "../../../../test-support/executable.rs"]
mod executable;
use std::future::Future;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

enum Reply {
    Json(Value),
    Bytes(Vec<u8>),
    Lost,
    Status(u16),
    Redirect(u16),
    InvalidHeaders { redirect: bool, held: bool },
    UntilCancelled(tokio::sync::oneshot::Sender<()>),
}
struct Exchange {
    operation: &'static str,
    reply: Reply,
}
fn exchange(operation: &'static str, value: Value) -> Exchange {
    Exchange {
        operation,
        reply: Reply::Json(json!({"ok":true,"value":value})),
    }
}
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn handle(
    mut socket: TcpStream,
    expected: Exchange,
    protocol: MailTransportVersion,
) -> TestResult {
    let mut data = Vec::new();
    let boundary;
    loop {
        let mut chunk = [0; 2048];
        let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk)).await??;
        if count == 0 || data.len() + count > 65536 {
            return Err("request framing".into());
        }
        data.extend_from_slice(&chunk[..count]);
        if let Some(index) = data.windows(4).position(|s| s == b"\r\n\r\n") {
            boundary = index + 4;
            break;
        }
    }
    let header = std::str::from_utf8(&data[..boundary])?;
    assert!(header.starts_with(&format!("POST /{} HTTP/1.1\r\n", protocol.as_str())));
    assert!(header
        .lines()
        .any(|s| s.eq_ignore_ascii_case("authorization: Bearer fixture-secret")));
    let length = header
        .lines()
        .find_map(|s| {
            s.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .map(|(_, v)| v.trim().parse::<usize>())
        })
        .ok_or("length")??;
    if length > 32768 {
        return Err("request bound".into());
    }
    while data.len() < boundary + length {
        let mut chunk = [0; 2048];
        let count = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk)).await??;
        if count == 0 || data.len() + count > 65536 {
            return Err("request body".into());
        }
        data.extend_from_slice(&chunk[..count]);
    }
    let request = decode::strict_value(&data[boundary..boundary + length])?;
    decode::exact(&request, &["operation", "args"])?;
    assert_eq!(request["operation"].as_str(), Some(expected.operation));
    if expected.operation == "open" {
        let kinds = request["args"]["request"].get("accepted_kinds");
        if protocol == MailTransportVersion::V2 {
            assert_eq!(
                kinds,
                Some(&json!(RemoteSeatMailKind::KNOWN
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()))
            );
            assert!(request["args"]["request"].get("extensions").is_none());
        } else {
            assert!(kinds.is_none());
        }
    }
    if expected.operation == "acknowledge_delivery" {
        decode::exact(
            &request["args"]["ack"],
            &[
                "ack_id",
                "source_binding",
                "projects",
                "source_message",
                "source_cursor",
                "recipient",
                "local_message",
                "local_admission",
                "canonical_message_digest",
                "protocol",
            ],
        )?;
        assert!(request["args"]["ack"].get("read").is_none());
    }
    let body = match expected.reply {
        Reply::Json(value) => serde_json::to_vec(&value)?,
        Reply::Bytes(bytes) => bytes,
        Reply::Lost => return Ok(()),
        Reply::Status(status) => {
            socket.write_all(format!("HTTP/1.1 {status} Temporary\r\nContent-Type: text/plain\r\nContent-Length: 1024\r\n\r\n").as_bytes()).await?;
            let mut byte = [0];
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await??,
                0,
                "an unsuccessful advisory must not wait for its foreign body"
            );
            return Ok(());
        }
        Reply::Redirect(port) => {
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/foreign\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await?;
            socket.shutdown().await?;
            return Ok(());
        }
        Reply::InvalidHeaders { redirect, held } => {
            let header = if redirect {
                "HTTP/1.1 302 Found\r\nContent-Type: application/json\r\nContent-Length: 1024\r\n\r\n"
            } else {
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1024\r\n\r\n"
            };
            socket.write_all(header.as_bytes()).await?;
            if held {
                let mut byte = [0];
                let count =
                    tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await??;
                assert_eq!(
                    count, 0,
                    "invalid headers must close the request without waiting for its body"
                );
            } else {
                socket.write_all(b"{").await?;
                socket.shutdown().await?;
            }
            return Ok(());
        }
        Reply::UntilCancelled(entered) => {
            entered.send(()).map_err(|_| "poll witness receiver gone")?;
            let mut byte = [0];
            let count =
                tokio::time::timeout(Duration::from_secs(2), socket.read(&mut byte)).await??;
            assert_eq!(count, 0, "dropping poll must close its owned HTTP request");
            return Ok(());
        }
    };
    let header = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(&body).await?;
    socket.shutdown().await?;
    Ok(())
}
async fn server(
    plan: Vec<Exchange>,
) -> TestResult<(HttpMailTransport, JoinHandle<Result<(), String>>)> {
    server_protocol(plan, MailTransportVersion::V1).await
}
async fn server_protocol(
    plan: Vec<Exchange>,
    protocol: MailTransportVersion,
) -> TestResult<(HttpMailTransport, JoinHandle<Result<(), String>>)> {
    // Page-in is fixture preparation; the real request still has its original
    // 300ms allowance, and all four test runtimes remain concurrent.
    static PREPARED: std::sync::OnceLock<Result<u64, String>> = std::sync::OnceLock::new();
    let binary = std::env::current_exe()?;
    PREPARED
        .get_or_init(|| executable::prepare(&binary))
        .as_ref()
        .map_err(|error| format!("mail HTTP {error}"))?;
    if plan.len() > 8 {
        return Err("test server plan bound".into());
    }
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = NonZeroU16::new(listener.local_addr()?.port()).ok_or("port")?;
    let transport = HttpMailTransport::with_protocol(
        port,
        Arc::new(Secret::new("fixture-secret".to_owned())),
        protocol,
    )?;
    let task = tokio::spawn(async move {
        let mut tasks = Vec::new();
        let mut failure = None;
        for expected in plan {
            let socket = match tokio::time::timeout(Duration::from_secs(2), listener.accept()).await
            {
                Ok(Ok((socket, _))) => socket,
                _ => {
                    failure = Some("accept failure".to_owned());
                    break;
                }
            };
            tasks.push(tokio::spawn(async move {
                handle(socket, expected, protocol)
                    .await
                    .map_err(|_| "exchange failure".to_owned())
            }));
        }
        // Join every owned handler even when one fails.
        for task in tasks {
            if !matches!(task.await, Ok(Ok(()))) {
                failure = Some("handler failure".to_owned());
            }
        }
        if let Some(failure) = failure {
            Err(failure)
        } else {
            Ok(())
        }
    });
    Ok((transport, task))
}
pub(super) fn run(future: impl Future<Output = TestResult>) -> TestResult {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future)
}
async fn joined(task: JoinHandle<Result<(), String>>) -> TestResult {
    task.await?.map_err(Into::into)
}
fn subscription_value() -> Value {
    json!({"subscription_id":"sub-1","generation":1,"snapshot":{"snapshot_id":"snap-1","barrier":"barrier-1","start_after":null,"retained_origin":"retained-origin/v1"}})
}

#[test]
fn authenticated_http_open_rejects_foreign_principal_and_redacts_credentials() -> TestResult {
    run(async {
        let mut value = binding_value();
        value["verified_principal"] = json!("foreign");
        let (mut port, task) = server(vec![exchange("open", value)]).await?;
        assert!(!format!("{port:?}").contains("fixture-secret"));
        assert_eq!(
            port.open(request()?, profile()).await.err(),
            Some(OpenFailure::ExactPrincipalMismatch.into())
        );
        joined(task).await
    })
}

#[test]
fn missing_or_malformed_write_reply_keeps_exact_retry_effect() -> TestResult {
    for reply in [
        Reply::Lost,
        Reply::Bytes(br#"{"ok":true,"value":{"tag":"Recorded"}}"#.to_vec()),
        Reply::Bytes(br#"{"ok":false,"ok":true,"value":{}}"#.to_vec()),
        Reply::Json(
            json!({"ok":false,"failure":{"family":"TransportFailure","tag":"TransportUnavailable","source_state":false}}),
        ),
    ] {
        run(async {
            let (mut port, task) = server(vec![
                exchange("open", binding_value()),
                Exchange {
                    operation: "acknowledge_delivery",
                    reply,
                },
            ])
            .await?;
            let binding = port.open(request()?, profile()).await?;
            let failure = port
                .acknowledge_delivery(&binding, ack()?)
                .await
                .err()
                .ok_or("must fail")?;
            assert_eq!(failure.effect, WriteEffect::UnknownRetryExact);
            assert!(!failure.to_string().contains("fixture-secret"));
            joined(task).await
        })?;
    }
    Ok(())
}

#[test]
fn typed_primary_delivery_is_distinct_from_secondary_projection_failure() -> TestResult {
    run(async {
        let (mut port, task) = server(vec![
            exchange("open", binding_value()),
            exchange("acknowledge_delivery", ack_value("refused")),
        ])
        .await?;
        let binding = port.open(request()?, profile()).await?;
        let reply = port.acknowledge_delivery(&binding, ack()?).await?;
        assert_eq!(
            reply.receipt().projection(),
            Some(crate::ack::SecondaryProjectionState::Refused)
        );
        joined(task).await
    })
}

#[test]
fn mixed_page_keeps_exact_attention_and_rejects_pre_correction_rows() -> TestResult {
    for missing in [false, true] {
        run(async {
            let mut quiet = row("msg-1", "done", "quiet");
            if missing {
                quiet.as_object_mut().ok_or("row")?.remove("attention");
            }
            let page = json!({"messages":[quiet,row("msg-2","handoff","wake"),row("msg-3","done","wake")],"next_after":null});
            let (mut port, task) = server(vec![
                exchange("open", binding_value()),
                exchange("subscribe_and_snapshot", subscription_value()),
                exchange("fetch_snapshot_page", page),
            ])
            .await?;
            let binding = port.open(request()?, profile()).await?;
            let subscription = port
                .subscribe_and_snapshot(&binding, ResumeRequest::cold())
                .await?;
            let result = port
                .fetch_snapshot_page(
                    &binding,
                    subscription.snapshot(),
                    None,
                    PageLimits::from_settled(binding.receipt().limits()),
                )
                .await;
            if missing {
                assert!(result.is_err());
            } else {
                assert_eq!(
                    result?
                        .messages()
                        .iter()
                        .map(BoundRemoteSeatMailMessageV1::attention)
                        .collect::<Vec<_>>(),
                    vec![
                        MailAttention::Quiet,
                        MailAttention::Wake,
                        MailAttention::Wake
                    ]
                );
            }
            joined(task).await
        })?;
    }
    Ok(())
}

#[test]
fn subscribe_timeout_reply_preserves_binding_for_an_exact_retry() -> TestResult {
    run(async {
        let (mut port, task) = server(vec![
            exchange("open", binding_value()),
            Exchange {
                operation: "subscribe_and_snapshot",
                reply: Reply::Status(408),
            },
            exchange("subscribe_and_snapshot", subscription_value()),
        ])
        .await?;
        let binding = port.open(request()?, profile()).await?;
        let failed = port
            .subscribe_and_snapshot(&binding, ResumeRequest::cold())
            .await;
        let retried = port
            .subscribe_and_snapshot(&binding, ResumeRequest::cold())
            .await;
        joined(task).await?;
        let failure = failed.err().ok_or("408 became a subscription")?;
        assert_eq!(
            failure.cause,
            OperationCause::Connection(ConnectionFailure::Unavailable)
        );
        assert_eq!(failure.effect, WriteEffect::None);
        assert!(retried.is_ok());
        Ok(())
    })
}

#[test]
fn pending_change_does_not_borrow_port_and_drop_releases_only_owned_poll() -> TestResult {
    run(async {
        let (entered, observed) = tokio::sync::oneshot::channel();
        let (mut port, task) = server(vec![
            exchange("open", binding_value()),
            exchange("subscribe_and_snapshot", subscription_value()),
            Exchange {
                operation: "next_change",
                reply: Reply::Status(302),
            },
            Exchange {
                operation: "next_change",
                reply: Reply::Status(408),
            },
            Exchange {
                operation: "next_change",
                reply: Reply::Status(503),
            },
            Exchange {
                operation: "next_change",
                reply: Reply::UntilCancelled(entered),
            },
            exchange("acknowledge_delivery", ack_value("posted")),
        ])
        .await?;
        let binding = port.open(request()?, profile()).await?;
        let mut subscription = port
            .subscribe_and_snapshot(&binding, ResumeRequest::cold())
            .await?;
        for _ in 0..3 {
            let failure = port
                .next_change(&mut subscription)
                .await
                .err()
                .ok_or("HTTP error became an advisory")?;
            assert_eq!(
                failure.cause,
                OperationCause::Connection(ConnectionFailure::Unavailable)
            );
            assert_eq!(failure.effect, WriteEffect::None);
        }
        let first = port.next_change(&mut subscription);
        let duplicate = port.next_change(&mut subscription);
        assert_eq!(
            duplicate.await.err(),
            Some(TransportFailure::SubscriptionRefused.into())
        );
        drop(first); // A never-polled request must release its local pending bit.
        let poll = port.next_change(&mut subscription);
        let poll = tokio::spawn(poll);
        // Observe the actual pending request before admitting the ordinary ACK.
        tokio::time::timeout(Duration::from_secs(2), observed).await??;
        let outcome = port.acknowledge_delivery(&binding, ack()?).await;
        poll.abort();
        let stopped = poll.await;
        assert!(stopped.is_err_and(|e| e.is_cancelled()));
        assert!(outcome.is_ok());
        joined(task).await
    })
}

#[test]
fn closed_authentication_failure_can_happen_after_open_without_false_ack_success() -> TestResult {
    run(async {
        let refusal = json!({"ok":false,"failure":{"family":"OpenFailure","tag":"Unauthenticated","reason":"capability-revoked"}});
        let (mut port, task) = server(vec![
            exchange("open", binding_value()),
            Exchange {
                operation: "acknowledge_delivery",
                reply: Reply::Json(refusal),
            },
        ])
        .await?;
        let binding = port.open(request()?, profile()).await?;
        let failure = port
            .acknowledge_delivery(&binding, ack()?)
            .await
            .err()
            .ok_or("refusal")?;
        assert_eq!(
            failure.cause,
            OperationCause::Open(OpenFailure::Unauthenticated)
        );
        assert_eq!(failure.effect, WriteEffect::None);
        joined(task).await
    })
}

#[test]
fn repeated_page_cursor_or_duplicate_source_id_is_not_progress() -> TestResult {
    for repeated in [false, true] {
        run(async {
            let rows = if repeated {
                vec![row("msg-1", "handoff", "wake")]
            } else {
                vec![
                    row("msg-1", "handoff", "wake"),
                    row("msg-1", "handoff", "wake"),
                ]
            };
            let (mut port, task) = server(vec![
                exchange("open", binding_value()),
                exchange("subscribe_and_snapshot", subscription_value()),
                exchange(
                    "fetch_snapshot_page",
                    json!({"messages":rows,"next_after":null}),
                ),
            ])
            .await?;
            let binding = port.open(request()?, profile()).await?;
            let subscription = port
                .subscribe_and_snapshot(&binding, ResumeRequest::cold())
                .await?;
            let cursor = ExclusiveSourceCursor::new("cursor-msg-1").ok_or("cursor")?;
            let after = if repeated { Some(&cursor) } else { None };
            assert_eq!(
                port.fetch_snapshot_page(
                    &binding,
                    subscription.snapshot(),
                    after,
                    PageLimits::from_settled(binding.receipt().limits())
                )
                .await
                .err(),
                Some(TransportFailure::CursorRegressed.into())
            );
            joined(task).await
        })?;
    }
    Ok(())
}

#[test]
fn lost_close_reply_does_not_supply_join_proof_or_a_usable_subscription() -> TestResult {
    run(async {
        let (mut port, task) = server(vec![
            exchange("open", binding_value()),
            exchange("subscribe_and_snapshot", subscription_value()),
            Exchange {
                operation: "close",
                reply: Reply::Lost,
            },
        ])
        .await?;
        let binding = port.open(request()?, profile()).await?;
        let mut subscription = port
            .subscribe_and_snapshot(&binding, ResumeRequest::cold())
            .await?;
        assert!(port
            .close(binding, ShutdownBudget::new(300, 2))
            .await
            .is_err());
        assert_eq!(
            port.next_change(&mut subscription).await.err(),
            Some(SubscriptionClosed::Disconnected.into())
        );
        joined(task).await
    })
}

#[test]
fn redirects_cannot_move_an_authenticated_request_to_another_listener() -> TestResult {
    run(async {
        let foreign = TcpListener::bind("127.0.0.1:0").await?;
        let (mut port, task) = server(vec![Exchange {
            operation: "open",
            reply: Reply::Redirect(foreign.local_addr()?.port()),
        }])
        .await?;
        let result = port.open(request()?, profile()).await;
        // The denial must happen before any connection to the redirect target.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), foreign.accept())
                .await
                .is_err()
        );
        assert_eq!(result.err(), Some(malformed(false)));
        joined(task).await
    })
}

#[test]
fn partial_dismiss_never_accepts_foreign_or_duplicate_coordinates() -> TestResult {
    for ids in [vec!["msg-1"], vec!["foreign"], vec!["msg-1", "msg-1"]] {
        run(async {
            let valid = ids == vec!["msg-1"];
            let failure = json!({"ok":false,"failure":{"family":"TransportFailure","tag":"TransportUnavailable","dismissed":ids,"already_dismissed":[],"effect":"unknown-retry-exact"}});
            let (mut port, task) = server(vec![
                exchange("open", binding_value()),
                Exchange {
                    operation: "dismiss",
                    reply: Reply::Json(failure),
                },
            ])
            .await?;
            let binding = port.open(request()?, profile()).await?;
            let request = BulkDismissRequest::try_new(
                &binding,
                vec![RemoteMailboxMessageId::new("msg-1").ok_or("id")?],
                DismissReason::AlreadyHandled,
                64,
            )
            .map_err(|_| "request")?;
            let failure = port
                .dismiss(&binding, request)
                .await
                .err()
                .ok_or("failure")?;
            assert_eq!(failure.effect, WriteEffect::UnknownRetryExact);
            assert_eq!(failure.partial_dismiss.is_some(), valid);
            joined(task).await
        })?;
    }
    Ok(())
}

#[test]
fn nmv2_http_explicit_route_echo_mixed_page_and_v1_ack() -> TestResult {
    run(async {
        let mut opened = binding_value();
        opened["protocol"] = json!(REMOTE_SEAT_MAIL_TRANSPORT_V2);
        opened["adapter_version"] = json!("stateengine-native-mail/2");
        let page = json!({"messages":[row("msg-1","handoff","wake")],"refused_rows":[{"msg_id":null,"start":0,"end":10,"reason":"MalformedEnvelope","source_cursor":"ref1.integrity"}],"next_after":null,"last_cursor":"ref1.integrity"});
        let (mut port, task) = server_protocol(
            vec![
                exchange("open", opened),
                exchange("subscribe_and_snapshot", subscription_value()),
                exchange("fetch_snapshot_page", page),
                exchange("acknowledge_delivery", ack_value("posted")),
                exchange("close", json!({"joined_tasks":0,"ticks_used":0})),
            ],
            MailTransportVersion::V2,
        )
        .await?;
        let request = request()?;
        let request = OpenRequest::new(
            request.expected_local_project().clone(),
            request.expected_recipient().clone(),
            ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V2).ok_or("protocol")?,
            request.client_instance().clone(),
        );
        let binding = port.open(request, profile()).await?;
        assert_eq!(
            binding.receipt().protocol().as_str(),
            REMOTE_SEAT_MAIL_TRANSPORT_V2
        );
        let subscription = port
            .subscribe_and_snapshot(&binding, ResumeRequest::cold())
            .await?;
        let page = port
            .fetch_snapshot_page(
                &binding,
                subscription.snapshot(),
                None,
                PageLimits::from_settled(binding.receipt().limits()),
            )
            .await?;
        assert_eq!(page.entry_count(), 2);
        assert_eq!(
            page.messages()[0].protocol().as_str(),
            REMOTE_SEAT_MAIL_TRANSPORT_V1
        );
        assert!(page.refused_rows()[0].indexed_id().is_none());
        assert_eq!(
            page.last_cursor().map(|c| c.as_str()),
            Some("ref1.integrity")
        );
        assert!(!page.has_more());
        port.acknowledge_delivery(&binding, ack()?).await?;
        port.close(binding, ShutdownBudget::new(5000, 2)).await?;
        joined(task).await
    })
}

#[test]
fn invalid_headers_remain_terminal_without_reading_the_body() -> TestResult {
    for redirect in [false, true] {
        for held in [false, true] {
            run(async {
                let (mut port, task) = server(vec![Exchange {
                    operation: "open",
                    reply: Reply::InvalidHeaders { redirect, held },
                }])
                .await?;
                let failure = port
                    .open(request()?, profile())
                    .await
                    .err()
                    .ok_or("invalid headers accepted")?;
                joined(task).await?;
                assert_eq!(
                    failure.cause,
                    OperationCause::Connection(ConnectionFailure::InvalidEnvelope),
                    "invalid headers changed refusal: redirect={redirect}, held={held}"
                );
                assert_eq!(failure.effect, WriteEffect::None);
                Ok(())
            })?;
        }
    }
    Ok(())
}
