//! THE THREE STARTUP PHASES, and that each is bounded on its own clock.
//!
//! Split out of `bounds.rs` under the workspace god-file ceiling. These
//! replaced a single witness asserting that ONE deadline covered handshake and
//! discovery together. That claim was true of this product and is not true of
//! the frozen contract, which connects under `timeout.startup` and then
//! discovers under a separate `timeout.catalog`. Relaxing the old test would
//! have hidden the change; these three state it.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

use super::super::*;
use super::fixtures::*;

/// A server that answers each step slowly, so the phases can be measured.
fn slow_server(
    theirs: tokio::io::DuplexStream,
    delays: [Duration; 2],
) -> tokio::task::JoinHandle<()> {
    slow_server_with_stall(theirs, delays, Duration::ZERO)
}

fn slow_server_with_stall(
    theirs: tokio::io::DuplexStream,
    delays: [Duration; 2],
    wall_stall: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(theirs);
        let mut lines = BufReader::new(read);
        for (reply, delay) in [handshake(), listing()].into_iter().zip(delays) {
            let mut request = String::new();
            if matches!(lines.read_line(&mut request).await, Ok(0) | Err(_)) {
                return;
            }
            // A stalled worker must not consume a paused test clock's budget.
            std::thread::sleep(wall_stall);
            tokio::time::sleep(delay).await;
            if write.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
            let _ = write
                .write_all(
                    b"
",
                )
                .await;
            let _ = write.flush().await;
        }
    })
}

/// THE STARTUP BUDGET BOUNDS THE HANDSHAKE.
///
/// The catalog budget is generous here, so a timeout can only be the startup
/// phase's doing. Its predecessor asserted that ONE budget covered handshake
/// AND discovery together; the frozen contract gives discovery its own clock
/// (`client.connect({timeout: startup})`, then a separate `catalogTimeout`),
/// so that claim is no longer true of this product and is replaced by these
/// three rather than relaxed.
#[tokio::test]
async fn the_startup_budget_bounds_the_handshake() {
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let server = slow_server(theirs, [Duration::from_millis(300), Duration::ZERO]);
    let (read, write) = tokio::io::split(ours);
    let mut channel = McpChannel::new(read, write, bounds());
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_millis(120),
        Duration::from_secs(30),
        "0.1.0",
    )
    .await;
    assert!(
        matches!(
            reason,
            Err(StartFailure {
                reason: Unusable::TimedOut,
                ..
            })
        ),
        "a slow handshake must exhaust the startup budget: {reason:?}"
    );
    server.abort();
}

/// THE CATALOG BUDGET BOUNDS DISCOVERY, SEPARATELY.
///
/// The handshake is instant and the startup budget generous, so only the
/// catalog phase can be responsible for this timeout. Without a distinct
/// catalog deadline this case would pass under the startup budget and the
/// phase would be unwitnessed.
#[tokio::test]
async fn the_catalog_budget_bounds_discovery_separately() {
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let server = slow_server(theirs, [Duration::ZERO, Duration::from_millis(300)]);
    let (read, write) = tokio::io::split(ours);
    let mut channel = McpChannel::new(read, write, bounds());
    let mut session = session();
    let reason = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(30),
        Duration::from_millis(120),
        "0.1.0",
    )
    .await;
    assert!(
        matches!(
            reason,
            Err(StartFailure {
                reason: Unusable::TimedOut,
                ..
            })
        ),
        "slow discovery must exhaust the CATALOG budget: {reason:?}"
    );
    server.abort();
}

/// DISCOVERY DOES NOT INHERIT THE TIME THE HANDSHAKE SPENT.
///
/// This is the behaviour change stated as a witness rather than left implicit.
/// The handshake consumes most of its budget and discovery then takes about as
/// long again; under the previous single deadline that total exceeded it and
/// the startup failed. The phases are separate now, so this SUCCEEDS -- and if
/// someone later collapses them back into one clock, this goes red and says
/// exactly which property was lost.
#[tokio::test(start_paused = true)]
async fn discovery_gets_its_own_clock_rather_than_the_handshakes_remainder() {
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let server = slow_server_with_stall(
        theirs,
        [Duration::from_millis(120), Duration::from_millis(120)],
        Duration::from_millis(250),
    );
    let (read, write) = tokio::io::split(ours);
    let mut channel = McpChannel::new(read, write, bounds());
    let mut session = session();
    let started = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_millis(200),
        Duration::from_millis(200),
        "0.1.0",
    )
    .await;
    assert!(
        started.is_ok(),
        "each phase has its own budget, and 120ms fits inside 200ms twice: {started:?}"
    );
    server.abort();
}

/// THE PRESENTED FAILURE NAMES THE PHASE THAT ACTUALLY FAILED.
///
/// Separating the budgets is only half the fix. Every timeout was reported as
/// "did not answer within the startup budget", so a server that handshook
/// cleanly and was slow to list its tools sent the operator to the wrong
/// setting -- they would raise `startup` and see no change. The message is
/// what an operator acts on, so it is what this asserts.
#[tokio::test]
async fn a_catalog_timeout_is_presented_as_the_catalog_phase() {
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let server = slow_server(theirs, [Duration::ZERO, Duration::from_millis(300)]);
    let (read, write) = tokio::io::split(ours);
    let mut channel = McpChannel::new(read, write, bounds());
    let mut session = session();
    let failure = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_secs(30),
        Duration::from_millis(120),
        "0.1.0",
    )
    .await
    .err()
    .unwrap_or_else(|| panic!("slow discovery must fail"));

    let entry = ServerEntry::unusable_in(failure.reason, failure.phase);
    let detail = entry.detail();
    assert!(
        detail.contains("catalog"),
        "a catalog timeout must name the catalog budget: {detail}"
    );
    assert!(
        !detail.contains("startup"),
        "a catalog timeout must not blame the startup budget: {detail}"
    );
    server.abort();
}

/// And the handshake phase still names its own budget, so the test above is
/// not satisfied by a message that never mentions startup at all.
#[tokio::test]
async fn a_handshake_timeout_is_presented_as_the_startup_phase() {
    let (ours, theirs) = tokio::io::duplex(1024 * 64);
    let server = slow_server(theirs, [Duration::from_millis(300), Duration::ZERO]);
    let (read, write) = tokio::io::split(ours);
    let mut channel = McpChannel::new(read, write, bounds());
    let mut session = session();
    let failure = start(
        &mut channel,
        &mut session,
        bounds(),
        Duration::from_millis(120),
        Duration::from_secs(30),
        "0.1.0",
    )
    .await
    .err()
    .unwrap_or_else(|| panic!("a slow handshake must fail"));

    let detail = ServerEntry::unusable_in(failure.reason, failure.phase).detail();
    assert!(
        detail.contains("startup"),
        "a handshake timeout must name the startup budget: {detail}"
    );
    server.abort();
}
