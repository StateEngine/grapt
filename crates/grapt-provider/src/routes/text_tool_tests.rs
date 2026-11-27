//! Custom text calls normalize to bounded JSON without losing their raw input.
use super::*;

fn opened() -> &'static str {
    r#"{"type":"response.output_item.added","item":{"type":"custom_tool_call","id":"text_item","call_id":"text_call","name":"apply_patch"}}"#
}
fn calls(harness: &Harness) -> Vec<&str> {
    harness
        .pending
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::ToolCalled { input, .. } => Some(input.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn custom_text_streams_and_duplicate_completion_emit_one_json_call(
) -> Result<(), Box<dyn std::error::Error>> {
    let text = "*** Begin Patch\n*** Add File: λ.txt\n+hello\n*** End Patch";
    let delta=serde_json::json!({"type":"response.custom_tool_call_input.delta","item_id":"text_item","delta":text}).to_string();
    let done=serde_json::json!({"type":"response.custom_tool_call_input.done","item_id":"text_item","input":text}).to_string();
    let item=serde_json::json!({"type":"response.output_item.done","item":{"type":"custom_tool_call","id":"text_item","call_id":"text_call","name":"apply_patch","input":text}}).to_string();
    let harness = responses(&[
        opened(),
        &delta,
        &done,
        &item,
        r#"{"type":"response.completed","response":{}}"#,
    ]);
    assert_eq!(calls(&harness).len(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(calls(&harness)[0])?,
        serde_json::json!({"input":text})
    );
    assert!(harness
        .shape()
        .iter()
        .any(|line| line == "terminal:ToolCalls"));
    Ok(())
}

#[test]
fn custom_text_can_arrive_only_on_completion_and_empty_text_is_not_an_object(
) -> Result<(), Box<dyn std::error::Error>> {
    for text in ["", "line\n\"quoted\"\tλ"] {
        let done=serde_json::json!({"type":"response.output_item.done","item":{"type":"custom_tool_call","id":"text_item","call_id":"text_call","name":"apply_patch","input":text}}).to_string();
        let harness = responses(&[
            opened(),
            &done,
            r#"{"type":"response.completed","response":{}}"#,
        ]);
        assert_eq!(calls(&harness).len(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(calls(&harness)[0])?["input"],
            text
        );
    }
    Ok(())
}

#[test]
fn custom_text_final_disagreement_fails_the_attempt() {
    for final_text in ["different", ""] {
        let delta=serde_json::json!({"type":"response.custom_tool_call_input.delta","item_id":"text_item","delta":"first"}).to_string();
        for done in [
            serde_json::json!({"type":"response.custom_tool_call_input.done","item_id":"text_item","input":final_text}),
            serde_json::json!({"type":"response.output_item.done","item":{"type":"custom_tool_call","id":"text_item","call_id":"text_call","name":"apply_patch","input":final_text}}),
        ] {
            let harness = responses(&[opened(), &delta, &done.to_string()]);
            assert_eq!(
                harness.terminal_class(),
                Some(&ProviderFailureClass::InvalidProviderOutput)
            );
            assert!(calls(&harness).is_empty());
        }
    }
}

#[test]
fn custom_text_missing_final_input_preserves_streamed_input(
) -> Result<(), Box<dyn std::error::Error>> {
    let delta=serde_json::json!({"type":"response.custom_tool_call_input.delta","item_id":"text_item","delta":"streamed"}).to_string();
    for done in [
        serde_json::json!({"type":"response.custom_tool_call_input.done","item_id":"text_item"}),
        serde_json::json!({"type":"response.output_item.done","item":{"type":"custom_tool_call","id":"text_item","call_id":"text_call","name":"apply_patch"}}),
    ] {
        let harness = responses(&[opened(), &delta, &done.to_string()]);
        assert_eq!(calls(&harness).len(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(calls(&harness)[0])?["input"],
            "streamed"
        );
    }
    Ok(())
}

#[test]
fn json_escape_growth_cannot_exceed_the_normalized_input_bound() {
    let text = "\0".repeat(crate::transport::sink::MAX_TOOL_INPUT_BYTES / 2);
    let done=serde_json::json!({"type":"response.custom_tool_call_input.done","item_id":"text_item","input":text}).to_string();
    let harness = responses(&[opened(), &done]);
    assert!(calls(&harness).is_empty());
    assert!(harness
        .pending
        .iter()
        .any(|fragment| matches!(fragment, AttemptFragment::ToolInputInvalid { .. })));
}

#[test]
fn custom_text_present_nonstring_final_input_is_invalid_instead_of_missing() {
    for input in [
        serde_json::Value::Null,
        serde_json::json!(false),
        serde_json::json!(17),
        serde_json::json!([]),
        serde_json::json!({}),
    ] {
        for streamed in [false, true] {
            for done in [
                serde_json::json!({"type":"response.custom_tool_call_input.done","item_id":"text_item","input":input}),
                serde_json::json!({"type":"response.output_item.done","item":{"type":"custom_tool_call","id":"text_item","call_id":"text_call","name":"apply_patch","input":input}}),
            ] {
                let delta = serde_json::json!({"type":"response.custom_tool_call_input.delta","item_id":"text_item","delta":"streamed"}).to_string();
                let done = done.to_string();
                let frames = if streamed {
                    vec![opened(), delta.as_str(), done.as_str()]
                } else {
                    vec![opened(), done.as_str()]
                };
                let harness = responses(&frames);
                assert_eq!(
                    harness.terminal_class(),
                    Some(&ProviderFailureClass::InvalidProviderOutput)
                );
                assert!(calls(&harness).is_empty());
            }
        }
    }
}
