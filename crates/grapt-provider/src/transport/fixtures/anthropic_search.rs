use super::*;
use crate::{CodexInputItem, ProviderReplay, ProviderRequest, ToolExecution};
use serde_json::json;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn anthropic_search_round_trips_server_blocks_and_citations_without_local_results(
) -> Result<(), Box<dyn std::error::Error>> {
    for failed in [false, true] {
        let result = json!({"type":"web_search_tool_result", "tool_use_id":"srvtoolu_search",
            "content":if failed { json!({"type":"web_search_tool_result_error","error_code":"max_uses_exceeded"}) }
            else { json!([{"type":"web_search_result","url":"https://example.com/source",
                "title":"Source", "encrypted_content":"OPAQUE_RESULT", "page_age":"recent"}]) }});
        let cited = json!({"type":"text","text":"An answer.","citations":[{
            "type":"web_search_result_location","url":"https://example.com/source","title":"Source",
            "encrypted_index":"OPAQUE_INDEX","cited_text":"A fact."}]});
        let mut repeated = cited["citations"][0].clone();
        repeated["encrypted_index"] = "OPAQUE_SECOND_INDEX".into();
        let mut expected_cited = cited.clone();
        expected_cited["text"] = "An answer. Another claim.".into();
        expected_cited["citations"]
            .as_array_mut()
            .ok_or("citations")?
            .push(repeated.clone());
        let events = [
            json!({"type":"message_start","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":98}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_search","name":"web_search","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{ \"query\": \"a question\" }"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":1,"content_block":result}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_start","index":2,"content_block":cited}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":" Another claim."}}),
            json!({"type":"content_block_delta","index":2,"delta":{"type":"citations_delta","citation":repeated}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}),
            json!({"type":"message_stop"}),
        ].map(|event| event.to_string());
        let fixture = serve(
            200,
            &[],
            vec![sse(&events.iter().map(String::as_str).collect::<Vec<_>>())],
        )
        .await;
        let mut request = anthropic_request();
        let ProviderRequest::AnthropicMessages(native) = &mut request else {
            return Err("protocol".into());
        };
        native.web_search = Some(crate::AnthropicWebSearchSpec::live(1)?);
        native.prompt_cache = crate::AnthropicPromptCache::StablePrefix;
        let model = native.model.clone();
        let fragments = drain(
            adapter(1)
                .open(input(
                    "pat_search",
                    &fixture.base,
                    ProtocolFamily::AnthropicMessages,
                    request.clone(),
                ))
                .await?,
        )
        .await;
        assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
        assert_eq!(
            fragments
                .iter()
                .filter(|f| matches!(f, AttemptFragment::ToolCalled {
            execution:ToolExecution::ProviderHosted,tool,..} if tool=="web_search"))
                .count(),
            1
        );
        assert!(!fragments.iter().any(|f| matches!(
            f,
            AttemptFragment::ToolCalled {
                execution: ToolExecution::Local,
                ..
            }
        )));
        assert_eq!(
            fragments
                .iter()
                .any(|f| matches!(f, AttemptFragment::HostedToolFailed { .. })),
            failed
        );
        assert!(text_of(&fragments).contains("https://example.com/source"));
        assert_eq!(
            text_of(&fragments)
                .matches("https://example.com/source")
                .count(),
            2
        );
        assert!(!text_of(&fragments).contains("OPAQUE"));
        let mut replayed = Vec::new();
        for fragment in &fragments {
            let state = match fragment {
                AttemptFragment::HostedToolResult { state, .. }
                | AttemptFragment::HostedToolFailed { state, .. }
                | AttemptFragment::TextClosed { state, .. } => state,
                _ => continue,
            };
            if let Some(bytes) = state.capture_bytes() {
                replayed.push(CodexInputItem::ProviderReplay(ProviderReplay::restore(
                    model.provider().clone(),
                    model.model().clone(),
                    model.variant().cloned(),
                    std::str::from_utf8(bytes)?,
                )?));
            }
        }
        let ProviderRequest::AnthropicMessages(native) = &mut request else {
            return Err("protocol".into());
        };
        native.messages.extend(replayed);
        native.messages.push(user("continue").into());
        let second = adapter(1)
            .open(input(
                "pat_replay",
                &fixture.base,
                ProtocolFamily::AnthropicMessages,
                request,
            ))
            .await?;
        assert_eq!(finish_of(&drain(second).await), Some(FinishReason::Stop));
        let body: serde_json::Value =
            serde_json::from_str(&fixture.body.lock().map_err(|_| "body lock")?)?;
        let content = body["messages"][1]["content"]
            .as_array()
            .ok_or("assistant content")?;
        assert_eq!(
            content
                .iter()
                .find(|b| b["type"] == "web_search_tool_result"),
            Some(&result)
        );
        assert_eq!(
            content.iter().find(|b| b["type"] == "text"),
            Some(&expected_cited)
        );
        assert!(!content
            .iter()
            .any(|b| b["type"] == "tool_result" || b["type"] == "tool_use"));
    }
    Ok(())
}
