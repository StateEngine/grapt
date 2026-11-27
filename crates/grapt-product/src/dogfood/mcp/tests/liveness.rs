//! Concurrency and observed liveness: what one connection's trouble may do to
//! another, and what a reader is told after a failure was seen.

use std::time::Duration;

use grapt_mcp::PROTOCOL_VERSION;
use serde_json::json;

use super::fixtures::*;

/// ONE STALLED CALL MUST NOT BLOCK THE PRODUCT.
///
/// A silent server takes the whole call budget. While the registry lock was
/// held across the network call, that meant every other location's status
/// read and every unrelated dispatch waited behind it -- the whole product
/// serialized behind one bad server.
///
/// A stalled call is started and left in flight; the assertions are that an
/// unrelated location's status read completes and an unrelated server's
/// call completes while it is still stalled.
#[tokio::test]
async fn a_stalled_call_does_not_block_another_locations_status_or_dispatch() {
    let registry = std::sync::Arc::new(registry_for_tests());

    let (stalling, _stall_frames, stall_server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let stalled_incarnation = registry
        .install_live_for_test("E:/projects/slow", "srv", local("slow"), stalling, "slow")
        .await;

    let (other, _other_frames, other_server) = conformant(handshake_answering(PROTOCOL_VERSION));
    let other_incarnation = registry
        .install_live_for_test("E:/projects/fast", "srv", local("fast"), other, "fast")
        .await;

    // A call that will never be answered, left IN FLIGHT.
    let stalling_registry = std::sync::Arc::clone(&registry);
    let stalled = tokio::spawn(async move {
        stalling_registry
            .call(
                "E:/projects/slow",
                "srv",
                &discovered_tool(),
                &approval_for(&local("slow"), "slow", stalled_incarnation),
                "call-stalled",
                json!({"silent": true}),
                Duration::from_secs(30),
            )
            .await
    });

    // Give the stalled call time to take its connection's lock.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // An unrelated location's STATUS read must complete.
    let status = tokio::time::timeout(
        Duration::from_secs(5),
        registry.with_status(
            "E:/projects/fast",
            &declared("srv", local("fast")),
            |entries| entries.contains_key("srv"),
        ),
    )
    .await;
    assert_eq!(
        status.ok(),
        Some(true),
        "a status read must not wait behind another location's stalled call"
    );

    // And an unrelated server's DISPATCH must complete.
    let dispatched = tokio::time::timeout(
        Duration::from_secs(5),
        registry.call(
            "E:/projects/fast",
            "srv",
            &discovered_tool(),
            &approval_for(&local("fast"), "fast", other_incarnation),
            "call-fast",
            json!({"id": "1"}),
            Duration::from_secs(5),
        ),
    )
    .await;
    match dispatched {
        Ok(Ok(_)) => {}
        other => panic!("an unrelated dispatch must not wait behind a stall: {other:?}"),
    }

    stalled.abort();
    stall_server.abort();
    other_server.abort();
}
