use super::*;
use serde_json::json;

#[test]
fn anthropic_search_refuses_unadvertised_unpaired_and_over_budget_server_blocks() {
    let start = json!({"type":"content_block_start","index":0,"content_block":{
        "type":"server_tool_use","id":"srvtoolu_one","name":"web_search","input":{"query":"question"}}});
    let stop = json!({"type":"content_block_stop","index":0});
    let result = json!({"type":"content_block_start","index":1,"content_block":{
        "type":"web_search_tool_result","tool_use_id":"srvtoolu_wrong","content":[]}});
    for case in [
        "unadvertised",
        "missing",
        "mismatched",
        "intervening",
        "over-budget",
    ] {
        let mut harness = Harness::new();
        let mut decoder = AnthropicDecoder::new().searching((case != "unadvertised").then_some(1));
        for event in [&start, &stop] {
            decoder.decode(&event.to_string(), &mut harness.sink());
        }
        let event = match case {
            "mismatched" => result.clone(),
            "intervening" => {
                json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"intervening"}})
            }
            "over-budget" => {
                let mut valid = result.clone();
                valid["content_block"]["tool_use_id"] = "srvtoolu_one".into();
                decoder.decode(&valid.to_string(), &mut harness.sink());
                decoder.decode(
                    r#"{"type":"content_block_stop","index":1}"#,
                    &mut harness.sink(),
                );
                let mut second = start.clone();
                second["content_block"]["id"] = "srvtoolu_two".into();
                second
            }
            _ => json!({"type":"message_stop"}),
        };
        decoder.decode(&event.to_string(), &mut harness.sink());
        assert_eq!(
            harness.terminal_class(),
            Some(&ProviderFailureClass::InvalidProviderOutput),
            "{case}"
        );
        assert!(!harness.fragments().iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ToolCalled {
                execution: ToolExecution::Local,
                ..
            }
        )));
    }
}
