use super::*;
use crate::fragment::FinishReason;
use crate::replay::ReplayBinding;
use serde_json::json;

fn decode(status: &str, enabled: bool) -> Harness {
    let mut harness = Harness::new();
    let mut decoder = ResponsesDecoder::new().searching(enabled.then(ReplayBinding::fixture));
    let item = json!({"type":"response.output_item.done","item":{
        "type":"web_search_call","id":"ws_native","status":status,
        "action":{"type":"search","queries":["fixture question"],
        "sources":[{"type":"url","url":"https://example.com/source?query=private"}]}}});
    decoder.decode(&item.to_string(), &mut harness.sink());
    decoder.decode(
        r#"{"type":"response.completed","response":{}}"#,
        &mut harness.sink(),
    );
    harness
}

#[test]
fn native_search_keeps_execution_and_sources_without_requesting_a_local_continuation(
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = decode("completed", true);
    let fragments = harness.fragments();
    assert_eq!(fragments.iter().filter(|fragment| matches!(fragment,
        AttemptFragment::ToolCalled { execution: ToolExecution::ProviderHosted, tool, .. } if tool == "web_search")).count(), 1);
    assert!(!fragments.iter().any(|fragment| matches!(
        fragment,
        AttemptFragment::ToolCalled {
            execution: ToolExecution::Local,
            ..
        }
    )));
    let (result, state) = fragments
        .iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::HostedToolResult { result, state, .. } => Some((result, state)),
            _ => None,
        })
        .ok_or("native terminal missing")?;
    assert!(result.contains("https://example.com/source?query=private"));
    let state: serde_json::Value =
        serde_json::from_slice(state.capture_bytes().ok_or("native replay missing")?)?;
    assert_eq!(state["kind"], "openai-web-search-v1");
    assert_eq!(
        state["item"]["action"]["sources"][0]["url"],
        "https://example.com/source?query=private"
    );
    assert!(matches!(
        fragments.last(),
        Some(AttemptFragment::ProviderTerminal {
            finish: FinishReason::Stop,
            ..
        })
    ));
    Ok(())
}

#[test]
fn failed_native_search_is_a_hosted_failure_and_unadvertised_search_is_refused() {
    for status in ["failed", "incomplete"] {
        let harness = decode(status, true);
        assert!(harness
            .fragments()
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::HostedToolFailed { .. })));
        assert!(matches!(
            harness.fragments().last(),
            Some(AttemptFragment::ProviderTerminal {
                finish: FinishReason::Stop,
                ..
            })
        ));
    }
    for (status, enabled) in [("searching", true), ("completed", false)] {
        let harness = decode(status, enabled);
        assert_eq!(
            harness.terminal_class(),
            Some(&ProviderFailureClass::InvalidProviderOutput)
        );
        assert!(!harness
            .fragments()
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::ToolCalled { .. })));
    }
}
