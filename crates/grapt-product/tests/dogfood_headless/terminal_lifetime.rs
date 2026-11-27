//! Normal terminals leave both sessions' subscriptions usable.
use super::*;

fn through_terminal(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    session: &str,
    stop: &AtomicBool,
) -> Result<(), String> {
    through_outcome(stream, bytes, session, "session.execution.succeeded", stop)
}

fn through_outcome(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    session: &str,
    outcome: &str,
    stop: &AtomicBool,
) -> Result<(), String> {
    stream
        .set_nonblocking(true)
        .map_err(|error| format!("SSE terminal nonblocking: {error}"))?;
    let mut chunk = [0u8; 8192];
    let wait = PollDeadline::new();
    while wait.pending() {
        if stop.load(Ordering::SeqCst) {
            return Err("SSE terminal stream cancelled".into());
        }
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("SSE terminal stream byte bound".into());
        }
        let found = String::from_utf8_lossy(bytes).lines().any(|line| {
            line.strip_prefix("data: ")
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .is_some_and(|event| {
                    event["type"] == outcome && event["data"]["sessionID"] == session
                })
        });
        if found {
            return Ok(());
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err("normal terminal closed another session's stream".into()),
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) => return Err(format!("SSE continuing stream read: {error}")),
        }
    }
    Err(format!("SSE terminal stream exceeded its deadline: {wait}"))
}

#[cfg(unix)]
fn live_text(
    receiver: &std::sync::mpsc::Receiver<String>,
    stop: &AtomicBool,
    allowance: Duration,
    name: &str,
) -> Result<String, String> {
    let wait = PollDeadline::within(allowance);
    while wait.pending() {
        if stop.load(Ordering::SeqCst) {
            return Err(format!("SSE live delta {name} cancelled"));
        }
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(text) => return Ok(text),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => return Err(format!("SSE live delta {name}: {error}")),
        }
    }
    Err(format!(
        "SSE live delta {name} exceeded its deadline: {wait}"
    ))
}

#[test]
fn two_sessions_complete_on_the_same_two_subscriptions() {
    two_subscriptions(Script::Success, "session.execution.succeeded");
}

#[test]
fn a_provider_refusal_keeps_the_other_sessions_two_subscriptions_live() {
    two_subscriptions(Script::Unauthorized, "session.execution.failed");
}

fn two_subscriptions(a_script: Script, a_outcome: &str) {
    let _journey = process_journey_custody();
    let root = root("terminal-two-sessions");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let entered = Arc::new(terminal_coordination::Phase::new());
    let release = Arc::new(terminal_coordination::Phase::new());
    let provider = ProviderFixture::start(vec![
        Script::HeldTerminalOutput {
            bytes: 1024,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        a_script,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let a = create(address);
    let b = create(address);
    let (mut first, mut first_bytes) = event_stream(address);
    let (mut second, mut second_bytes) = event_stream(address);
    json(
        address,
        "POST",
        &format!("/api/session/{b}/prompt"),
        r#"{"id":"msg_terminal_b","text":"held B","delivery":"queue","resume":true}"#,
    );
    entered
        .wait("held output entered", &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    json(
        address,
        "POST",
        &format!("/api/session/{a}/prompt"),
        r#"{"id":"msg_terminal_a","text":"complete A","delivery":"queue","resume":true}"#,
    );
    through_outcome(&mut first, &mut first_bytes, &a, a_outcome, &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    through_outcome(
        &mut second,
        &mut second_bytes,
        &a,
        a_outcome,
        &provider.stop,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let active = json(address, "GET", "/api/session/active", "");
    assert!(
        active["data"].get(&b).is_some(),
        "B was not running at A's terminal"
    );
    release.release();
    through_terminal(&mut first, &mut first_bytes, &b, &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    through_terminal(&mut second, &mut second_bytes, &b, &provider.stop)
        .unwrap_or_else(|error| panic!("{error}"));
    for bytes in [&first_bytes, &second_bytes] {
        assert!(
            String::from_utf8_lossy(bytes).lines().any(|line| {
                line.strip_prefix("data: ")
                    .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                    .is_some_and(|event| {
                        event["type"] == "session.text.delta" && event["data"]["sessionID"] == b
                    })
            }),
            "B lost its live text"
        );
    }
    let health = json(address, "GET", "/api/health", "");
    assert_eq!(
        health["global_reconciles"],
        serde_json::json!({
            "unpublished_advisory_notice": 0,
            "unrepresentable_frame": 0,
            "projection_failure": 0,
        }),
        "ordinary execution terminals must not request global recovery: {health}"
    );
    provider
        .settle_terminal_workers()
        .unwrap_or_else(|error| panic!("{error}"));
    child.stop_after_witness(&root);
    process_fixture::cleanup(&root);
}

#[cfg(unix)]
#[test]
fn an_unrelated_terminal_preserves_acp_live_text_before_its_own_terminal() {
    acp_terminal_case(false);
}

#[cfg(unix)]
#[test]
fn a_missing_phase_cleans_up_the_real_acp_and_product_pair() {
    acp_terminal_case(true);
}

#[cfg(unix)]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn acp_terminal_case(missing_phase: bool) {
    use super::acp_process::{now, AcpObserved, AcpProcess, ACP_FRAMES};
    let _journey = process_journey_custody();
    let root = root(if missing_phase {
        "terminal-acp-missing"
    } else {
        "terminal-acp-live"
    });
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    let advance = Arc::new(if missing_phase {
        terminal_coordination::Phase::within(Duration::from_millis(200))
    } else {
        terminal_coordination::Phase::new()
    });
    let finish = Arc::new(terminal_coordination::Phase::new());
    let provider = ProviderFixture::start(vec![
        Script::PhasedOutput {
            advance: Arc::clone(&advance),
            finish: Arc::clone(&finish),
        },
        Script::Success,
    ]);
    let mut acp = AcpProcess::start(&root, provider.address, ACP_FRAMES);
    let deadline = now() + Duration::from_secs(if missing_phase { 15 } else { 60 });
    let mut observed = AcpObserved::default();
    acp.request(
        1,
        "initialize",
        serde_json::json!({"protocolVersion":1,"clientCapabilities":{}}),
        deadline,
        &mut observed,
    )
    .unwrap_or_else(|error| panic!("ACP initialize: {error}"));
    let session = acp
        .request(
            2,
            "session/new",
            serde_json::json!({"cwd":root,"mcpServers":[]}),
            deadline,
            &mut observed,
        )
        .unwrap_or_else(|error| panic!("ACP new: {error}"));
    let b = session["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("ACP session missing"))
        .to_owned();
    let ready: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("serve-ready.json"))
            .unwrap_or_else(|error| panic!("ACP readiness: {error}")),
    )
    .unwrap_or_else(|error| panic!("ACP readiness JSON: {error}"));
    let address: SocketAddr = ready["observed_endpoint"]
        .as_str()
        .and_then(|endpoint| endpoint.strip_prefix("http://"))
        .and_then(|endpoint| endpoint.parse().ok())
        .unwrap_or_else(|| panic!("ACP endpoint missing"));
    let a = create(address);
    let (sent, received) = std::sync::mpsc::sync_channel(4);
    let cancellation = Arc::clone(&provider.stop);
    let mut controller = terminal_coordination::Worker::spawn(
        "ACP controller",
        Arc::clone(&provider.stop),
        move || {
            if missing_phase {
                return Ok(());
            }
            let before = live_text(
                &received,
                &cancellation,
                Duration::from_secs(15),
                "before A",
            )?;
            if before != "before A; " {
                return Err(format!("SSE ACP first live delta: {before}"));
            }
            let (mut events, mut bytes) = event_stream(address);
            json(
                address,
                "POST",
                &format!("/api/session/{a}/prompt"),
                r#"{"id":"msg_acp_unrelated_a","text":"finish A","delivery":"queue","resume":true}"#,
            );
            through_terminal(&mut events, &mut bytes, &a, &cancellation)?;
            let active = json(address, "GET", "/api/session/active", "");
            assert!(
                active["data"].get(&b).is_some(),
                "ACP B was no longer running"
            );
            // Let an erroneous rebind finish before B's next text is published.
            thread::sleep(Duration::from_millis(200));
            advance.release();
            let after = live_text(&received, &cancellation, Duration::from_secs(5), "after A");
            // Release even when the second live delta was suppressed, so a red
            // witness still finishes and cleans up the real ACP/product pair.
            finish.release();
            if after.as_deref() == Ok("after A; ") {
                Ok(())
            } else {
                Err(format!(
                    "unrelated terminal suppressed ACP live text until its own terminal: {after:?}"
                ))
            }
        },
    );
    let response = acp.request_observing(3, "session/prompt",
        serde_json::json!({"sessionId":session["sessionId"],"prompt":[{"type":"text","text":"B streams across A"}]}),
        deadline, &mut observed, |row| {
            if row["method"] == "session/update" && row["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                if let Some(text) = row["params"]["update"]["content"]["text"].as_str() {
                    let _ = sent.try_send(text.to_owned());
                }
            }
        });
    if response.is_err() {
        provider.stop.store(true, Ordering::SeqCst);
    }
    let controlled = controller.finish();
    acp.cleanup();
    let cleaned_up = acp.cleaned_up;
    provider.stop.store(true, Ordering::SeqCst);
    let workers = provider.settle_terminal_workers();
    drop(controller);
    drop(provider);
    if missing_phase {
        process_fixture::force_cleanup(&root);
    } else {
        process_fixture::cleanup(&root);
    }
    assert!(cleaned_up, "ACP terminal witness left a process");
    controlled.unwrap_or_else(|error| panic!("{error}"));
    if missing_phase {
        assert_eq!(
            workers,
            Err("SSE phase advance exceeded its 200ms deadline".into())
        );
        assert!(!root.exists(), "missing-phase root was not cleaned");
        eprintln!("NAMED_MISSING_PHASE_REAL_PAIR: {workers:?}; ACP/product cleaned; response={response:?}");
        return;
    }
    workers.unwrap_or_else(|error| panic!("{error}"));
    let response = response.unwrap_or_else(|error| panic!("ACP terminal response: {error}"));
    assert_eq!(response["stopReason"], "end_turn");
    let answer = observed
        .updates
        .iter()
        .zip(&observed.texts)
        .filter(|(kind, _)| *kind == "agent_message_chunk")
        .map(|(_, text)| text.as_str())
        .collect::<String>();
    assert_eq!(answer, "before A; after A; ", "ACP duplicated or lost text");
}

#[test]
fn a_missing_provider_phase_is_named_and_its_worker_is_settled() {
    let advance = Arc::new(terminal_coordination::Phase::within(Duration::from_millis(
        100,
    )));
    let finish = Arc::new(terminal_coordination::Phase::within(Duration::from_millis(
        100,
    )));
    let provider = ProviderFixture::start(vec![Script::PhasedOutput { advance, finish }]);
    let mut stream = TcpStream::connect_timeout(&provider.address, Duration::from_secs(2))
        .unwrap_or_else(|error| panic!("missing phase connect: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap_or_else(|error| panic!("missing phase read timeout: {error}"));
    stream
        .write_all(b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}")
        .unwrap_or_else(|error| panic!("missing phase request: {error}"));
    let mut bytes = Vec::new();
    stream
        .take(4096)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|error| panic!("missing phase EOF: {error}"));
    assert!(
        String::from_utf8_lossy(&bytes).contains("before A; "),
        "provider never entered the real phase"
    );
    let failure = provider.settle_terminal_workers();
    assert_eq!(
        failure,
        Err("SSE phase advance exceeded its 100ms deadline".into())
    );
    eprintln!("NAMED_MISSING_PHASE: {failure:?}; owned provider worker settled");
    drop(provider);
}
