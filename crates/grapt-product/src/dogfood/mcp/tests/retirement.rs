//! Retirement: marking a held connection dead without waiting behind whoever
//! is using it.

use grapt_mcp::session::{McpSession, SessionBounds};
use grapt_mcp::McpBounds;

use super::super::channel::HeldIo;
use super::super::{BoxedRead, LiveIo, McpChannel};
use super::fixtures::{approval_for, local};

/// Drive a call until it is PROVABLY waiting on the connection's I/O lock.
///
/// Sleeping and hoping establishes elapsed time, not that the caller passed
/// its approval check and registered a waiter. Polling to `Pending` while the
/// guard is deliberately held, with no other lock contested, does: the call
/// can only be pending at the I/O wait, because every step before it —
/// registry lock, lookup, approval comparison — is uncontended and completes
/// within the same poll.
fn poll_until_queued<F: std::future::Future>(call: &mut std::pin::Pin<Box<F>>) {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    match call.as_mut().poll(&mut context) {
        std::task::Poll::Pending => {}
        std::task::Poll::Ready(_) => {
            panic!("the call completed without ever queuing on the held connection")
        }
    }
}

pub(super) fn held_connection_for_test() -> HeldIo<LiveIo> {
    held_connection()
}

fn held_connection() -> HeldIo<LiveIo> {
    let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    HeldIo::new(LiveIo {
        _slot: match super::super::ConnectionSlot::admit(&counter, 1) {
            Some(slot) => slot,
            None => panic!("the witness must be admitted"),
        },
        channel: McpChannel::new(
            Box::new(tokio::io::empty()) as BoxedRead,
            Box::new(tokio::io::sink()) as super::super::BoxedWrite,
            match McpBounds::conservative() {
                Ok(bounds) => bounds,
                Err(_) => panic!("the conservative bounds must be constructible"),
            },
        ),
        session: McpSession::declared(match SessionBounds::conservative() {
            Ok(bounds) => bounds,
            Err(_) => panic!("the conservative session bounds must be constructible"),
        }),
        _child: None,
        _stderr: None,
        _remote: None,
    })
}

/// Retirement is observable WITHOUT taking the connection's I/O lock.
///
/// That is the whole point: an owner must be able to mark a connection dead
/// while a caller is holding its lock for up to the call budget. An owner that
/// had to take the lock to retire a connection could not retire the one that
/// most needs it.
#[test]
fn a_connection_can_be_retired_while_its_io_lock_is_held() {
    let held = held_connection();
    assert!(!held.is_retired());

    // The lock is HELD, as a stalled call would hold it.
    let guard = held.io().try_lock();
    assert!(guard.is_ok(), "the lock is free before anyone takes it");

    held.retire();
    assert!(
        held.is_retired(),
        "retirement must not wait behind the caller holding the lock"
    );
}

// NOT YET WITNESSED, and I will not pretend otherwise: the end-to-end path --
// a call queued behind a stalled one, its connection retired mid-wait, and the
// queued caller refusing on acquisition -- is not covered here. The scenario
// needs the stalled holder released on demand, and aborting its task did not
// release the guard promptly enough for a deterministic assertion. The
// re-check itself is in `call` and the mechanism above is witnessed; the
// integration is owed.

/// A QUEUED CALL REFUSES A CONNECTION RETIRED WHILE IT WAITED, AND THE
/// CONNECTION RECEIVES NOTHING.
///
/// The approval comparison happens before queuing, and a queue can outlast the
/// connection. This holds the I/O guard in a controlled owner rather than
/// relying on a cancelled task to drop one: the owner occupies the connection,
/// the call queues behind it, the declaration is withdrawn through the REAL
/// eviction path, then the owner drops its guard on purpose.
#[tokio::test]
async fn a_queued_call_refuses_a_connection_retired_while_it_waited() {
    let registry = std::sync::Arc::new(super::fixtures::registry_for_tests());
    let (stream, frames, server) = super::fixtures::conformant(
        super::fixtures::handshake_answering(grapt_mcp::PROTOCOL_VERSION),
    );
    let incarnation = registry
        .install_live_for_test(
            "E:/projects/one",
            "srv",
            super::fixtures::local("prog"),
            stream,
            "prog",
        )
        .await;
    let approved = approval_for(&local("prog"), "prog", incarnation);

    // A controlled owner occupies the connection.
    let handle = match registry.io_handle_for_test("E:/projects/one", "srv").await {
        Some(handle) => handle,
        None => panic!("the installed connection must be reachable"),
    };
    let owned = handle.io().lock().await;

    let frames_before = match frames.lock() {
        Ok(frames) => frames.len(),
        Err(_) => panic!("the recorder was poisoned"),
    };

    // The call queues behind the owner, and the PRECONDITION IS ASSERTED: it
    // is polled until pending, which it can only be at the I/O wait.
    let tool = super::fixtures::discovered_tool();
    let mut queued = Box::pin(registry.call(
        "E:/projects/one",
        "srv",
        &tool,
        &approved,
        "call-queued",
        serde_json::json!({}),
        std::time::Duration::from_secs(20),
    ));
    poll_until_queued(&mut queued);

    // The declaration is withdrawn through the real eviction path.
    registry
        .with_status(
            "E:/projects/one",
            &std::collections::BTreeMap::new(),
            |entries| assert!(entries.is_empty()),
        )
        .await;

    // The owner releases DELIBERATELY. No cancellation timing involved.
    drop(owned);

    match tokio::time::timeout(std::time::Duration::from_secs(10), queued).await {
        Ok(Err(super::super::McpCallRefusal::ConnectionReplaced { current, .. })) => {
            assert!(
                current.contains("retired"),
                "the refusal must name retirement: {current}"
            );
        }
        other => panic!("a queued call must refuse a retired connection: {other:?}"),
    }

    let frames_after = match frames.lock() {
        Ok(frames) => frames.len(),
        Err(_) => panic!("the recorder was poisoned"),
    };
    assert_eq!(
        frames_before, frames_after,
        "a retired connection must receive NOTHING"
    );
    server.abort();
}

/// The positive companion: an un-retired connection still serves the same
/// queued shape, so the witness above cannot pass by refusing everything.
#[tokio::test]
async fn a_queued_call_on_a_live_connection_still_runs() {
    let registry = std::sync::Arc::new(super::fixtures::registry_for_tests());
    let (stream, _frames, server) = super::fixtures::conformant(
        super::fixtures::handshake_answering(grapt_mcp::PROTOCOL_VERSION),
    );
    let incarnation = registry
        .install_live_for_test(
            "E:/projects/one",
            "srv",
            super::fixtures::local("prog"),
            stream,
            "prog",
        )
        .await;
    let approved = approval_for(&local("prog"), "prog", incarnation);
    let handle = match registry.io_handle_for_test("E:/projects/one", "srv").await {
        Some(handle) => handle,
        None => panic!("the installed connection must be reachable"),
    };
    let owned = handle.io().lock().await;

    let tool = super::fixtures::discovered_tool();
    let mut queued = Box::pin(registry.call(
        "E:/projects/one",
        "srv",
        &tool,
        &approved,
        "call-queued",
        serde_json::json!({"id": "1"}),
        std::time::Duration::from_secs(20),
    ));
    // The companion must ACTUALLY have queued too, or it proves nothing about
    // the path the negative exercises.
    poll_until_queued(&mut queued);
    drop(owned);

    match tokio::time::timeout(std::time::Duration::from_secs(10), queued).await {
        Ok(Ok(_)) => {}
        other => panic!("a queued call on a live connection must run: {other:?}"),
    }
    server.abort();
}
