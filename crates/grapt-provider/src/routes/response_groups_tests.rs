use crate::transport::fixtures::{anthropic_request, compat_request, responses_request};
use crate::{CodexInputItem, CodexToolText, InputMessage, MessageRole, ProviderRequest, ToolName};
use grapt_types::ToolCallId;
use serde_json::{json, Value};

fn text(value: &str) -> CodexInputItem {
    InputMessage {
        role: MessageRole::Assistant,
        content: value.into(),
    }
    .into()
}

fn exchange(text_at: usize) -> Result<Vec<CodexInputItem>, Box<dyn std::error::Error>> {
    let mut response = Vec::new();
    for name in ["call-a", "call-b"] {
        response.push(CodexInputItem::ToolCall {
            call: ToolCallId::new(name).ok_or("call")?,
            tool: ToolName::new("read_lines").ok_or("tool")?,
            arguments: CodexToolText::try_new("{}".into())?,
        });
    }
    response.insert(text_at, text("response text"));
    response.insert(0, CodexInputItem::AssistantResponseStart);
    for name in ["call-a", "call-b"] {
        response.push(CodexInputItem::ToolOutput {
            call: ToolCallId::new(name).ok_or("call")?,
            output: CodexToolText::try_new(format!("result:{name}"))?,
            is_error: name == "call-b",
        });
    }
    response.extend([
        CodexInputItem::AssistantResponseStart,
        text("next response"),
        CodexInputItem::AssistantResponseStart,
        text("another response"),
        InputMessage {
            role: MessageRole::User,
            content: "next input".into(),
        }
        .into(),
    ]);
    Ok(response)
}

fn bodies(input: &[CodexInputItem]) -> Result<[Value; 3], Box<dyn std::error::Error>> {
    let ProviderRequest::AnthropicMessages(mut anthropic) = anthropic_request() else {
        panic!("family")
    };
    let ProviderRequest::OpenAiCompatible(mut compatible) = compat_request() else {
        panic!("family")
    };
    let ProviderRequest::OpenAiResponses(mut responses) = responses_request() else {
        panic!("family")
    };
    anthropic.messages = input.to_vec();
    compatible.messages = input.to_vec();
    responses.input = input.to_vec();
    for request in [
        ProviderRequest::AnthropicMessages(anthropic.clone()),
        ProviderRequest::OpenAiCompatible(compatible.clone()),
        ProviderRequest::OpenAiResponses(responses.clone()),
    ] {
        super::request_validation::validate(&request)?;
    }
    Ok([
        super::body::anthropic_body(&anthropic)?,
        super::body::compatible_body(&compatible, crate::CompatibleProtocol::OpenAi)?,
        super::body::openai_responses_body(&responses)?,
    ])
}

#[test]
fn every_wire_groups_parallel_calls_and_results_without_merging_responses(
) -> Result<(), Box<dyn std::error::Error>> {
    for text_at in [0, 1, 2] {
        let input = exchange(text_at)?;
        let [anthropic, chat, responses] = bodies(&input)?;
        let blocks = anthropic["messages"][0]["content"]
            .as_array()
            .ok_or("blocks")?;
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[text_at],
            json!({"type":"text","text":"response text"})
        );
        assert_eq!(
            anthropic["messages"][1]["content"],
            json!([
                {"type":"tool_result","tool_use_id":"call-a","content":"result:call-a","is_error":false},
                {"type":"tool_result","tool_use_id":"call-b","content":"result:call-b","is_error":true}
            ])
        );
        assert_eq!(anthropic["messages"].as_array().ok_or("messages")?.len(), 5);
        assert_eq!(chat["messages"][0]["content"], "response text");
        assert_eq!(
            chat["messages"][0]["tool_calls"]
                .as_array()
                .ok_or("calls")?
                .len(),
            2
        );
        for (index, name) in ["call-a", "call-b"].iter().enumerate() {
            assert_eq!(chat["messages"][0]["tool_calls"][index]["id"], *name);
            assert_eq!(
                chat["messages"][index + 1],
                json!({"role":"tool","tool_call_id":name,"content":format!("result:{name}")})
            );
            assert_eq!(
                responses["input"][index + 3]["type"],
                "function_call_output"
            );
            assert_eq!(responses["input"][index + 3]["call_id"], *name);
        }
        assert_eq!(chat["messages"][3]["content"], "next response");
        assert_eq!(chat["messages"][4]["content"], "another response");
        assert_eq!(
            responses["input"][text_at]["content"], "response text",
            "Responses must keep text at its own ordinal"
        );
        assert_final_turns(&[anthropic.clone(), chat.clone(), responses.clone()]);
        let encoded = [anthropic, chat, responses]
            .map(|body| serde_json::to_vec(&body).unwrap_or_else(|e| panic!("body bytes: {e}")));
        assert_eq!(
            encoded,
            bodies(&input)?.map(|body| serde_json::to_vec(&body).unwrap_or_else(|e| panic!("body bytes: {e}"))),
            "serialization must be byte-stable"
        );
    }
    Ok(())
}

#[test]
fn explicit_groups_refuse_interleaved_missing_duplicate_and_unmatched_results(
) -> Result<(), Box<dyn std::error::Error>> {
    let input = exchange(2)?;
    let mut interleaved = input.clone();
    interleaved.swap(3, 4); // exact bad C04: first result, assistant text, second result.
    let mut missing = input.clone();
    missing.remove(5);
    let mut duplicate = input.clone();
    duplicate.insert(5, input[4].clone());
    let mut unmatched = input.clone();
    unmatched[4] = CodexInputItem::ToolOutput {
        call: ToolCallId::new("unknown-call").ok_or("call")?,
        output: CodexToolText::try_new("unmatched".into())?,
        is_error: false,
    };
    for bad in [interleaved, missing, duplicate, unmatched] {
        assert!(super::response_groups::validate(&bad).is_err());
        assert!(bodies(&bad).is_err());
    }
    Ok(())
}

#[test]
fn cache_marking_runs_after_grouping_and_keeps_tool_content_policy(
) -> Result<(), Box<dyn std::error::Error>> {
    let ProviderRequest::OpenAiCompatible(mut request) = compat_request() else {
        panic!("family")
    };
    request.messages = exchange(2)?;
    request.prompt_cache = crate::CompatiblePromptCache::AnthropicBreakpoints;
    let body = super::body::compatible_body(&request, crate::CompatibleProtocol::OpenAi)?;
    assert_eq!(
        body["messages"][0]["tool_calls"]
            .as_array()
            .ok_or("calls")?
            .len(),
        2
    );
    assert_eq!(body["messages"][1]["content"], "result:call-a");
    assert_eq!(body["messages"][2]["content"], "result:call-b");
    assert_eq!(
        body["messages"][4]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        body["messages"][5]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    Ok(())
}

// Native tool results become a user turn; Chat and Responses retain tool items.
fn assert_final_turns(bodies: &[Value; 3]) {
    for (index, body) in bodies.iter().enumerate() {
        let key = if index == 2 { "input" } else { "messages" };
        let last = body[key]
            .as_array()
            .and_then(|items| items.last())
            .unwrap_or_else(|| panic!("missing final wire turn for format {index}"));
        assert!(
            last["role"] == "user"
                || last["role"] == "tool"
                || last["type"] == "function_call_output",
            "coding request ended in assistant prefill for format {index}: {last}"
        );
    }
}

#[test]
fn coding_request_final_turn_is_user_or_tool_in_every_format(
) -> Result<(), Box<dyn std::error::Error>> {
    let user = || {
        CodexInputItem::from(InputMessage {
            role: MessageRole::User,
            content: "inspect current state".into(),
        })
    };
    let mut complete = exchange(2)?;
    complete.truncate(6); // C04 response and both results.
    let interrupted = vec![
        user(),
        CodexInputItem::AssistantResponseStart,
        text("Historical tool has no recorded outcome; effects are unknown."),
        user(),
    ];
    // First request, continuation, distinct text responses, interrupted/new input.
    for input in [vec![user()], complete, exchange(0)?, interrupted] {
        let before = bodies(&input)?;
        assert_final_turns(&before);
        // The durable reopen witness separately proves identical assembled input.
        assert_eq!(before, bodies(&input)?);
    }
    Ok(())
}
