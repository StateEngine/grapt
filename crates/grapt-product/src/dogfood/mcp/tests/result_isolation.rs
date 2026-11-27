//! Result validation belongs to one call, never the connection generation.
use super::super::*;
use super::fixtures::{conformant, discovered_tool, handshake_answering, live_launch, local};
use grapt_mcp::session::CallOutcome;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

async fn assert_followup_calls_succeed(
    registry: &McpRegistry,
    tool: &McpToolName,
    approved: &LaunchIdentity,
) {
    for call in ["small-read", "send-once"] {
        let result = registry
            .call_with(
                "fixture",
                "stateengine",
                tool,
                approved,
                call,
                json!({"action":call}),
                Duration::from_secs(5),
                None,
                None,
                Some(json!({"fixture-private":"fixture-capability"})),
            )
            .await;
        assert!(result.is_ok(), "{call}: {result:?}");
    }
    let repeated = registry
        .call_with(
            "fixture",
            "stateengine",
            tool,
            approved,
            "send-once",
            json!({}),
            Duration::from_secs(5),
            None,
            None,
            Some(json!({"fixture-private":"fixture-capability"})),
        )
        .await;
    assert!(
        matches!(repeated, Err(McpCallRefusal::Session(_))),
        "{repeated:?}"
    );
}

#[tokio::test]
async fn oversized_private_result_keeps_connection_for_the_next_read_and_send() {
    let registry = super::fixtures::registry_for_tests();
    let (stream, frames, peer) = conformant(handshake_answering(grapt_mcp::PROTOCOL_VERSION));
    registry
        .install_live_for_test("fixture", "stateengine", local("prog"), stream, "prog")
        .await;
    let approved = {
        let locations = registry.entries.lock().await;
        live_launch(&locations["fixture"].entries["stateengine"])
    };
    let held = registry
        .io_handle_for_test("fixture", "stateengine")
        .await
        .unwrap_or_else(|| panic!("installed connection missing"));
    let tool = discovered_tool();
    let result = registry
        .call_with(
            "fixture",
            "stateengine",
            &tool,
            &approved,
            "large",
            json!({"data":"x".repeat(32_740)}),
            Duration::from_secs(5),
            None,
            None,
            Some(json!({"fixture-private":"fixture-capability"})),
        )
        .await;
    let outcome =
        result.unwrap_or_else(|error| panic!("oversized result should succeed: {error:?}"));
    assert!(
        outcome.text().len() > 32_768 && outcome.text().contains(&"x".repeat(32_740)),
        "call_with must preserve intact oversized output for consumer validation/spill"
    );
    assert!(
        !held.is_retired(),
        "a completely consumed reply retired its healthy channel"
    );
    assert_eq!(
        held.io().lock().await.session.outcome("large"),
        Some(CallOutcome::Completed)
    );
    assert_followup_calls_succeed(&registry, &tool, &approved).await;
    assert!(
        !held.is_retired(),
        "a duplicate refused before dispatch poisoned the stream"
    );
    let current = registry
        .io_handle_for_test("fixture", "stateengine")
        .await
        .unwrap_or_else(|| panic!("connection disappeared"));
    assert!(Arc::ptr_eq(&held, &current));
    let frames = frames
        .lock()
        .unwrap_or_else(|_| panic!("recorder poisoned"));
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["method"] == "tools/call")
            .count(),
        3
    );
    peer.abort();
}
