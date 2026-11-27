//! Deterministic end-to-end chaos scenarios for `grapt-fault-harness`.

use std::error::Error;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;

use grapt_fault_harness::{
    ChaosProxy, FailureTranscript, FaultAction, FaultPlan, FaultSchedule, FaultSeed, FaultTrigger,
    StorageError, StorageFault, StorageFaultScript, StorageOperation, StorageOutcome,
    TerminalCommitError, TerminalLedger,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

async fn send_http_get(
    addr: SocketAddr,
    path: &str,
    extra_header: Option<(&str, &str)>,
) -> std::io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect(addr).await?;
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    if let Some((k, v)) = extra_header {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok(response)
}

fn split_http_response(raw: &[u8]) -> Result<(u16, String, Vec<u8>), Box<dyn Error>> {
    let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Err("missing HTTP header terminator".into());
    };
    let headers = String::from_utf8_lossy(&raw[..pos]).into_owned();
    let status_line = headers.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or("missing HTTP status code")?
        .parse::<u16>()?;
    let body = raw[pos + 4..].to_vec();
    Ok((status, headers, body))
}

fn extract_header(headers: &str, name: &str) -> Option<String> {
    for line in headers.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case(name) {
                return Some(v.trim().to_owned());
            }
        }
    }
    None
}

fn parse_sse_events(body: &[u8]) -> Vec<(u64, String)> {
    let text = String::from_utf8_lossy(body);
    let mut events = Vec::new();
    for frame in text.split("\n\n") {
        let mut id = None;
        let mut data = None;
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("id: ") {
                id = rest.trim().parse::<u64>().ok();
            } else if let Some(rest) = line.strip_prefix("data: ") {
                data = Some(rest.to_owned());
            }
        }
        if let (Some(id), Some(data)) = (id, data) {
            events.push((id, data));
        }
    }
    events
}

/// Live SSE upstream that holds the first stream open after frame 2 until `release` fires.
///
/// Proves the proxy streams frames live and triggers `MidStreamEofFrames` at the frame-2
/// boundary without buffering the full response or waiting for upstream to close.
async fn spawn_live_sse_upstream(release_tail: Arc<Notify>) -> Result<SocketAddr, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let release = Arc::clone(&release_tail);
            tokio::spawn(async move {
                let mut req_buf = [0_u8; 2048];
                let Ok(n) = stream.read(&mut req_buf).await else {
                    return;
                };
                let req_text = String::from_utf8_lossy(&req_buf[..n]);
                let after_id = extract_header(&req_text, "Last-Event-ID")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);

                let headers =
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
                if stream.write_all(headers.as_bytes()).await.is_err() {
                    return;
                }
                for id in (after_id + 1)..=4 {
                    let frame = format!("id: {id}\ndata: chunk-{id}\n\n");
                    if stream.write_all(frame.as_bytes()).await.is_err()
                        || stream.flush().await.is_err()
                    {
                        return;
                    }
                    if after_id == 0 && id == 2 {
                        // Hold open until test releases; live proxy must EOF at frame 2 anyway.
                        release.notified().await;
                    }
                }
                let _ = stream.shutdown().await;
            });
        }
    });
    Ok(addr)
}

async fn spawn_static_upstream(payload: &'static str) -> Result<SocketAddr, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut req_buf = [0_u8; 1024];
            let _ = stream.read(&mut req_buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    Ok(addr)
}

fn run_seeded_plan_once(seed: FaultSeed) -> FailureTranscript {
    let plan = FaultPlan::seeded_http_chaos("repro-check", seed, 6);
    let schedule = FaultSchedule::new(plan);
    for idx in 1..=6 {
        let _ = schedule.next_request_action("GET", &format!("/v1/step/{idx}"));
    }
    schedule.record_failure("synthetic-chaos-failure")
}

#[test]
fn seed_reproducibility_and_transcript_replay() -> Result<(), Box<dyn Error>> {
    let seed = FaultSeed::new(0x0000_5eed_cafe_1234);
    let first = run_seeded_plan_once(seed);
    let second = run_seeded_plan_once(seed);
    let different = run_seeded_plan_once(FaultSeed::new(0x0000_5eed_cafe_9999));

    assert_eq!(first, second);
    assert_eq!(first.to_json()?, second.to_json()?);
    assert_eq!(first.replay_banner(), second.replay_banner());
    assert!(first.replay_banner().contains(&seed.as_u64().to_string()));
    assert_ne!(first.events, different.events);
    Ok(())
}

#[tokio::test]
async fn sse_mid_stream_drop_and_cursor_reconnect() -> Result<(), Box<dyn Error>> {
    let release_tail = Arc::new(Notify::new());
    let upstream = spawn_live_sse_upstream(Arc::clone(&release_tail)).await?;
    let plan = FaultPlan::new("sse-drop-reconnect", FaultSeed::new(42)).with_rule(
        FaultTrigger::OnRequest(1),
        FaultAction::MidStreamEofFrames { after_frames: 2 },
    );
    let schedule = FaultSchedule::new(plan);
    let proxy = ChaosProxy::new(upstream, schedule.clone()).start().await?;

    // Request 1 returns EOF immediately after frame 2 even though upstream is still open!
    let first_raw = send_http_get(proxy.addr(), "/v1/events", None).await?;
    let (status1, _, body1) = split_http_response(&first_raw)?;
    assert_eq!(status1, 200);
    let first_events = parse_sse_events(&body1);
    assert_eq!(
        first_events,
        vec![(1, "chunk-1".to_owned()), (2, "chunk-2".to_owned())]
    );

    release_tail.notify_waiters();
    let last_id = first_events.last().map_or(0, |(id, _)| *id);
    schedule.record_event(
        "client",
        "sse-reconnect",
        format!("Last-Event-ID={last_id}"),
    );

    let second_raw =
        send_http_get(proxy.addr(), "/v1/events", Some(("Last-Event-ID", "2"))).await?;
    let (status2, _, body2) = split_http_response(&second_raw)?;
    assert_eq!(status2, 200);
    let second_events = parse_sse_events(&body2);
    assert_eq!(
        second_events,
        vec![(3, "chunk-3".to_owned()), (4, "chunk-4".to_owned())]
    );

    let mut combined = first_events;
    combined.extend(second_events);
    assert_eq!(
        combined.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );

    proxy.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn mid_stream_rst_delivers_connection_reset_not_eof() -> Result<(), Box<dyn Error>> {
    let upstream = spawn_static_upstream("0123456789abcdef").await?;
    let plan = FaultPlan::new("mid-stream-rst", FaultSeed::new(88)).with_rule(
        FaultTrigger::OnRequest(1),
        FaultAction::MidStreamRst { after_bytes: 4 },
    );
    let proxy = ChaosProxy::new(upstream, FaultSchedule::new(plan))
        .start()
        .await?;

    let mut stream = TcpStream::connect(proxy.addr()).await?;
    stream
        .write_all(b"GET /v1/stream HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await?;

    let mut buf = Vec::new();
    let err = match stream.read_to_end(&mut buf).await {
        Ok(n) => {
            return Err(format!("expected TCP RST error, got clean EOF after {n} bytes").into())
        }
        Err(err) => err,
    };
    assert!(
        matches!(
            err.kind(),
            ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe
        ),
        "expected connection reset error, got {:?}",
        err.kind()
    );

    proxy.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn http_429_retry_after_chaos_replay() -> Result<(), Box<dyn Error>> {
    let upstream = spawn_static_upstream("ok-after-retries").await?;
    let seed = FaultSeed::new(777);

    let run_once = || async {
        let plan = FaultPlan::new("retry-after-429", seed)
            .with_rule(
                FaultTrigger::OnRequest(1),
                FaultAction::InjectHttpStatus {
                    status: 429,
                    retry_after_secs: Some(3),
                    body: "rate limited".to_owned(),
                },
            )
            .with_rule(
                FaultTrigger::OnRequest(2),
                FaultAction::InjectHttpStatus {
                    status: 529,
                    retry_after_secs: Some(5),
                    body: "overloaded".to_owned(),
                },
            );
        let schedule = FaultSchedule::new(plan);
        let proxy = ChaosProxy::new(upstream, schedule).start().await?;

        let r1 = split_http_response(&send_http_get(proxy.addr(), "/v1/turns", None).await?)?;
        let r2 = split_http_response(&send_http_get(proxy.addr(), "/v1/turns", None).await?)?;
        let r3 = split_http_response(&send_http_get(proxy.addr(), "/v1/turns", None).await?)?;

        assert_eq!(r1.0, 429);
        assert_eq!(extract_header(&r1.1, "retry-after").as_deref(), Some("3"));
        assert_eq!(r2.0, 529);
        assert_eq!(extract_header(&r2.1, "retry-after").as_deref(), Some("5"));
        assert_eq!(r3.0, 200);
        assert_eq!(String::from_utf8_lossy(&r3.2), "ok-after-retries");

        let transcript = proxy.transcript();
        proxy.shutdown().await;
        Ok::<FailureTranscript, Box<dyn Error>>(transcript)
    };

    let t1 = run_once().await?;
    let t2 = run_once().await?;
    assert_eq!(t1, t2);
    assert_eq!(t1.events.len(), 3);
    Ok(())
}

#[tokio::test]
async fn partial_utf8_truncation_and_deterministic_delay_gate() -> Result<(), Box<dyn Error>> {
    // "data: café\n\n" -> 'é' is 2 UTF-8 bytes (0xc3, 0xa9) at byte offsets 9..11.
    let upstream = spawn_static_upstream("data: caf\u{00e9}\n\n").await?;
    let plan = FaultPlan::new("utf8-and-delay", FaultSeed::new(909))
        .with_rule(
            FaultTrigger::OnRequest(1),
            FaultAction::TruncateBody { keep_bytes: 10 },
        )
        .with_rule(
            FaultTrigger::OnRequest(2),
            FaultAction::DelayChunk {
                after_bytes: 6,
                yield_steps: 0,
                wait_for_gate: true,
            },
        )
        .with_rule(
            FaultTrigger::OnRequest(3),
            FaultAction::DelayChunk {
                after_bytes: 6,
                yield_steps: 0,
                wait_for_gate: true,
            },
        );
    let proxy = ChaosProxy::new(upstream, FaultSchedule::new(plan))
        .start()
        .await?;

    // Request 1: truncated mid-UTF-8 codepoint must be detected honestly as invalid UTF-8.
    let (_, _, truncated_body) =
        split_http_response(&send_http_get(proxy.addr(), "/v1/stream", None).await?)?;
    assert_eq!(truncated_body.len(), 10);
    assert!(std::str::from_utf8(&truncated_body).is_err());

    // Request 2: await exact gate entry (zero yield guessing), then release.
    let addr = proxy.addr();
    let client_task = tokio::spawn(async move { send_http_get(addr, "/v1/stream", None).await });
    proxy.wait_until_delayed().await;
    proxy.release_delayed();
    let (status, _, full_body) = split_http_response(&client_task.await??)?;
    assert_eq!(status, 200);
    assert_eq!(std::str::from_utf8(&full_body)?, "data: caf\u{00e9}\n\n");

    // Request 3: pre-arm release BEFORE request arrives — stateful semaphore never loses it.
    proxy.release_delayed();
    let (status3, _, full_body3) =
        split_http_response(&send_http_get(proxy.addr(), "/v1/stream", None).await?)?;
    assert_eq!(status3, 200);
    assert_eq!(std::str::from_utf8(&full_body3)?, "data: caf\u{00e9}\n\n");

    proxy.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn shutdown_aborts_and_joins_in_flight_handlers() -> Result<(), Box<dyn Error>> {
    let upstream = spawn_static_upstream("head-and-tail-payload").await?;
    let plan = FaultPlan::new("shutdown-joins", FaultSeed::new(314)).with_rule(
        FaultTrigger::OnRequest(1),
        FaultAction::DelayChunk {
            after_bytes: 4,
            yield_steps: 0,
            wait_for_gate: true,
        },
    );
    let proxy = ChaosProxy::new(upstream, FaultSchedule::new(plan))
        .start()
        .await?;

    let addr = proxy.addr();
    let client_task = tokio::spawn(async move { send_http_get(addr, "/v1/slow", None).await });
    proxy.wait_until_delayed().await;

    // Shut down while handler is parked at the gate; it must be aborted before shutdown returns.
    proxy.shutdown().await;

    let raw = client_task.await??;
    let (_, _, body) = split_http_response(&raw)?;
    assert_eq!(body, b"head");
    Ok(())
}

async fn probe_and_run_solo_turn(
    stateengine_addr: SocketAddr,
    ledger: &mut TerminalLedger,
    schedule: &FaultSchedule,
    turn_key: &str,
) -> Result<(Option<String>, StorageOutcome), Box<dyn Error>> {
    let raw = send_http_get(stateengine_addr, "/v1/stateengine/status", None).await?;
    let (status, _, body) = split_http_response(&raw)?;
    let warning = if status == 200 {
        None
    } else {
        let reason = String::from_utf8_lossy(&body).into_owned();
        let msg = format!(
            "warning: stateengine unavailable (HTTP {status}: {reason}); continuing in solo mode"
        );
        schedule.record_event("solo-mode", "stateengine-probe", msg.clone());
        Some(msg)
    };
    let outcome = ledger.commit_terminal(turn_key, "solo-turn-completed")?;
    Ok((warning, outcome))
}

#[tokio::test]
async fn simulated_stateengine_down_solo_degradation() -> Result<(), Box<dyn Error>> {
    // Case A: ChaosProxy explicitly simulates UpstreamDown (503).
    let seed = FaultSeed::new(503);
    let plan = FaultPlan::new("stateengine-down-solo", seed).with_rule(
        FaultTrigger::Always,
        FaultAction::UpstreamDown {
            status: 503,
            reason: "stateengine host unreachable".to_owned(),
        },
    );
    let schedule = FaultSchedule::new(plan);
    let proxy = ChaosProxy::disconnected(schedule.clone()).start().await?;
    let mut ledger =
        TerminalLedger::new(StorageFaultScript::new(seed)).with_schedule(schedule.clone());

    let (warning, outcome) =
        probe_and_run_solo_turn(proxy.addr(), &mut ledger, &schedule, "solo-turn-1").await?;
    let warning_text = warning.ok_or("expected explicit degraded warning on 503")?;
    assert!(warning_text.contains("HTTP 503"));
    assert!(warning_text.contains("solo mode"));
    assert!(
        matches!(outcome, StorageOutcome::Committed(ref r) if r.payload == "solo-turn-completed")
    );
    ledger.assert_no_duplicate_terminals()?;
    proxy.shutdown().await;

    // Case B: ChaosProxy points at a closed upstream port -> real HTTP 502 from connection refusal.
    let closed_listener = TcpListener::bind("127.0.0.1:0").await?;
    let closed_addr = closed_listener.local_addr()?;
    drop(closed_listener);

    let proxy2 = ChaosProxy::new(
        closed_addr,
        FaultSchedule::new(FaultPlan::new("stateengine-refused", FaultSeed::new(502))),
    )
    .start()
    .await?;
    let (warning2, outcome2) =
        probe_and_run_solo_turn(proxy2.addr(), &mut ledger, &schedule, "solo-turn-2").await?;
    let warning2_text = warning2.ok_or("expected explicit degraded warning on 502")?;
    assert!(warning2_text.contains("HTTP 502"));
    assert!(matches!(outcome2, StorageOutcome::Committed(ref r) if r.sequence == 2));
    ledger.assert_no_duplicate_terminals()?;
    proxy2.shutdown().await;
    Ok(())
}

#[test]
fn storage_lifecycle_faults_catch_duplicates_and_verify_reconciled_recovery(
) -> Result<(), Box<dyn Error>> {
    // Negative proof: a naive caller that blindly retries write_terminal_raw after
    // CrashAfterCommitBeforeAck produces a duplicate durable terminal, and the assertion fails.
    let naive_script = StorageFaultScript::new(FaultSeed::new(100)).inject(
        StorageOperation::CommitTerminal,
        1,
        StorageFault::CrashAfterCommitBeforeAck,
    );
    let mut naive_ledger = TerminalLedger::new(naive_script);
    assert_eq!(
        naive_ledger.write_terminal_raw("dup-turn", "done"),
        Err(TerminalCommitError::CrashAfterCommitBeforeAck {
            key: "dup-turn".to_owned()
        })
    );
    naive_ledger.simulate_restart();
    let _ = naive_ledger.write_terminal_raw("dup-turn", "done")?;
    assert_eq!(naive_ledger.durable_records().len(), 2);
    assert_eq!(
        naive_ledger.assert_no_duplicate_terminals(),
        Err(StorageError::DuplicateTerminal {
            key: "dup-turn".to_owned()
        })
    );

    // Positive proof: reconciling durable state across restart prevents duplicates under all faults.
    let seed = FaultSeed::new(2026);
    let schedule = FaultSchedule::new(FaultPlan::new("storage-lifecycle", seed));
    let script = StorageFaultScript::new(seed)
        .inject(
            StorageOperation::CommitTerminal,
            1,
            StorageFault::CrashBeforeCommit,
        )
        .inject(
            StorageOperation::CommitTerminal,
            2,
            StorageFault::CrashAfterCommitBeforeAck,
        )
        .inject(StorageOperation::CommitTerminal, 3, StorageFault::Enospc)
        .inject(
            StorageOperation::CommitTerminal,
            4,
            StorageFault::SqliteBusy,
        )
        .inject(StorageOperation::CommitTerminal, 5, StorageFault::Stalled);

    let mut ledger = TerminalLedger::new(script).with_schedule(schedule.clone());

    assert_eq!(
        ledger.commit_terminal("turn-1", "done-1"),
        Err(TerminalCommitError::CrashBeforeCommit {
            key: "turn-1".to_owned()
        })
    );
    assert!(ledger.durable_records().is_empty());

    assert_eq!(
        ledger.commit_terminal("turn-1", "done-1"),
        Err(TerminalCommitError::CrashAfterCommitBeforeAck {
            key: "turn-1".to_owned()
        })
    );
    assert_eq!(ledger.durable_records().len(), 1);

    ledger.simulate_restart();
    let outcome = ledger.commit_terminal("turn-1", "done-1")?;
    assert!(matches!(outcome, StorageOutcome::AlreadyCommitted(rec) if rec.sequence == 1));
    assert_eq!(ledger.durable_records().len(), 1);

    assert!(matches!(
        ledger.commit_terminal("turn-2", "done-2"),
        Err(TerminalCommitError::Enospc { .. })
    ));
    assert!(matches!(
        ledger.commit_terminal("turn-2", "done-2"),
        Err(TerminalCommitError::SqliteBusy { .. })
    ));
    assert!(matches!(
        ledger.commit_terminal("turn-2", "done-2"),
        Err(TerminalCommitError::Stalled { .. })
    ));
    assert_eq!(ledger.durable_records().len(), 1);

    let outcome2 = ledger.commit_terminal("turn-2", "done-2")?;
    assert!(matches!(outcome2, StorageOutcome::Committed(rec) if rec.sequence == 2));
    assert_eq!(ledger.durable_records().len(), 2);
    assert_eq!(ledger.restart_count(), 1);
    ledger.assert_no_duplicate_terminals()?;
    assert!(!schedule.transcript().events.is_empty());
    Ok(())
}
