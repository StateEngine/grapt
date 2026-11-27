//! Provider usage retention and non-fatal accounting diagnostics.
use super::*;

#[test]
fn a_failed_response_retains_reported_usage_before_its_failure() {
    let response_error = r#"{"type":"response.failed","response":{"usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":90},"output_tokens":2},"error":{"code":"server_error"}}}"#;
    let cases = [
        responses_for_route(&[response_error], UsageRoute::ChatGptCodex),
        responses(&[response_error]),
        anthropic(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":90,"cache_creation_input_tokens":0,"output_tokens":0}}}"#,
            r#"{"type":"message_delta","usage":{"output_tokens":2}}"#,
            r#"{"type":"error","error":{"type":"overloaded_error"}}"#,
        ]),
        compat(&[
            r#"{"choices":[],"usage":{"prompt_tokens":100,"prompt_tokens_details":{"cached_tokens":90},"completion_tokens":2}}"#,
            r#"{"error":{"type":"server_error"}}"#,
        ]),
    ];
    for (case, harness) in cases.into_iter().enumerate() {
        let fragments = harness.fragments();
        let observed: Vec<_> = fragments
            .iter()
            .enumerate()
            .filter_map(|(index, fact)| match fact {
                AttemptFragment::Usage { usage } => Some((index, usage)),
                _ => None,
            })
            .collect();
        assert_eq!(observed.len(), 1, "reported usage was lost or duplicated");
        let (at, usage) = observed[0];
        assert_eq!(usage.input_total().value(), Some(100), "case {case}");
        assert_eq!(usage.cache_read().value(), Some(90));
        assert_eq!(usage.output().value(), Some(2));
        assert!(fragments
            .iter()
            .skip(at + 1)
            .any(|fact| matches!(fact, AttemptFragment::ProviderFailure { .. })));
    }
    let absent = responses(&[r#"{"type":"response.failed","response":{"error":{}}}"#]);
    assert!(!absent
        .fragments()
        .iter()
        .any(|fact| matches!(fact, AttemptFragment::Usage { .. })));
}

#[test]
fn invalid_usage_has_a_closed_diagnosis_instead_of_provider_words() {
    use grapt_types::{
        failure_diagnostic::{UsageDiagnostic, UsageIssue},
        usage::UsageFieldKind,
    };
    let wire = r#"{"type":"response.completed","response":{"usage":{"input_tokens":"invalid","output_tokens":1,"vendorText":"sk-ant-secret\npretend success"}}}"#;
    let mut harness = Harness::new();
    let mut decoder = ResponsesDecoder::new();
    decoder.decode(wire, &mut harness.sink());
    let diagnostic = harness.usage_warning;
    assert_eq!(
        diagnostic,
        Some(UsageDiagnostic {
            field: Some(UsageFieldKind::InputTotal),
            issue: UsageIssue::WrongType
        })
    );
    assert_eq!(harness.terminal_class(), None);
    assert!(harness.pending.iter().any(|fragment| matches!(
        fragment,
        AttemptFragment::ProviderTerminal { usage: None, .. }
    )));
}

#[test]
fn malformed_usage_keeps_successful_terminals_across_routes() {
    let responses_payload =
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":"bad"}}}"#;
    let cases = [
        (
            "responses",
            responses_for_route(&[responses_payload], UsageRoute::OpenAiResponses),
        ),
        (
            "codex",
            responses_for_route(&[responses_payload], UsageRoute::ChatGptCodex),
        ),
        (
            "anthropic",
            anthropic(&[
                r#"{"type":"message_start","message":{"usage":{"input_tokens":"bad"}}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
                r#"{"type":"message_stop"}"#,
            ]),
        ),
        (
            "compatible",
            compat(&[
                r#"{"choices":[],"usage":{"prompt_tokens":"bad"}}"#,
                r#"{"choices":[{"delta":{"content":"preserved"},"finish_reason":"stop"}]}"#,
            ]),
        ),
    ];

    for (route, harness) in cases {
        assert!(harness.usage_warning.is_some(), "{route} warning missing");
        assert_eq!(
            harness.terminal_class(),
            None,
            "{route} accounting changed the expected outcome: {:?}",
            harness.shape()
        );
        assert!(
            harness.fragments().iter().any(|fragment| matches!(
                fragment,
                AttemptFragment::ProviderTerminal { usage: None, .. }
            )),
            "{route} emitted the wrong terminal after malformed accounting: {:?}",
            harness.shape()
        );
    }
}

#[test]
fn review_usage_sink_warning_preserves_latest_snapshot_on_failure() {
    for warned in [false, true] {
        let mut harness = Harness::new();
        for output in [2, 5] {
            let usage = crate::usage::normalize_anthropic_usage(&serde_json::json!({
                "usage": {"input_tokens": 7, "output_tokens": output}
            }))
            .unwrap_or_else(|error| panic!("valid usage: {error}"))
            .unwrap_or_else(|| panic!("usage present"));
            let mut sink = harness.sink();
            sink.remember_usage(usage);
            if warned {
                sink.usage_protocol_conflict();
            }
        }
        harness
            .sink()
            .fail(ProviderFailureClass::Transport, "stream ended");
        let output = harness
            .fragments()
            .into_iter()
            .find_map(|fragment| match fragment {
                AttemptFragment::Usage { usage } => usage.output().value(),
                _ => None,
            });
        assert_eq!(output, Some(5), "non-fatal warning: {warned}");
        assert_eq!(
            harness.terminal_class(),
            Some(&ProviderFailureClass::Transport)
        );
    }
}

#[test]
fn review_usage_sink_invalid_snapshot_stays_unavailable_on_failure() {
    let mut harness = Harness::new();
    let usage = crate::usage::normalize_anthropic_usage(&serde_json::json!({
        "usage": {"input_tokens": 7, "output_tokens": 5}
    }))
    .unwrap_or_else(|error| panic!("valid usage: {error}"))
    .unwrap_or_else(|| panic!("usage present"));
    harness.sink().remember_usage(usage.clone());
    let error = crate::usage::normalize_anthropic_usage(&serde_json::json!({
        "usage": {"output_tokens": -1}
    }))
    .err()
    .unwrap_or_else(|| panic!("negative usage refused"));
    harness.sink().invalid_usage(error);
    harness.sink().remember_usage(usage);
    harness
        .sink()
        .fail(ProviderFailureClass::Transport, "stream ended");
    assert!(!harness
        .fragments()
        .iter()
        .any(|fragment| matches!(fragment, AttemptFragment::Usage { .. })));
}
