//! EB02, EB04, EB05 and EB13 — what the receipts are obliged to say, proven by
//! running the binary.
//!
//! Every law here is now witnessed against a REAL RUN. That is not a style
//! choice: while `serve` composed no host, `render_exit` had no caller in
//! `src/` and the ready receipt was computed and dropped, so a mutant of either
//! could not change one byte of this binary's observable behaviour. A reddening
//! witness over an unreachable function proves a reference, not a product law.
//! The pure-function assertions are retained BELOW the binary ones as a
//! secondary check on the rendering itself, never as the only killer.
//!
//! The module is declared by path for the reason given in `eb_platform.rs`.

// The module is shared source compiled into this test binary as well as into
// the binary itself. Each witness file uses the part of it that carries its own
// law, so items belonging to a sibling witness are legitimately unused HERE.
#[allow(dead_code)]
#[path = "../src/receipt.rs"]
mod receipt;

use crate::receipt_scratch;

use grapt_api::ApiHostExit;
use receipt::{render_exit, ServeBounds, ServeReceipt, ServeWindow, CREDENTIAL_REFUSES_EVERYTHING};
use serde_json::Value;
use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

/// Ten distinct values, so a receipt that echoed one bound into another field
/// cannot pass by coincidence. Each is admissible to the crate that owns it,
/// because the binary now really composes a host from them.
fn distinct_bounds() -> ServeBounds {
    let Some(serve_window) = ServeWindow::try_new(120) else {
        panic!("a 120 ms window must be admitted");
    };
    ServeBounds {
        max_connections: 11,
        max_inflight: 22,
        max_headers: 33,
        max_request_head_bytes: 16_384,
        max_request_body_bytes: 55_555,
        max_requests_per_connection: 66,
        header_read_allowance_ms: 7_777,
        shutdown_drain_allowance_ms: 8_888,
        bind_attempts: 9,
        max_resolved_addresses: 10,
        serve_window,
    }
}

/// The argv that drives one `serve` run from a `ServeBounds`.
///
/// Built from the same struct the witnesses assert against, so a bound cannot
/// be supplied on the command line and compared against a different number.
fn serve_argv(bind: &str, bounds: &ServeBounds) -> Vec<String> {
    let pairs: [(&str, String); 12] = [
        ("--bind", bind.to_owned()),
        ("--max-connections", bounds.max_connections.to_string()),
        ("--max-inflight", bounds.max_inflight.to_string()),
        ("--max-headers", bounds.max_headers.to_string()),
        (
            "--max-request-head-bytes",
            bounds.max_request_head_bytes.to_string(),
        ),
        (
            "--max-request-body-bytes",
            bounds.max_request_body_bytes.to_string(),
        ),
        (
            "--max-requests-per-connection",
            bounds.max_requests_per_connection.to_string(),
        ),
        (
            "--header-read-allowance-ms",
            bounds.header_read_allowance_ms.to_string(),
        ),
        (
            "--shutdown-drain-allowance-ms",
            bounds.shutdown_drain_allowance_ms.to_string(),
        ),
        ("--bind-attempts", bounds.bind_attempts.to_string()),
        (
            "--max-resolved-addresses",
            bounds.max_resolved_addresses.to_string(),
        ),
        (
            "--serve-window-ms",
            bounds.serve_window.milliseconds().to_string(),
        ),
    ];
    let mut argv = vec!["serve".to_owned()];
    for (flag, value) in pairs {
        argv.push(flag.to_owned());
        argv.push(value);
    }
    argv
}

/// A receipt root of this case's own, outside any repository and emptied first.
///
/// Per-case rather than shared: these witnesses run concurrently under `cargo
/// test`, and a shared root would let one run's ready receipt be read as
/// another's.
fn case_root(case: &str) -> (receipt_scratch::tmp_guard::Dir, PathBuf) {
    let scratch = receipt_scratch::outside_temp_dir();
    let base = scratch.join("grapt-engined-eb-receipt").join(case);
    std::fs::create_dir_all(&base)
        .unwrap_or_else(|error| panic!("the case directory must be creatable: {error}"));
    // The root guard owns the whole tree; the test holds it for the run,
    // because the served binary recreates its paths on demand and a root
    // removed early would be resurrected as an empty ancestor.
    (scratch, base)
}

/// Any literal address, for the secondary assertions that only render.
///
/// Constructed rather than parsed-and-unwrapped: `unwrap` and `expect` are
/// denied workspace-wide, and a witness that reached for either would be asking
/// for an exception the code it guards does not get.
fn a_literal_address() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 1))
}

fn command(root: &Path, argv: &[String]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_grapt-engined"));
    command.args(argv).arg("--receipt-root").arg(root);
    command
}

/// Run one `serve` to its own terminal and hand back what a caller sees.
fn run_serve(root: &Path, argv: &[String]) -> Output {
    command(root, argv)
        .output()
        .unwrap_or_else(|error| panic!("the binary under test must be runnable: {error}"))
}

/// The durable ready receipt, parsed.
fn ready_receipt(root: &Path) -> Value {
    let path = root.join("serve-ready.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "serve must persist its ready receipt at {}: {error}",
            path.display()
        )
    });
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("the ready receipt must parse: {error}; got {text}"))
}

/// Wait for the ready receipt of a host that is still running.
///
/// Bounded by an ATTEMPT COUNT rather than by a deadline, because
/// `std::time::Instant::now` is a disallowed method in this workspace — the
/// ambient-clock law applies to a witness exactly as it applies to a durable
/// owner, and reaching for a clock here to save three lines would be the same
/// exception this lane refuses everywhere else.
///
/// A bound reached is a real failure rather than a hang: a `serve` that never
/// publishes readiness is precisely the defect the durable artifact exists to
/// make visible.
fn await_ready(root: &Path, child: &mut Child) -> Value {
    let path = root.join("serve-ready.json");
    for _ in 0..READY_POLL_ATTEMPTS {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                return value;
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("serve exited with {status} before publishing a ready receipt");
        }
        std::thread::sleep(READY_POLL_INTERVAL);
    }
    panic!(
        "serve published no ready receipt at {} within {} attempts",
        path.display(),
        READY_POLL_ATTEMPTS
    );
}

/// Two thousand ten-millisecond attempts: twenty seconds of patience, spent
/// without ever asking what time it is.
const READY_POLL_ATTEMPTS: u32 = 2_000;
const READY_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn stdout_object(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout carries exactly one JSON object; got {lines:?}"
    );
    serde_json::from_str(lines[0]).unwrap_or_else(|error| {
        panic!("the terminal receipt must parse: {error}; got {}", lines[0])
    })
}

fn observed_address(ready: &Value) -> SocketAddr {
    let text = ready
        .get("observed_address")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("the ready receipt must carry observed_address; got {ready}"));
    text.parse().unwrap_or_else(|error| {
        panic!("observed_address must be a socket address: {error}; got {text}")
    })
}

/// EB02. A bound accepted on the command line and then not echoed is a bound
/// the operator cannot confirm took effect, which makes the receipt a claim
/// rather than evidence — and a ready receipt nobody can read is not an echo.
#[test]
fn the_ready_receipt_echoes_every_supplied_bound() {
    let bounds = distinct_bounds();
    let (_case_guard, root) = case_root("echoes-every-bound");
    let output = run_serve(&root, &serve_argv("127.0.0.1:0", &bounds));
    assert_eq!(
        output.status.code(),
        Some(0),
        "the run must reach its own terminal; stdout {}",
        String::from_utf8_lossy(&output.stdout)
    );

    // THE KILLING ASSERTION: the durable artifact a real run wrote.
    let ready = ready_receipt(&root);
    for (key, value) in [
        ("max_connections", bounds.max_connections as u64),
        ("max_inflight", bounds.max_inflight as u64),
        ("max_headers", bounds.max_headers as u64),
        (
            "max_request_head_bytes",
            bounds.max_request_head_bytes as u64,
        ),
        (
            "max_request_body_bytes",
            bounds.max_request_body_bytes as u64,
        ),
        (
            "max_requests_per_connection",
            bounds.max_requests_per_connection as u64,
        ),
        ("header_read_allowance_ms", bounds.header_read_allowance_ms),
        (
            "shutdown_drain_allowance_ms",
            bounds.shutdown_drain_allowance_ms,
        ),
        ("bind_attempts", bounds.bind_attempts as u64),
        (
            "max_resolved_addresses",
            bounds.max_resolved_addresses as u64,
        ),
        ("serve_window_ms", bounds.serve_window.milliseconds()),
    ] {
        assert_eq!(
            ready.get(key).and_then(Value::as_u64),
            Some(value),
            "the persisted ready receipt must echo {key} as {value}; got {ready}"
        );
    }

    // NO WALL CLOCK. The window is echoed as the number the operator supplied,
    // and nothing here is an absolute time: a timestamp in this receipt would
    // silently take, inside `serve`, the very clock authority E1a refuses in
    // `service ensure`.
    for forbidden in ["started_at", "will_stop_at", "timestamp", "now"] {
        assert!(
            ready.get(forbidden).is_none(),
            "the ready receipt carries no wall-clock fact; {forbidden} appeared in {ready}"
        );
    }

    // Secondary, on the rendering itself.
    let rendered = ServeReceipt::from_bounds(&bounds, a_literal_address());
    assert_eq!(rendered.max_inflight, bounds.max_inflight);
    assert_eq!(rendered.max_connections, bounds.max_connections);
    assert_eq!(rendered.serve_window_ms, bounds.serve_window.milliseconds());
}

/// EB04. An operator who starts this daemon and sees a healthy bind must not
/// have to discover by sending a request that every request answers 401.
#[test]
fn the_ready_receipt_states_that_every_request_answers_401() {
    let (_case_guard, root) = case_root("states-the-401-posture");
    let output = run_serve(&root, &serve_argv("127.0.0.1:0", &distinct_bounds()));
    assert_eq!(
        output.status.code(),
        Some(0),
        "the run must reach a terminal"
    );

    // THE KILLING ASSERTION: what the real run actually disclosed.
    let ready = ready_receipt(&root);
    assert_eq!(
        ready.get("credential_posture").and_then(Value::as_str),
        Some(CREDENTIAL_REFUSES_EVERYTHING),
        "the persisted ready receipt must disclose the posture from the one named \
         constant; got {ready}"
    );
    assert!(
        ready
            .get("credential_posture")
            .and_then(Value::as_str)
            .is_some_and(|posture| posture.contains("401")),
        "the disclosed posture must say what every request actually gets; got {ready}"
    );

    // Secondary, on the rendering itself.
    let rendered = ServeReceipt::from_bounds(&distinct_bounds(), a_literal_address());
    assert_eq!(rendered.credential_posture, CREDENTIAL_REFUSES_EVERYTHING);
}

/// EB05. A forced close is a different fact from a graceful stop: connections
/// were still live when the drain allowance expired and were aborted. Rendering
/// it as `stopped_gracefully` tells an operator the system settled when it did
/// not, which is the one thing an exit receipt exists to prevent.
///
/// Both dispositions are driven through the BINARY, because that is the only
/// thing that makes the mutant observable: `render_exit` is reached from
/// `run_serve` and its output is the `exit` field on stdout.
#[test]
fn a_forced_close_is_never_rendered_as_stopped_gracefully() {
    // A run with nothing held: the window elapses, the drain completes, and the
    // host reports a graceful stop.
    let (_graceful_guard, graceful_root) = case_root("graceful-disposition");
    let graceful = run_serve(
        &graceful_root,
        &serve_argv("127.0.0.1:0", &distinct_bounds()),
    );
    let graceful_receipt = stdout_object(&graceful);
    assert_eq!(
        graceful_receipt.get("exit").and_then(Value::as_str),
        Some("stopped_gracefully"),
        "an unheld run stops gracefully; got {graceful_receipt}"
    );

    // A run with a connection still live when the one-millisecond drain
    // allowance expires. The window is long enough to connect inside; the
    // header allowance is long enough that the host is still waiting for the
    // request head, so the connection is genuinely live at the deadline.
    let mut bounds = distinct_bounds();
    bounds.shutdown_drain_allowance_ms = 1;
    bounds.header_read_allowance_ms = 30_000;
    let Some(window) = ServeWindow::try_new(2_500) else {
        panic!("a 2500 ms window must be admitted");
    };
    bounds.serve_window = window;

    let (_forced_guard, forced_root) = case_root("forced-close-disposition");
    let argv = serve_argv("127.0.0.1:0", &bounds);
    let mut child = command(&forced_root, &argv)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("the binary under test must be spawnable: {error}"));
    let ready = await_ready(&forced_root, &mut child);
    let address = observed_address(&ready);
    let mut held = TcpStream::connect(address).unwrap_or_else(|error| {
        panic!("the witness must reach the live host at {address}: {error}")
    });
    // A partial request head, never terminated. It forces the accept loop to
    // take the socket and start reading, so the connection is admitted rather
    // than merely queued in the kernel backlog.
    held.write_all(b"GET /witness HTTP/1.1\r\nHost: witness\r\n")
        .unwrap_or_else(|error| panic!("the held connection must accept bytes: {error}"));

    let forced = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("the served run must terminate: {error}"));
    drop(held);

    // THE KILLING ASSERTION for EB05: the exit field a real forced close put on
    // stdout. Under the mutant this reads "stopped_gracefully".
    let forced_receipt = stdout_object(&forced);
    assert_eq!(
        forced_receipt.get("exit").and_then(Value::as_str),
        Some("forced_close"),
        "a connection live at the drain deadline is a FORCED close and must never be \
         rendered as graceful; got {forced_receipt}"
    );
    assert_ne!(
        forced_receipt.get("exit"),
        graceful_receipt.get("exit"),
        "a forced close and a graceful stop must never render alike"
    );

    // Secondary, on the rendering itself.
    let forced_value = ApiHostExit::ForcedClose {
        admitted_connections: 9,
        completed_before_deadline: 4,
        active_at_deadline: 5,
        aborted_and_joined: 5,
    };
    let graceful_value = ApiHostExit::StoppedGracefully {
        admitted_connections: 9,
        completed_connections: 9,
        failed_connections: 0,
        admitted_requests: 12,
        refused_requests: 0,
    };
    assert_eq!(render_exit(&forced_value), "forced_close");
    assert_eq!(render_exit(&graceful_value), "stopped_gracefully");
    assert_ne!(render_exit(&forced_value), "stopped_gracefully");
}

/// EB13. The ready receipt names the address the host is ACTUALLY listening on.
///
/// This is a fidelity-of-outcome law, and it is not EB02's. EB02 asks whether
/// what you supplied came back; this asks whether what actually happened came
/// back. With an ephemeral `:0` bind the receipt is the only way anyone learns
/// the port, so echoing the requested address would leave every reader with a
/// port nothing listens on — the same class of false claim as a hardcoded exit.
#[test]
fn the_ready_receipt_names_the_observed_address_and_never_the_requested_one() {
    let mut bounds = distinct_bounds();
    // This witness connects after reading the filesystem receipt. Keep its
    // listener live while the parent is scheduled under the full workspace gate.
    let Some(window) = ServeWindow::try_new(2_500) else {
        panic!("a 2500 ms witness window must be admitted");
    };
    bounds.serve_window = window;
    let (_case_guard, root) = case_root("observed-address-fidelity");
    let requested = "127.0.0.1:0";
    let mut child = command(&root, &serve_argv(requested, &bounds))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("the binary under test must be spawnable: {error}"));
    let ready = await_ready(&root, &mut child);
    let address = observed_address(&ready);

    // THE KILLING ASSERTION: an ephemeral bind resolves to a real port, and the
    // receipt carries THAT. Under a mutant that echoes the requested address
    // this reads 0 and fails here.
    assert_ne!(
        address.port(),
        0,
        "an ephemeral bind is assigned a real port and the receipt must carry it; \
         got {ready}"
    );
    assert_ne!(
        ready.get("observed_address").and_then(Value::as_str),
        Some(requested),
        "the receipt must not echo the REQUESTED address back; got {ready}"
    );

    // And the address is not merely non-zero, it is the one a client reaches:
    // a receipt that invented a plausible port would pass the assertions above.
    let reached = TcpStream::connect(address);
    assert!(
        reached.is_ok(),
        "the observed address must be the live listener; connecting to {address} failed: {:?}",
        reached.err()
    );
    drop(reached);

    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("the served run must terminate: {error}"));
    let terminal = stdout_object(&output);
    assert_eq!(
        terminal.get("operation").and_then(Value::as_str),
        Some("serve"),
        "the terminal receipt is the serve receipt; got {terminal}"
    );
}

/// The operator-interrupt arm, witnessed where an interrupt can be delivered.
///
/// Linux only, and the platform split is in the WITNESS rather than in the
/// product: `main.rs` carries no `cfg` arm selecting behaviour, and this test
/// does not change that. A `SIGINT` to a child is deliverable here; on Windows
/// no `cargo` integration test can portably raise a console control event in
/// another process without taking a dependency this lane refuses.
#[cfg(target_os = "linux")]
#[test]
fn an_operator_interrupt_stops_the_host_and_is_named_as_the_trigger() {
    let mut bounds = distinct_bounds();
    let Some(window) = ServeWindow::try_new(60_000) else {
        panic!("a 60 s window must be admitted");
    };
    bounds.serve_window = window;

    let (_case_guard, root) = case_root("operator-interrupt");
    let mut child = command(&root, &serve_argv("127.0.0.1:0", &bounds))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("the binary under test must be spawnable: {error}"));
    let _ready = await_ready(&root, &mut child);

    // `kill` rather than a signalling crate: the frozen dependency closure
    // takes no new edge for a witness.
    let signalled = Command::new("kill")
        .arg("-INT")
        .arg(child.id().to_string())
        .status()
        .unwrap_or_else(|error| panic!("the witness must be able to signal the child: {error}"));
    assert!(signalled.success(), "kill -INT must be delivered");

    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("the interrupted run must terminate: {error}"));
    let terminal = stdout_object(&output);

    // The window is sixty seconds and this run ends in well under it, so the
    // trigger cannot be `window_elapsed` by coincidence.
    assert_eq!(
        terminal.get("trigger").and_then(Value::as_str),
        Some("operator_interrupt"),
        "an interrupted run names the operator as the trigger; got {terminal}"
    );
    assert_eq!(
        terminal.get("exit").and_then(Value::as_str),
        Some("stopped_gracefully"),
        "an interrupt with nothing held still drains gracefully; got {terminal}"
    );
}

/// The same arm, on a platform where it cannot be witnessed — declared, not
/// skipped.
///
/// The failure this guards against is a silent green: a Windows run in which
/// the interrupt arm is simply never exercised and the evidence reads as
/// though it were. This test's NAME is the disclosure, so the unsupported
/// outcome appears in the test list rather than in nobody's notes. Its owner is
/// the governed Linux battery under the successor profile.
#[cfg(not(target_os = "linux"))]
#[test]
fn the_operator_interrupt_arm_is_unwitnessed_on_this_platform_and_owned_by_the_linux_battery() {
    assert_ne!(
        std::env::consts::OS,
        "linux",
        "on Linux the real interrupt witness runs instead of this disclosure"
    );
    eprintln!(
        "UNSUPPORTED HERE: the operator_interrupt arm of await_stop is not witnessed on {}. \
         A mutant deleting that arm passes every window-driven witness in this file. \
         Owner: the governed Linux battery under the successor -B profile.",
        std::env::consts::OS
    );
}
