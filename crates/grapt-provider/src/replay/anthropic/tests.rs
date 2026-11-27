use super::*;
use crate::replay::ProviderReplay;
use crate::transport::fixtures::{anthropic_request, responses_request};
use crate::{CodexInputItem, CodexToolText, ProviderRequest, ToolName};

fn pair() -> (Value, Value) {
    (
        json!({"type":"server_tool_use","id":"srv_search","name":"web_search",
            "input":{"query":"fixture","extension":[true,{"opaque":"INPUT_SECRET"}]}}),
        json!({"type":"web_search_tool_result","tool_use_id":"srv_search","content":[{
            "type":"web_search_result","url":"https://example.com","title":"fixture",
            "encrypted_content":"RESULT_SECRET","page_age":null,"extension":{"nested":[1,2]}}]}),
    )
}

fn restore(state: &AdapterState) -> Result<ProviderReplay, Box<dyn std::error::Error>> {
    let request = anthropic_request();
    let model = request.model();
    Ok(ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        model.variant().cloned(),
        std::str::from_utf8(state.capture_bytes().ok_or("state")?)?,
    )?)
}

#[test]
fn native_search_and_cited_text_round_trip_every_opaque_field(
) -> Result<(), Box<dyn std::error::Error>> {
    let (server_use, result) = pair();
    let binding = ReplayBinding::fixture();
    let search = restore(&anthropic_search_state(&server_use, &result, &binding)?)?;
    assert!(search.is_anthropic_search());
    assert_eq!(
        search.native_search_call().map(ToolCallId::as_str),
        Some("srv_search")
    );
    assert_eq!(search.anthropic_block(), Some(json!([server_use, result])));
    assert_eq!(
        search.byte_len(),
        json!([server_use, result]).to_string().len()
    );
    let block = json!({"type":"text","text":"answer","extension":{"opaque":"TEXT_SECRET"},
        "citations":[{"type":"web_search_result_location","url":"https://example.com",
            "title":"fixture","cited_text":"quoted","encrypted_index":"INDEX_SECRET"}]});
    let rendered = "answer [fixture](https://example.com)";
    let state = anthropic_text_state(&block, rendered, &binding)?;
    let text = restore(&state)?;
    assert!(text.is_anthropic_text());
    assert_eq!(text.anthropic_rendered_text(), Some(rendered));
    assert_eq!(text.anthropic_block(), Some(block.clone()));
    assert_eq!(text.byte_len(), block.to_string().len() + rendered.len());
    assert!(!format!("{search:?} {text:?} {state:?}").contains("SECRET"));
    let digest = |replay: &ProviderReplay| {
        let mut fields = Vec::new();
        replay.digest_fields(&mut |field| fields.push(field.to_vec()));
        fields
    };
    let mut changed = block;
    changed["citations"][0]["encrypted_index"] = "OTHER_SECRET".into();
    assert_ne!(
        digest(&text),
        digest(&restore(&anthropic_text_state(
            &changed, rendered, &binding
        )?)?)
    );
    Ok(())
}

#[test]
fn native_restore_rejects_malformed_pairs_citations_and_opaque_bounds(
) -> Result<(), Box<dyn std::error::Error>> {
    let (server_use, result) = pair();
    let binding = ReplayBinding::fixture();
    for field in ["id", "name", "input"] {
        let mut bad = server_use.clone();
        bad[field] = Value::Null;
        assert!(anthropic_search_state(&bad, &result, &binding).is_err());
    }
    for field in ["tool_use_id", "type", "content"] {
        let mut bad = result.clone();
        bad[field] = Value::Null;
        assert!(anthropic_search_state(&server_use, &bad, &binding).is_err());
    }
    let mut bad = result.clone();
    bad["content"][0]["encrypted_content"] = "".into();
    assert!(anthropic_search_state(&server_use, &bad, &binding).is_err());
    for opaque in [json!("x".repeat(MAX_NATIVE_BYTES)), json!(vec![0; 1025])] {
        let mut bad = result.clone();
        bad["extension"] = opaque;
        let artifact = json!({"kind":"anthropic-web-search-v1",
            "item":[server_use,bad],"binding":binding.0})
        .to_string();
        assert!(ProviderReplay::restore(
            anthropic_request().model().provider().clone(),
            anthropic_request().model().model().clone(),
            None,
            &artifact
        )
        .is_err());
    }
    let error = json!({"type":"web_search_tool_result","tool_use_id":"srv_search",
        "content":{"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}});
    assert!(
        restore(&anthropic_search_state(&server_use, &error, &binding)?)?.is_anthropic_search()
    );
    let bad_text = json!({"type":"text","text":"answer","citations":[{
        "type":"web_search_result_location","url":"https://example.com",
        "title":"fixture","cited_text":"quoted"}]});
    assert!(anthropic_text_state(&bad_text, "answer", &binding).is_err());
    Ok(())
}

#[test]
fn native_search_projection_rejects_duplicate_calls_and_cross_protocol_replay(
) -> Result<(), Box<dyn std::error::Error>> {
    let (server_use, result) = pair();
    let replay = restore(&anthropic_search_state(
        &server_use,
        &result,
        &ReplayBinding::fixture(),
    )?)?;
    let ProviderRequest::AnthropicMessages(mut request) = anthropic_request() else {
        return Err("family".into());
    };
    request
        .messages
        .push(CodexInputItem::ProviderReplay(replay.clone()));
    assert!(crate::routes::body::anthropic_body(&request).is_ok());
    request
        .messages
        .push(CodexInputItem::ProviderReplay(replay.clone()));
    assert!(crate::routes::body::anthropic_body(&request).is_err());
    request.messages.pop();
    request.messages.push(CodexInputItem::ToolCall {
        call: ToolCallId::new("srv_search").ok_or("call")?,
        tool: ToolName::new("local").ok_or("tool")?,
        arguments: CodexToolText::try_new("{}".into())?,
    });
    request.messages.push(CodexInputItem::ToolOutput {
        call: ToolCallId::new("srv_search").ok_or("call")?,
        output: CodexToolText::try_new("done".into())?,
        is_error: false,
    });
    assert!(crate::routes::body::anthropic_body(&request).is_err());
    let ProviderRequest::OpenAiResponses(mut responses) = responses_request() else {
        return Err("family".into());
    };
    responses.model = request.model.clone();
    for replay in [
        replay,
        restore(&anthropic_text_state(
            &json!({"type":"text","text":"answer"}),
            "answer",
            &ReplayBinding::fixture(),
        )?)?,
    ] {
        responses.input = vec![CodexInputItem::ProviderReplay(replay)];
        assert!(crate::routes::body::openai_responses_body(&responses).is_err());
        assert!(
            crate::routes::request_validation::validate(&ProviderRequest::OpenAiResponses(
                responses.clone()
            ))
            .is_err()
        );
    }
    Ok(())
}
