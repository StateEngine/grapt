//! Unknown provider failures use the existing bounded retry and visibility rules.
use super::*;

fn recovers(label: &str, route: Route) {
    let outcome = run(
        label,
        route,
        vec![route.error(value!("future_error_v7")), route.success()],
    );
    assert!(
        outcome.completed(),
        "unknown error was not retried: {}",
        outcome.history
    );
    assert_eq!(
        outcome.calls, 2,
        "the successful retry was not the next attempt"
    );
    assert!(outcome.history.to_string().contains("ordinary answer"));
    outcome.assert_no_tools();
}

fn exhausted(label: &str, route: Route) {
    // The ruled schedule is 2+4+8+16 s, so five attempts spend about 30 s here.
    // Narrowing ONLY the base backoff keeps the five-attempt bound and the
    // terminal class exactly as production has them, and this variable is set
    // in these two cases alone.
    let outcome = run_scoped(
        label,
        route,
        vec![route.error(value!("future_error_v7")); 5],
        &[("GRAPT_RETRY_BASE_MS", "1")],
    );
    assert!(
        outcome.failed(),
        "a persistent provider failure completed: {}",
        outcome.history
    );
    assert_eq!(outcome.calls, 5, "the existing attempt bound changed");
    assert!(outcome.messages.iter().all(|message| matches!(
        message.terminal,
        Some(StepTerminal::Failed {
            reason: grapt_types::StepTerminalReason::PolicyLimit { .. }
        })
    )));
    outcome.assert_no_tools();
}

fn terminals(label: &str, route: Route, types: &[&str]) {
    for kind in types {
        let outcome = run(
            label,
            route,
            vec![route.error(value!(kind)), route.success()],
        );
        assert!(
            outcome.failed(),
            "known terminal {kind} changed: {}",
            outcome.history
        );
        assert_eq!(outcome.calls, 1, "known terminal {kind} was retried");
        outcome.assert_no_tools();
    }
}

fn visible(label: &str, route: Route) {
    let mut frames = route.text(value!("already delivered answer"));
    let error = value!({"type":"future_error_v7","message":"private-vendor-error"});
    frames.push(match route {
        Route::Anthropic => value!({"type":"error","error":error}),
        _ => value!({"error":error}),
    });
    let outcome = run(label, route, vec![raw(frames), route.success()]);
    assert!(
        outcome.failed(),
        "a failure after visible output completed: {}",
        outcome.history
    );
    assert_eq!(outcome.calls, 1, "visible output was replayed by a retry");
    assert!(outcome
        .history
        .to_string()
        .contains("already delivered answer"));
    outcome.assert_no_tools();
}

fn malformed_types(label: &str, route: Route) {
    for kind in [Value::Null, value!(""), value!(42)] {
        let outcome = run(label, route, vec![route.error(kind), route.success()]);
        assert!(
            outcome.failed(),
            "malformed error type completed: {}",
            outcome.history
        );
        assert_eq!(
            outcome.calls, 1,
            "malformed error type was promoted to a retry"
        );
        outcome.assert_no_tools();
    }
    let mut error = value!({"error":{"message":"private-vendor-error"}});
    if matches!(route, Route::Anthropic) {
        error["type"] = value!("error");
    }
    let outcome = run(label, route, vec![raw(vec![error]), route.success()]);
    assert!(
        outcome.failed(),
        "missing error type completed: {}",
        outcome.history
    );
    assert_eq!(
        outcome.calls, 1,
        "missing error type was promoted to a retry"
    );
}

#[test]
fn compatible_unknown_error_retries_and_recovers() {
    recovers("compatible-unknown-retry", Route::Compatible);
}
#[test]
fn anthropic_unknown_error_retries_and_recovers() {
    recovers("anthropic-unknown-retry", Route::Anthropic);
}
#[test]
fn compatible_unknown_errors_stop_at_the_existing_attempt_bound() {
    exhausted("compatible-unknown-cap", Route::Compatible);
}
#[test]
fn anthropic_unknown_errors_stop_at_the_existing_attempt_bound() {
    exhausted("anthropic-unknown-cap", Route::Anthropic);
}
#[test]
fn compatible_known_terminal_errors_do_not_retry() {
    terminals(
        "compatible-known-terminal",
        Route::Compatible,
        &[
            "authentication_error",
            "invalid_request_error",
            "insufficient_quota",
            "model_not_found",
            "content_filter",
        ],
    );
}
#[test]
fn anthropic_known_terminal_errors_do_not_retry() {
    terminals(
        "anthropic-known-terminal",
        Route::Anthropic,
        &[
            "authentication_error",
            "permission_error",
            "invalid_request_error",
            "not_found_error",
            "content_filter",
        ],
    );
}
#[test]
fn compatible_unknown_error_after_completed_tool_input_does_not_retry() {
    // Compatible text stays unfinished until the stream terminal. A finish
    // chunk does settle complete tool input immediately, committing ToolCalled
    // before the trailing error arrives.
    let frames = vec![
        value!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,
            "id":"call_visible","type":"function","function":{"name":"update_plan",
            "arguments":"{\"plan\":[]}"}}]},"finish_reason":null}]}),
        value!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        value!({"error":{"type":"future_error_v7","message":"private-vendor-error"}}),
    ];
    let outcome = run(
        "compatible-visible-tool-error",
        Route::Compatible,
        vec![raw(frames), Route::Compatible.success()],
    );
    assert!(outcome.failed(), "{}", outcome.history);
    assert_eq!(outcome.calls, 1, "an admitted tool was replayed by a retry");
    assert!(outcome.tool_admission_facts > 0, "{}", outcome.history);
    assert_eq!(outcome.tool_terminals, 1, "{}", outcome.history);
    let tools = outcome.history["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|item| item["type"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 1, "{}", outcome.history);
    assert_eq!(tools[0]["id"], "call_visible");
    assert_eq!(tools[0]["name"], "update_plan");
    assert_eq!(tools[0]["state"]["input"], value!({"plan":[]}));
}
#[test]
fn compatible_unknown_error_after_unfinished_text_can_retry() {
    let mut frames = Route::Compatible.text(value!("unfinished answer"));
    frames.push(value!({"error":{"type":"future_error_v7","message":"private-vendor-error"}}));
    let outcome = run(
        "compatible-unfinished-text-error",
        Route::Compatible,
        vec![raw(frames), Route::Compatible.success()],
    );
    assert!(outcome.completed(), "{}", outcome.history);
    assert_eq!(outcome.calls, 2);
    assert!(outcome.history.to_string().contains("ordinary answer"));
    assert!(!outcome.history.to_string().contains("unfinished answer"));
    outcome.assert_no_tools();
}
#[test]
fn anthropic_unknown_error_after_visible_output_does_not_retry() {
    visible("anthropic-visible-error", Route::Anthropic);
}
#[test]
fn compatible_missing_or_malformed_error_types_stay_terminal() {
    malformed_types("compatible-malformed-error", Route::Compatible);
}
#[test]
fn anthropic_missing_or_malformed_error_types_stay_terminal() {
    malformed_types("anthropic-malformed-error", Route::Anthropic);
}

#[test]
fn responses_unknown_failure_keeps_its_existing_retry_behavior() {
    recovers("responses-unknown-control", Route::Responses);
}
