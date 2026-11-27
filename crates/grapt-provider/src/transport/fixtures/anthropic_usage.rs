//! Anthropic's early usage snapshot and cumulative message deltas over a socket.
//!
//! Constructed wire bytes reproduce the cumulative-count transition confirmed
//! on unchanged 7cef3077. These are not a raw provider capture; attribution of
//! the original live READY failure is a separate diagnostic.

use super::*;

async fn stream(start_usage: &str, delta_usage: &[&str]) -> Vec<AttemptFragment> {
    let mut frames = vec![
        format!(r#"{{"type":"message_start","message":{{"usage":{start_usage}}}}}"#),
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#
            .into(),
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"READY"}}"#
            .into(),
        r#"{"type":"content_block_stop","index":0}"#.into(),
    ];
    for usage in delta_usage {
        frames.push(format!(
            r#"{{"type":"message_delta","delta":{{"stop_reason":"end_turn"}},"usage":{usage}}}"#
        ));
    }
    frames.push(r#"{"type":"message_stop"}"#.into());
    let chunks = frames.iter().map(|frame| sse(&[frame])).collect();
    let fixture = serve(200, &[], chunks).await;
    let open = adapter(1)
        .open(input(
            "att_anthropic_usage",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
        .unwrap_or_else(|error| panic!("open: {error}"));
    drain(open).await
}

fn terminal_usage(fragments: &[AttemptFragment]) -> &crate::TokenUsage {
    assert_eq!(text_of(fragments), "READY");
    assert_eq!(failure_of(fragments), None, "{fragments:?}");
    assert_eq!(finish_of(fragments), Some(FinishReason::Stop));
    fragments
        .iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ProviderTerminal { usage, .. } => usage.as_ref(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("successful Anthropic stream omitted terminal usage"))
}

#[tokio::test]
async fn initial_output_count_is_replaced_by_cumulative_final_count() {
    let fragments = stream(
        r#"{"input_tokens":25,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}"#,
        &[r#"{"output_tokens":15}"#],
    )
    .await;
    let usage = terminal_usage(&fragments);
    assert_eq!(usage.input_total().value(), Some(25));
    assert_eq!(usage.input_fresh().value(), Some(25));
    assert_eq!(usage.output().value(), Some(15));
    assert_eq!(usage.cache_read().value(), Some(0));
    assert_eq!(usage.cache_write().value(), Some(0));
}

#[tokio::test]
async fn partial_cumulative_snapshots_retain_prior_input_and_cache_counts() {
    let fragments = stream(
        r#"{"input_tokens":25,"output_tokens":1,"cache_read_input_tokens":2,"cache_creation_input_tokens":3}"#,
        &[
            r#"{"input_tokens":30,"output_tokens":3,"cache_read_input_tokens":4}"#,
            r#"{"output_tokens":15}"#,
        ],
    )
    .await;
    let usage = terminal_usage(&fragments);
    assert_eq!(usage.input_total().value(), Some(37));
    assert_eq!(usage.input_fresh().value(), Some(30));
    assert_eq!(usage.output().value(), Some(15));
    assert_eq!(usage.cache_read().value(), Some(4));
    assert_eq!(usage.cache_write().value(), Some(3));
}

#[tokio::test]
async fn omitted_usage_leaves_stated_values_and_unknowns_intact() {
    let fragments = stream(r#"{"input_tokens":25,"output_tokens":0}"#, &[r#"{}"#]).await;
    let usage = terminal_usage(&fragments);
    assert_eq!(usage.input_fresh().value(), Some(25));
    assert_eq!(usage.output().value(), Some(0));
    assert_eq!(usage.cache_read().value(), None);
    assert_eq!(usage.cache_write().value(), None);
    assert_eq!(usage.input_total().value(), None);
}

#[tokio::test]
async fn null_terminal_usage_preserves_prior_reported_output() {
    let fragments = stream(
        r#"{"input_tokens":25,"output_tokens":1}"#,
        &[r#"{"output_tokens":null}"#],
    )
    .await;
    let usage = terminal_usage(&fragments);
    assert_eq!(usage.output().value(), Some(1));
}

#[tokio::test]
async fn decreasing_cumulative_count_leaves_usage_unknown_for_every_reported_field() {
    for delta in [
        r#"{"output_tokens":0}"#,
        r#"{"input_tokens":24}"#,
        r#"{"cache_read_input_tokens":1}"#,
        r#"{"cache_creation_input_tokens":2}"#,
    ] {
        let fragments = stream(
            r#"{"input_tokens":25,"output_tokens":1,"cache_read_input_tokens":2,"cache_creation_input_tokens":3}"#,
            &[delta],
        )
        .await;
        assert_eq!(text_of(&fragments), "READY");
        assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
        assert_eq!(failure_of(&fragments), None, "{delta}: {fragments:?}");
        assert!(fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderTerminal { usage: None, .. }
        )));
        assert!(!fragments
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::Usage { .. })));
    }
}

#[tokio::test]
async fn later_recovery_cannot_restore_unknown_usage_for_any_reported_field() {
    for (decrease, recovery) in [
        (r#"{"output_tokens":0}"#, r#"{"output_tokens":15}"#),
        (r#"{"input_tokens":24}"#, r#"{"input_tokens":30}"#),
        (
            r#"{"cache_read_input_tokens":1}"#,
            r#"{"cache_read_input_tokens":4}"#,
        ),
        (
            r#"{"cache_creation_input_tokens":2}"#,
            r#"{"cache_creation_input_tokens":5}"#,
        ),
    ] {
        let fragments = stream(
            r#"{"input_tokens":25,"output_tokens":1,"cache_read_input_tokens":2,"cache_creation_input_tokens":3}"#,
            &[decrease, recovery],
        ).await;
        assert_eq!(text_of(&fragments), "READY");
        assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
        assert_eq!(
            failure_of(&fragments),
            None,
            "{decrease} then {recovery}: {fragments:?}"
        );
        assert!(fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderTerminal { usage: None, .. }
        )));
        assert!(!fragments
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::Usage { .. })));
    }
}

#[test]
fn other_routes_keep_strict_equal_leaf_merging() {
    let early = crate::usage::normalize_anthropic_usage(&serde_json::json!({
        "usage": {"output_tokens": 1}
    }))
    .unwrap_or_else(|error| panic!("early: {error}"))
    .unwrap_or_else(|| panic!("early usage missing"));
    let later = crate::usage::normalize_anthropic_usage(&serde_json::json!({
        "usage": {"output_tokens": 15}
    }))
    .unwrap_or_else(|error| panic!("later: {error}"))
    .unwrap_or_else(|| panic!("later usage missing"));
    for route in [
        crate::UsageRoute::OpenAiResponses,
        crate::UsageRoute::ChatGptCodex,
        crate::UsageRoute::OpenAiCompatible,
    ] {
        assert!(early.merge(&later, route).is_err(), "weakened {route:?}");
        assert_eq!(early.merge(&early, route), Ok(early.clone()));
    }
}
