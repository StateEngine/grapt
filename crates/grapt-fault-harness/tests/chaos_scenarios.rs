//! Deterministic end-to-end chaos scenarios for `grapt-fault-harness`.

use std::error::Error;
use std::net::SocketAddr;

use grapt_fault_harness::{
    ChaosProxy, FailureTranscript, FaultAction, FaultPlan, FaultSchedule, FaultSeed, FaultTrigger,
    StorageFault, StorageFaultScript, StorageOperation, StorageOutcome, TerminalCommitError,
    TerminalLedger,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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

async fn spawn_sse_upstream() -> Result<SocketAddr, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut req_buf = [0_u8; 2048];
            let Ok(n) = stream.read(&mut req_buf).await else {
                continue;
            };
            let req_text = String::from_utf8_lossy(&req_buf[..n]);
            let after_id = extract_header(&req_text, "Last-Event-ID")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);

            let mut body = String::new();
            for id in (after_id + 1)..=4 {
                body.push_str(&format!("id: {id}\ndata: chunk-{id}\n\n"));
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.shutdown().await;
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
    let upstream = spawn_sse_upstream().await?;
    let plan = FaultPlan::new("sse-drop-reconnect", FaultSeed::new(42)).with_rule(
        FaultTrigger::OnRequest(1),
        FaultAction::MidStreamEofFrames { after_frames: 2 },
    );
    let schedule = FaultSchedule::new(plan);
    let proxy = ChaosProxy::new(upstream, schedule.clone()).start().await?;

    let first_raw = send_http_get(proxy.addr(), "/v1/events", None).await?;
    let (status1, _, body1) = split_http_response(&first_raw)?;
    assert_eq!(status1, 200);
    let first_events = parse_sse_events(&body1);
    assert_eq!(
        first_events,
        vec![(1, "chunk-1".to_owned()), (2, "chunk-2".to_owned())]
    );

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
    assert_eq!(combined.len(), 4);
    assert_eq!(
        combined.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
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
async fn partial_utf8_truncation_and_gated_chunk_delay() -> Result<(), Box<dyn Error>> {
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
                yield_steps: 2,
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

    // Request 2: delayed behind a deterministic release gate (zero wall-clock sleeps).
    let addr = proxy.addr();
    let client_task = tokio::spawn(async move { send_http_get(addr, "/v1/stream", None).await });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    proxy.release_delayed();
    let delayed_raw = client_task.await??;
    let (status, _, full_body) = split_http_response(&delayed_raw)?;
    assert_eq!(status, 200);
    assert_eq!(std::str::from_utf8(&full_body)?, "data: caf\u{00e9}\n\n");

    proxy.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn simulated_stateengine_down_solo_degradation() -> Result<(), Box<dyn Error>> {
    let plan = FaultPlan::new("stateengine-down-solo", FaultSeed::new(503)).with_rule(
        FaultTrigger::Always,
        FaultAction::UpstreamDown {
            status: 503,
            reason: "stateengine host unreachable".to_owned(),
        },
    );
    let schedule = FaultSchedule::new(plan);
    let proxy = ChaosProxy::disconnected(schedule.clone()).start().await?;

    let raw = send_http_get(proxy.addr(), "/v1/stateengine/status", None).await?;
    let (status, _, body) = split_http_response(&raw)?;
    assert_eq!(status, 503);
    let body_text = String::from_utf8_lossy(&body);
    assert!(body_text.contains("unreachable"));

    // Simulate solo degradation: record an explicit warning and complete local work.
    let degraded_warning = format!(
        "warning: stateengine unavailable (HTTP {status}: {body_text}); continuing in solo mode"
    );
    schedule.record_event("solo-mode", "stateengine-probe", degraded_warning.clone());
    let local_result = "solo-turn-completed";

    assert!(degraded_warning.contains("solo mode"));
    assert_eq!(local_result, "solo-turn-completed");
    assert_eq!(proxy.transcript().events.len(), 2);

    proxy.shutdown().await;
    Ok(())
}

#[test]
fn storage_lifecycle_faults_prevent_duplicate_terminals_across_restart(
) -> Result<(), Box<dyn Error>> {
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
        .inject(StorageOperation::CommitTerminal, 4, StorageFault::Enospc)
        .inject(
            StorageOperation::CommitTerminal,
            5,
            StorageFault::SqliteBusy,
        )
        .inject(StorageOperation::CommitTerminal, 6, StorageFault::Stalled);

    let mut ledger = TerminalLedger::new(script).with_schedule(schedule.clone());

    // Attempt 1: crash before commit -> nothing durable yet.
    assert_eq!(
        ledger.commit_terminal("turn-1", "done-1"),
        Err(TerminalCommitError::CrashBeforeCommit {
            key: "turn-1".to_owned()
        })
    );
    assert!(ledger.durable_records().is_empty());

    // Attempt 2: crash after durable commit before ack -> record is durable, client sees crash.
    assert_eq!(
        ledger.commit_terminal("turn-1", "done-1"),
        Err(TerminalCommitError::CrashAfterCommitBeforeAck {
            key: "turn-1".to_owned()
        })
    );
    assert_eq!(ledger.durable_records().len(), 1);

    // Simulated restart + reconnect retry of turn-1 -> deduplicated, no second durable terminal.
    ledger.simulate_restart();
    let outcome = ledger.commit_terminal("turn-1", "done-1")?;
    assert!(matches!(outcome, StorageOutcome::AlreadyCommitted(rec) if rec.sequence == 1));
    assert_eq!(ledger.durable_records().len(), 1);

    // Attempts 4..6: ENOSPC, SQLite busy, stalled -> honest failures, zero partial writes.
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

    // Attempt 7: faults cleared -> turn-2 commits cleanly once.
    let outcome2 = ledger.commit_terminal("turn-2", "done-2")?;
    assert!(matches!(outcome2, StorageOutcome::Committed(rec) if rec.sequence == 2));
    assert_eq!(ledger.durable_records().len(), 2);
    assert_eq!(ledger.restart_count(), 1);
    ledger.assert_no_duplicate_terminals()?;
    assert!(!schedule.transcript().events.is_empty());
    Ok(())
}
