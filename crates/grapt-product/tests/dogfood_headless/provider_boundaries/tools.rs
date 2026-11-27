//! No completion custody means no tool admission, even for blank arguments.
use super::*;

fn eof(route: Route, partial: bool) -> Script {
    let mut frames = match route {
        Route::Responses => vec![value!({"type":"response.output_item.added",
            "item":{"type":"function_call","id":"item_boundary","call_id":"call_boundary","name":"update_plan"}})],
        Route::Compatible => vec![value!({"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"id":"call_boundary","function":{"name":"update_plan"}}]}}]})],
        Route::Anthropic => vec![
            value!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
            value!({"type":"content_block_start","index":0,"content_block":
                {"type":"tool_use","id":"call_boundary","name":"update_plan","input":{}}}),
        ],
    };
    if partial {
        frames.push(match route {
            Route::Responses => value!({"type":"response.function_call_arguments.delta","item_id":"item_boundary","delta":"{\"plan\":["}),
            Route::Compatible => value!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"{\"plan\":["}}]}}]}),
            Route::Anthropic => value!({"type":"content_block_delta","index":0,
                "delta":{"type":"input_json_delta","partial_json":"{\"plan\":["}}),
        });
    }
    raw(frames)
}

fn no_admission(label: &str, route: Route, partial: bool) {
    // Repeating the same drop lets the ordinary retry owner reach its own cap.
    let outcome = run(label, route, vec![eof(route, partial); 5]);
    assert!(
        outcome.failed(),
        "unfinished input became a completed turn: {}",
        outcome.history
    );
    assert!(
        (1..=5).contains(&outcome.calls),
        "drop exceeded the existing attempt bound"
    );
    outcome.assert_no_tools();
}

#[test]
fn responses_tool_name_then_eof_never_admits_a_blank_call() {
    no_admission("responses-name-eof", Route::Responses, false);
}
#[test]
fn compatible_tool_name_then_eof_never_admits_a_blank_call() {
    no_admission("compatible-name-eof", Route::Compatible, false);
}
#[test]
fn anthropic_tool_name_then_eof_never_admits_a_blank_call() {
    no_admission("anthropic-name-eof", Route::Anthropic, false);
}
#[test]
fn responses_partial_arguments_then_eof_never_admit_a_call() {
    no_admission("responses-arguments-eof", Route::Responses, true);
}
#[test]
fn compatible_partial_arguments_then_eof_never_admit_a_call() {
    no_admission("compatible-arguments-eof", Route::Compatible, true);
}
#[test]
fn anthropic_partial_arguments_then_eof_never_admit_a_call() {
    no_admission("anthropic-arguments-eof", Route::Anthropic, true);
}

fn split_announcement(label: &str, route: Route) {
    let frames = match route {
        Route::Compatible => vec![
            value!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_split"}]}}]}),
            value!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,
                "function":{"name":"update_plan","arguments":"{\"plan\":[]}"}}]}}]}),
        ],
        Route::Responses => vec![
            value!({"type":"response.output_item.added","item":{"type":"function_call","id":"item_split","call_id":"call_split"}}),
            value!({"type":"response.output_item.added","item":{"type":"function_call","id":"item_split","name":"update_plan"}}),
        ],
        Route::Anthropic => vec![
            value!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
            value!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_split","input":{}}}),
            value!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"update_plan","input":{}}}),
        ],
    };
    let outcome = run(label, route, vec![route.completed(frames); 5]);
    assert!(
        outcome.failed()
            || (outcome.completed()
                && outcome.tool_admission_facts > 0
                && outcome.tool_terminals > 0),
        "tool intent vanished before a legitimate stop: {}",
        outcome.history
    );
    if outcome.failed() {
        outcome.assert_no_tools();
    } else {
        let tool = outcome.history["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .find(|item| item["type"] == "tool" && item["id"] == "call_split")
            .unwrap_or_else(|| panic!("split call result missing: {}", outcome.history));
        assert_eq!(tool["name"], "update_plan");
        assert_eq!(tool["state"]["status"], "completed");
        assert_eq!(tool["state"]["input"], value!({"plan":[]}));
    }
}

#[test]
fn responses_split_tool_identity_is_handled_or_explicitly_refused() {
    split_announcement("responses-split-tool", Route::Responses);
}

#[test]
fn anthropic_split_tool_identity_is_handled_or_explicitly_refused() {
    split_announcement("anthropic-split-tool", Route::Anthropic);
}
