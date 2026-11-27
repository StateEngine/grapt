#![cfg(feature = "loopback-testkit")]

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "public_seams/acp.rs"]
mod acp;
#[path = "public_seams/acp_gap.rs"]
mod acp_gap;
#[path = "public_seams/acp_replay.rs"]
mod acp_replay;
#[path = "public_seams/background.rs"]
mod background;
#[path = "public_seams/command_failures.rs"]
mod command_failures;
#[path = "public_seams/compatibility.rs"]
mod compatibility;
#[path = "public_seams/event_stream.rs"]
mod event_stream;
#[path = "public_seams/images.rs"]
mod images;
#[path = "public_seams/logout.rs"]
mod logout;
#[path = "public_seams/long_turn.rs"]
mod long_turn;
#[path = "public_seams/resident.rs"]
mod resident;
#[path = "public_seams/seat_provision.rs"]
mod seat_provision;
#[path = "public_seams/selection.rs"]
mod selection;
#[path = "public_seams/server_loss.rs"]
mod server_loss;
#[path = "public_seams/step_cap.rs"]
mod step_cap;
mod support;

use support::{spawn_retrying_catalog_server, spawn_server, Script};

use grapt_cli::{
    serve_acp, AcpConnectionError, ApiClient, BoundedOutput, CancellationCause,
    CancellationOutcome, ChildLiveness, CliLimits, ClientEndpoint, ConvergenceError,
    DiscoveredService, DiscoveryError, ExitClass, OutputError, OutputMode, RunError, Runner,
    ServiceDiscovery,
};
use grapt_service::RegistrationRecord;
use grapt_types::authority::Timestamp;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> TestResult<Self> {
        let sequence = SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "grapt-cli-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root)?;
        Ok(Self { root })
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup_result = std::fs::remove_dir_all(&self.root);
    }
}

async fn prove_cancellation_race() -> TestResult {
    let scripts = vec![
        Script::json(
            "GET",
            "/api/session/active",
            200,
            r#"{"data":{"ses_a":{"type":"running"}}}"#,
        ),
        Script::status("POST", "/api/session/ses_a/interrupt", 409),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let runner = Runner::new(client(&endpoint)?);
    let receipt = runner
        .cancel("ses_a".parse()?, CancellationCause::Sigint)
        .await?;
    assert_eq!(receipt.exit, ExitClass::Success);
    assert_eq!(receipt.cancelled, None);
    assert_eq!(
        receipt.cancellation_outcome,
        Some(CancellationOutcome::AlreadyTerminal)
    );
    assert_eq!(
        server.await??,
        [
            "GET /api/session/active",
            "POST /api/session/ses_a/interrupt",
            "GET /api/session/active",
        ]
    );
    Ok(())
}

#[tokio::test]
async fn runner_cancel_reconciles_terminal_truth_across_interrupt_race() -> TestResult {
    prove_cancellation_race().await
}

#[tokio::test]
async fn runner_cancel_waits_for_terminal_after_interrupt_success() -> TestResult {
    let scripts = vec![
        Script::json(
            "GET",
            "/api/session/active",
            200,
            r#"{"data":{"ses_a":{"type":"running"}}}"#,
        ),
        Script::status("POST", "/api/session/ses_a/interrupt", 200),
        Script::json("GET", "/api/session/active", 200, r#"{"data":{}}"#),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let receipt = Runner::new(client(&endpoint)?)
        .cancel("ses_a".parse()?, CancellationCause::Sigterm)
        .await?;
    assert_eq!(receipt.exit, ExitClass::Cancelled);
    assert_eq!(
        receipt.cancellation_outcome,
        Some(CancellationOutcome::InterruptSettled)
    );
    assert_eq!(server.await??.len(), 3);
    Ok(())
}

async fn prove_runner_convergence() -> TestResult {
    prove_runner_advisory(true).await
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn prove_runner_advisory(conflict: bool) -> TestResult {
    let first_event = concat!(
        "event: message\n",
        "data: {\"type\":\"session.text.ended\",\"id\":\"evt_1\",\"created\":1.0,\"durable\":{\"aggregateID\":\"ses_a\",\"seq\":1,\"version\":1},\"data\":{\"sessionID\":\"ses_a\",\"assistantMessageID\":\"msg_a\",\"ordinal\":0,\"text\":\"one\"}}\n\n"
    );
    let sparse_event = concat!(
        "event: message\n",
        "data: {\"type\":\"session.text.ended\",\"id\":\"evt_3\",\"created\":1.0,\"durable\":{\"aggregateID\":\"ses_a\",\"seq\":3,\"version\":1},\"data\":{\"sessionID\":\"ses_a\",\"assistantMessageID\":\"msg_a\",\"ordinal\":0,\"text\":\"three\"}}\n\n"
    );
    let prompt = r#"{"data":{"admittedSeq":1,"id":"msg_user","sessionID":"ses_a","timeCreated":1.0,"type":"user","data":{"text":"hello"},"delivery":"queue"}}"#;
    let second_event = if conflict {
        sparse_event.replace("\"seq\":3", "\"seq\":1")
    } else {
        sparse_event.to_owned()
    };
    let mut scripts = vec![
        Script::sse("GET", "/api/event", first_event),
        Script::json("POST", "/api/session/ses_a/prompt", 200, prompt),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            r#"{"data":[],"cursor":{}}"#,
        ),
        Script::json(
            "GET",
            "/api/session/ses_a/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json(
            "GET",
            "/api/session/ses_a/message",
            200,
            r#"{"data":[],"cursor":{}}"#,
        ),
        Script::json(
            "GET",
            "/api/session/ses_a/permission",
            200,
            r#"{"data":[],"hasMore":false}"#,
        ),
        Script::json(
            "GET",
            "/api/session/active",
            200,
            r#"{"data":{"ses_a":{"type":"running"}}}"#,
        ),
        Script::sse("GET", "/api/event", &second_event),
    ];
    if !conflict {
        // Durable history holds the admitted prompt by the terminal read.
        for _ in 0..3 {
            scripts.push(Script::json(
                "GET",
                "/api/session/ses_a/message",
                200,
                r#"{"data":[{"type":"user","id":"msg_user","time":{"created":1.0},"text":"hello"}],"cursor":{}}"#,
            ));
        }
        for _ in 0..2 {
            scripts.push(Script::json(
                "GET",
                "/api/session/ses_a/permission",
                200,
                r#"{"data":[],"hasMore":false}"#,
            ));
        }
        scripts.push(Script::json(
            "GET",
            "/api/session/active",
            200,
            r#"{"data":{}}"#,
        ));
    }
    let (endpoint, server) = spawn_server(scripts).await?;
    let runner = Runner::new(client(&endpoint)?);
    let mut output = BoundedOutput::new(
        Vec::new(),
        Vec::new(),
        OutputMode::Jsonl,
        CliLimits::production(),
    );
    let result = runner
        .run_selected("ses_a".parse()?, 1, "hello".to_owned(), &mut output)
        .await;
    if conflict {
        assert!(
            matches!(
                result,
                Err(RunError::Convergence(
                    ConvergenceError::ConflictingDuplicate { sequence: 1 }
                ))
            ),
            "unexpected runner result: {result:?}"
        );
    } else {
        assert_eq!(result?.exit, ExitClass::Success);
    }
    let (stdout, _stderr, receipt) = output.into_parts();
    let rendered = String::from_utf8(stdout)?;
    assert!(receipt.visible);
    assert!(!rendered.contains("session.text.ended"));
    assert!(!rendered.contains("evt_1"));
    assert_eq!(server.await??.len(), if conflict { 8 } else { 14 });
    Ok(())
}

#[tokio::test]
async fn runner_refuses_advisory_conflict_without_emitting_advisory_truth() -> TestResult {
    prove_runner_convergence().await
}

#[tokio::test]
async fn runner_reconciles_sparse_advisories_from_durable_truth() -> TestResult {
    prove_runner_advisory(false).await
}

async fn prove_child_owner() -> TestResult {
    let registration = registration_for_pid(41)?;
    assert!(matches!(
        ServiceDiscovery::verify_child_owner(&registration, 42),
        Err(DiscoveryError::ChildOwnerMismatch {
            expected: 42,
            found: 41
        })
    ));

    let root = Scratch::new("child-owner")?;
    std::fs::write(root.path().join("registration"), registration.encode())?;
    let discovery = ServiceDiscovery::try_new(root.path(), env!("CARGO_PKG_VERSION"))?;
    assert!(matches!(
        discovery.discover_for_child(42).await,
        Err(DiscoveryError::ChildOwnerMismatch {
            expected: 42,
            found: 41
        })
    ));

    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"))
        .arg("not-a-command")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let child_pid = child.id().ok_or("child pid absent")?;
    let stale_pid = if child_pid == 1 { 2 } else { child_pid - 1 };
    assert!(matches!(
        ApiClient::from_embedded(
            &DiscoveredService::loopback_testkit(registration_for_pid(stale_pid)?),
            child,
            CliLimits::production()
        ),
        Err((grapt_cli::ApiClientError::EmbeddedOwnerMismatch { .. }, _))
    ));
    Ok(())
}

fn registration_for_pid(pid: u32) -> TestResult<RegistrationRecord> {
    registration_for_endpoint(pid, "http://127.0.0.1:17777", "instance-old")
}

fn registration_for_endpoint(
    pid: u32,
    endpoint: &str,
    instance: &str,
) -> TestResult<RegistrationRecord> {
    let owner = OwnerReceipt::new(
        ServiceSlotId::new("grapt").ok_or("slot")?,
        ServiceInstanceId::new(instance).ok_or("instance")?,
        EnvironmentReceipt::new("test").ok_or("environment")?,
        ServiceGeneration::new(7),
        ProcessId::new(pid).ok_or("pid")?,
        Endpoint::new(endpoint).ok_or("endpoint")?,
    );
    Ok(RegistrationRecord::new(
        owner,
        ServiceRegistrationVersion::new(env!("CARGO_PKG_VERSION")).ok_or("version")?,
        Timestamp::from_millis(1),
    )?)
}

fn write_discovery_fixture(root: &Path, pid: u32, endpoint: &str, build: &str) -> TestResult {
    let registration = registration_for_endpoint(pid, endpoint, "instance-live")?;
    std::fs::write(root.join("registration"), registration.encode())?;
    std::fs::write(root.join("basic.credential"), b"test:test\n")?;
    std::fs::write(
        root.join("serve-ready.json"),
        format!(
            "{{\"schema\":\"secode.product.ready.v2\",\"operation\":\"serve\",\"service_slot\":\"grapt\",\"service_instance\":\"instance-live\",\"environment\":\"test\",\"service_generation\":7,\"pid\":{pid},\"observed_endpoint\":\"{endpoint}\",\"serve_window_ms\":1,\"build\":\"{build}\"}}"
        ),
    )?;
    Ok(())
}

#[tokio::test]
async fn child_owner_verification_refuses_stale_daemon_identity() -> TestResult {
    prove_child_owner().await
}

fn prove_binary_main() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_grapt"))
        .arg("not-a-command")
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(i32::from(ExitClass::Usage.code()))
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.len() <= CliLimits::production().values().stderr_bytes);
    assert!(String::from_utf8(output.stderr)?.contains("unknown command"));
    Ok(())
}

#[test]
fn binary_main_routes_usage_diagnostics_through_bounded_stderr() -> TestResult {
    prove_binary_main()
}

#[test]
fn binary_run_refuses_invalid_stdin_before_embedded_connection_effects() -> TestResult {
    let root = Scratch::new("stdin-refusal")?;
    let marker = root.path().join("child-launched.marker");
    let state = root.path().join("child-state.marker");
    let service = root.path().join("service-state");
    let product = instrumented_product(root.path())?;
    let maximum = CliLimits::production().values().stdin_bytes;
    let oversized = vec![b'x'; maximum.saturating_add(1)];

    assert_stdin_refusal_is_effect_free(
        &product,
        &service,
        &marker,
        &state,
        &oversized,
        &format!("stdin exceeded its {maximum}-byte bound"),
    )?;
    assert_stdin_refusal_is_effect_free(
        &product,
        &service,
        &marker,
        &state,
        &[0xff],
        "stdin is not UTF-8",
    )?;
    Ok(())
}

async fn write_rpc(
    input: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: serde_json::Value,
) -> TestResult {
    input.write_all(value.to_string().as_bytes()).await?;
    input.write_all(b"\n").await?;
    Ok(())
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

async fn read_rpc_response(
    output: &mut BufReader<tokio::io::DuplexStream>,
    id: i64,
) -> TestResult<serde_json::Value> {
    read_rpc_response_with_frames(output, id)
        .await
        .map(|(response, _)| response)
}

async fn read_rpc_response_with_frames(
    output: &mut BufReader<tokio::io::DuplexStream>,
    id: i64,
) -> TestResult<(serde_json::Value, Vec<serde_json::Value>)> {
    let mut frames = Vec::new();
    for _ in 0..16 {
        let mut line = String::new();
        let read = output.read_line(&mut line).await?;
        if read == 0 {
            return Err("ACP output closed before its response".into());
        }
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("id").and_then(serde_json::Value::as_i64) == Some(id) {
            return Ok((value, frames));
        }
        frames.push(value);
    }
    Err(format!("ACP response id {id} exceeded the fixture frame bound").into())
}

fn assert_stdin_refusal_is_effect_free(
    product: &Path,
    service: &Path,
    marker: &Path,
    state: &Path,
    input: &[u8],
    diagnostic: &str,
) -> TestResult {
    let mut child = Command::new(env!("CARGO_BIN_EXE_grapt"))
        .arg("run")
        .arg("--embedded")
        .arg("--product")
        .arg(product)
        .arg("--service")
        .arg(service)
        .arg("--new")
        .arg("--stdin")
        .env("GRAPT_TEST_CHILD_MARKER", marker)
        .env("GRAPT_TEST_STATE_MARKER", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("binary stdin pipe absent")?;
    stdin.write_all(input)?;
    drop(stdin);
    let output = child.wait_with_output()?;

    assert_eq!(
        output.status.code(),
        Some(i32::from(ExitClass::Usage.code()))
    );
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr)?.contains(diagnostic));
    assert!(
        !marker.exists(),
        "rejected stdin launched the product child"
    );
    assert!(
        !state.exists(),
        "rejected stdin advanced product-owned state"
    );
    assert!(
        !service.exists(),
        "rejected stdin created connection-owned service state"
    );
    Ok(())
}

/// Create an executable stub without this process ever holding it open for
/// writing.
///
/// A test binary spawns children from many threads at once. A child forked
/// while this process held a write descriptor on the stub keeps a copy of it
/// until that child execs, and executing the stub inside that window fails
/// with ETXTBSY ("Text file busy"): observed as a launch-window test refused
/// with "child launch failed: Text file busy (os error 26)" under load. A
/// short-lived `sh` writes the file instead, so no descriptor of ours exists.
#[cfg(unix)]
fn write_executable(path: &std::path::Path, contents: impl AsRef<[u8]>) -> TestResult {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 700 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;
    writer
        .stdin
        .take()
        .ok_or("the stub writer has no stdin")?
        .write_all(contents.as_ref())?;
    let status = writer.wait()?;
    if !status.success() {
        return Err(format!("writing {} failed: {status}", path.display()).into());
    }
    Ok(())
}

#[cfg(unix)]
fn instrumented_product(root: &Path) -> TestResult<PathBuf> {
    let product = root.join("instrumented-product.sh");
    write_executable(
        &product,
        concat!(
            "#!/bin/sh\n",
            "printf 'launched\\n' > \"$GRAPT_TEST_CHILD_MARKER\"\n",
            "printf 'advanced\\n' > \"$GRAPT_TEST_STATE_MARKER\"\n",
        ),
    )?;
    Ok(product)
}

#[cfg(windows)]
fn instrumented_product(root: &Path) -> TestResult<PathBuf> {
    let product = root.join("instrumented-product.cmd");
    std::fs::write(
        &product,
        concat!(
            "@echo off\r\n",
            "> \"%GRAPT_TEST_CHILD_MARKER%\" echo launched\r\n",
            "> \"%GRAPT_TEST_STATE_MARKER%\" echo advanced\r\n",
        ),
    )?;
    Ok(product)
}

fn prove_partial_output() -> TestResult {
    let mut output = BoundedOutput::new(
        PrefixThenBroken::new(2),
        PrefixThenBroken::new(3),
        OutputMode::Text,
        CliLimits::production(),
    );
    assert!(matches!(output.text("hello"), Err(OutputError::BrokenPipe)));
    let receipt = output.receipt();
    assert_eq!(receipt.stdout_bytes, 2);
    assert_eq!(receipt.stdout_records, 0);
    assert!(receipt.visible);
    assert!(matches!(
        output.diagnostic("failure"),
        Err(OutputError::BrokenPipe)
    ));
    assert_eq!(output.receipt().stderr_bytes, 3);
    Ok(())
}

#[test]
fn partial_stdout_and_stderr_failures_preserve_honest_receipts() -> TestResult {
    prove_partial_output()
}

fn client(endpoint: &str) -> Result<grapt_cli::ApiClient, grapt_cli::ApiClientError> {
    grapt_cli::ApiClient::loopback_testkit(
        ClientEndpoint::loopback_testkit(endpoint)?,
        CliLimits::production(),
    )
}

#[derive(Debug)]
struct PrefixThenBroken {
    remaining: usize,
}

impl PrefixThenBroken {
    const fn new(remaining: usize) -> Self {
        Self { remaining }
    }
}

impl Write for PrefixThenBroken {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
        }
        let amount = self.remaining.min(bytes.len());
        self.remaining -= amount;
        Ok(amount)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
