//! SUSTAINED-RECOVERY-02 H03: the ordinary product owns retry and terminal
//! decisions across the bounded experiment relay and permission driver.

use super::*;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

struct RelayProcess(Option<Child>);

impl RelayProcess {
    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            child
                .kill()
                .unwrap_or_else(|error| panic!("relay stop failed: {error}"));
            child
                .wait()
                .unwrap_or_else(|error| panic!("relay reap failed: {error}"));
        }
    }
}

impl Drop for RelayProcess {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn recovery_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("scripts/qualification/sustained-recovery")
        .join(name)
}

fn start_relay(root: &Path, upstream: SocketAddr) -> (RelayProcess, SocketAddr, PathBuf) {
    let reservation = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("relay port reservation failed: {error}"));
    let address = reservation
        .local_addr()
        .unwrap_or_else(|error| panic!("relay reserved address failed: {error}"));
    drop(reservation);
    let log = root.join("relay.jsonl");
    let (mut ready, output) = UnixStream::pair()
        .unwrap_or_else(|error| panic!("relay readiness channel failed: {error}"));
    ready
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap_or_else(|error| panic!("relay readiness guard failed: {error}"));
    let child = Command::new("python3")
        .arg(recovery_script("request_gate.py"))
        .arg("serve")
        .arg("--plain-http")
        .arg("--announce-ready")
        .arg("--listen")
        .arg(address.ip().to_string())
        .arg("--port")
        .arg(address.port().to_string())
        .arg("--upstream-host")
        .arg(upstream.ip().to_string())
        .arg("--upstream-port")
        .arg(upstream.port().to_string())
        .arg("--transport-timeout")
        .arg("0.05")
        .arg("--maximum-calls")
        .arg("8")
        .arg("--maximum-seconds")
        .arg("20")
        .arg("--model")
        .arg("gpt-live")
        .arg("--log")
        .arg(&log)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdout(Stdio::from(OwnedFd::from(output)))
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|error| panic!("relay spawn failed: {error}"));
    let relay = RelayProcess(Some(child));
    // Startup is complete only when Python has actually bound the relay.
    // The timeout bounds a broken fixture; it is not a polling budget.
    let mut signal = [0_u8; b"RELAY_READY\n".len()];
    if let Err(error) = ready.read_exact(&mut signal) {
        drop(relay);
        panic!("relay bind readiness was not announced: {error}");
    }
    assert_eq!(&signal, b"RELAY_READY\n", "invalid relay bind readiness");
    (relay, address, log)
}

fn drive_permission(address: SocketAddr, root: &Path, session: &str) {
    let wait = PollDeadline::new();
    while wait.pending() {
        let page = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        if page["data"].as_array().is_some_and(|rows| !rows.is_empty()) {
            let output = Command::new("python3")
                .arg(recovery_script("permission_driver.py"))
                .arg("decide")
                .arg("--prefix")
                .arg(format!("http://{address}/api/session/{session}"))
                .arg("--database")
                .arg(root.join("database.sqlite3"))
                .arg("--record")
                .arg(root.join("permission-driver.jsonl"))
                .arg("--reply")
                .arg("once")
                .env("GRAPT_DRIVER_AUTHORIZATION", AUTHORIZATION)
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .output()
                .unwrap_or_else(|error| panic!("permission driver failed to start: {error}"));
            assert!(
                output.status.success(),
                "permission driver failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("\"decided\": true"));
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let relay = fs::read_to_string(root.join("relay.jsonl"))
        .unwrap_or_else(|error| format!("relay evidence unavailable: {error}"));
    panic!("permission driver saw no fresh request before its deadline ({wait}); relay={relay}")
}

#[cfg(not(windows))]
#[test]
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn sr_h03_ordinary_product_owns_relay_retry_terminal_and_permission() {
    let _journey = process_journey_custody();
    let root = root("sr-relay-product-recovery");
    prepare(&root, 120_000);
    attended(&root);
    prepare_model_inventory(&root, 128_000);
    fs::write(root.join("tool.txt"), b"permission-mediated relay recovery")
        .unwrap_or_else(|error| panic!("relay fixture write failed: {error}"));
    let partial_entered = Arc::new(AtomicBool::new(false));
    let partial_release = Arc::new(AtomicBool::new(false));
    let provider = ProviderFixture::start(vec![
        Script::DisconnectBeforeHead,
        // The Author preset asks before replacing a file, so the
        // external permission driver has a request to decide.
        write_call(
            "call_sr_relay_read",
            "tool.txt",
            "replaced through the relay\n",
        ),
        Script::SuccessText("product retried once and continued through the approved tool"),
        Script::StallBeforeFirstFragment,
        Script::SuccessText("product recovered from the relay head timeout"),
        Script::PartialThenHeldDisconnect {
            entered: Arc::clone(&partial_entered),
            release: Arc::clone(&partial_release),
        },
        Script::SuccessText("the retry answered after the unfinished partial"),
    ]);
    let (mut relay, relay_address, relay_log) = start_relay(&root, provider.address);
    let mut child = spawn(&root, relay_address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");

    let recovered = json(address, "POST", "/api/session", "{}");
    let recovered = recovered["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing retry Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{recovered}/prompt"),
        r#"{"id":"msg_sr_relay_retry","text":"recover through the relay and read tool.txt","delivery":"queue","resume":true}"#,
    );
    drive_permission(address, &root, recovered);
    permission_residuals::wait_session_idle(address, recovered);
    let messages = json(
        address,
        "GET",
        &format!("/api/session/{recovered}/message"),
        "",
    );
    assert!(messages
        .to_string()
        .contains("product retried once and continued through the approved tool"));

    let timed = json(address, "POST", "/api/session", "{}");
    let timed = timed["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing timeout Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{timed}/prompt"),
        r#"{"id":"msg_sr_relay_timeout","text":"recover from one response-head timeout","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, timed);
    let messages = json(address, "GET", &format!("/api/session/{timed}/message"), "");
    assert!(messages
        .to_string()
        .contains("product recovered from the relay head timeout"));

    let partial = json(address, "POST", "/api/session", "{}");
    let partial = partial["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing partial Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{partial}/prompt"),
        r#"{"id":"msg_sr_relay_partial","text":"retry after an unfinished partial","delivery":"queue","resume":true}"#,
    );
    let wait = PollDeadline::new();
    while wait.pending() {
        if partial_entered.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let partial_dispatched = partial_entered.load(Ordering::SeqCst);
    // Drop the stream as soon as the partial is out. The fixture serves one
    // connection at a time, so the retry this drop now earns (SC-REL-02b)
    // can only be served once the held connection is gone.
    partial_release.store(true, Ordering::SeqCst);
    assert!(
        partial_dispatched,
        "fixture saw no partial-response dispatch before its deadline ({wait})"
    );
    permission_residuals::wait_session_idle(address, partial);
    // SC-REL-02b: the partial was streamed but never finished, so the drop
    // retries. The unfinished text is not durable, and the retry
    // is built from the same history: its request is the first one, byte for
    // byte.
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        7,
        "the dropped partial must be retried exactly once"
    );
    let partial_messages = json(
        address,
        "GET",
        &format!("/api/session/{partial}/message"),
        "",
    )
    .to_string();
    assert!(
        !partial_messages.contains("visible partial"),
        "{partial_messages}"
    );
    assert!(partial_messages.contains("the retry answered after the unfinished partial"));
    assert!(
        !partial_messages.contains("executionFailure"),
        "{partial_messages}"
    );
    {
        let bodies = provider
            .bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(bodies.len(), 7);
        assert_eq!(
            bodies[5], bodies[6],
            "the retry must resend the dropped attempt's request unchanged"
        );
    }

    child.stop_after_witness(&root);
    drop(child);
    relay.stop();
    let records: Vec<serde_json::Value> = fs::read_to_string(&relay_log)
        .unwrap_or_else(|error| panic!("relay evidence is unreadable: {error}"))
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("relay evidence is not JSON: {error}"))
        })
        .collect();
    let admitted: Vec<_> = records
        .iter()
        .filter(|row| row["outcome"] == "admitted")
        .collect();
    assert_eq!(
        admitted.len(),
        7,
        "relay admission count lost call identity"
    );
    assert!(admitted.iter().enumerate().all(|(index, row)| {
        row["call"] == format!("relay-call-{:02}", index.saturating_add(1))
    }));
    let failures: Vec<_> = records
        .iter()
        .filter(|row| row["outcome"] == "transport-failed")
        .collect();
    assert_eq!(failures.len(), 3);
    assert!(failures
        .iter()
        .any(|row| row["phase"] == "upstream-response-head"));
    assert!(failures.iter().any(|row| {
        row["phase"] == "upstream-response-stream"
            && row["response_bytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0)
    }));
    assert_eq!(provider.calls.load(Ordering::SeqCst), admitted.len());
    let permission_log = fs::read_to_string(root.join("permission-driver.jsonl"))
        .unwrap_or_else(|error| panic!("permission evidence is unreadable: {error}"));
    assert!(permission_log.contains("\"phase\": \"attempt\""));
    assert!(permission_log.contains("\"phase\": \"applied\""));
    assert!(!permission_log.contains("resources"));
    process_fixture::cleanup(&root);
}
