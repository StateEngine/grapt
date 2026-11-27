use super::*;
use crate::ToolInputFormat;

fn definition() -> ToolDefinition {
    let schema = serde_json::json!({
        "type":"object", "properties":{"input":{"type":"string"}},
        "required":["input"], "additionalProperties":false,
    });
    ToolDefinition {
        input_format: ToolInputFormat::Text {
            grammar: "start: /.+/".into(),
        },
        name: "apply_patch".into(),
        description: Some("Apply a text patch".into()),
        input_schema: schema.to_string(),
    }
}
fn exchange() -> Result<Vec<CodexInputItem>, Box<dyn std::error::Error>> {
    let call = ToolCallId::new("text_call").ok_or("call")?;
    Ok(vec![
        CodexInputItem::AssistantResponseStart,
        CodexInputItem::TextToolCall {
            call: call.clone(),
            tool: ToolName::new("apply_patch").ok_or("tool")?,
            input: CodexToolText::try_new("*** Begin Patch\n*** End Patch".into())?,
        },
        CodexInputItem::ToolOutput {
            call,
            output: CodexToolText::try_new("No files were modified.".into())?,
            is_error: true,
        },
    ])
}
#[test]
fn responses_text_tools_emit_grammar_custom_calls_and_matching_outputs(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut request = OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: exchange()?,
        web_search: None,
        prompt_cache_key: None,
        max_output_tokens: None,
        tools: vec![definition()],
        tool_choice: Some(ToolChoice::Named {
            tool: "apply_patch".into(),
        }),
    };
    let wire = super::super::openai_responses_body(&request)?;
    assert_eq!(wire["tools"][0]["type"], "custom");
    assert_eq!(
        wire["tools"][0]["format"],
        serde_json::json!({"type":"grammar","syntax":"lark","definition":"start: /.+/"})
    );
    assert!(wire["tools"][0].get("parameters").is_none());
    assert_eq!(
        wire["tool_choice"],
        serde_json::json!({"type":"custom","name":"apply_patch"})
    );
    assert_eq!(wire["input"][0]["type"], "custom_tool_call");
    assert_eq!(wire["input"][0]["input"], "*** Begin Patch\n*** End Patch");
    assert_eq!(wire["input"][1]["type"], "custom_tool_call_output");
    request.tools.clear();
    request.tool_choice = None;
    let withheld = super::super::openai_responses_body(&request)?;
    assert_eq!(
        withheld["input"], wire["input"],
        "historical syntax must survive current tool withholding"
    );
    let codex = ChatGptCodexRequest {
        model: request.model,
        input: request.input,
        web_search: None,
        prompt_cache_key: None,
        tools: vec![definition()],
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    };
    assert_eq!(super::super::codex_body(&codex)?["input"], wire["input"]);
    Ok(())
}
#[test]
fn function_only_routes_receive_the_same_text_in_a_json_input_field(
) -> Result<(), Box<dyn std::error::Error>> {
    let anthropic = AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        model: model("anthropic", "claude-test"),
        messages: exchange()?,
        max_tokens: bound(100),
        tools: vec![definition()],
        tool_choice: None,
        thinking: None,
        effort: crate::EffortPolicy::default(),
        prompt_cache: crate::AnthropicPromptCache::Off,
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
    };
    let wire = super::super::anthropic_body(&anthropic)?;
    assert_eq!(
        wire["tools"][0]["input_schema"]["properties"]["input"]["type"],
        "string"
    );
    assert_eq!(
        wire["messages"][0]["content"][0]["input"],
        serde_json::json!({"input":"*** Begin Patch\n*** End Patch"})
    );
    assert_eq!(wire["messages"][1]["content"][0]["type"], "tool_result");
    let messages = crate::routes::compatible_input::project(&exchange()?)?;
    let arguments = messages[0]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .ok_or("arguments")?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(arguments)?,
        serde_json::json!({"input":"*** Begin Patch\n*** End Patch"})
    );
    Ok(())
}
