//! Captured signed bytes survive a second actual HTTP request.
use super::*;
use crate::{CodexInputItem, CodexToolText, ProviderReplay, ProviderRequest, ToolName};

#[tokio::test]
async fn gemini_keeps_signed_text_separate_from_adjacent_unsigned_parts(
) -> Result<(), Box<dyn std::error::Error>> {
    for family in [ProtocolFamily::GoogleGemini, ProtocolFamily::VertexGemini] {
        let fixture = serve(200, &[], vec![sse(&[
        r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Public explanation."},{"text":"Signed context.","thoughtSignature":"synthetic-signature"}]},"finishReason":"STOP"}]}"#,
    ])]).await;
        let base = if family == ProtocolFamily::VertexGemini {
            format!(
                "{}/v1/projects/project-a/locations/global/publishers/google",
                fixture.base
            )
        } else {
            fixture.base.clone()
        };
        let provider = adapter(1);
        let selected = model(
            if family == ProtocolFamily::VertexGemini {
                "google-vertex"
            } else {
                "google"
            },
            "gemini-3-flash-preview",
        );
        let request = |native| {
            if family == ProtocolFamily::VertexGemini {
                ProviderRequest::VertexGemini(native)
            } else {
                ProviderRequest::GoogleGemini(native)
            }
        };
        let mut native = crate::GoogleGeminiRequest {
            model: selected.clone(),
            messages: vec![user("Hello").into()],
            max_tokens: None,
            tools: Vec::new(),
            tool_choice: None,
        };
        let first = provider
            .open(input(
                "gemini-first",
                &base,
                family,
                request(native.clone()),
            ))
            .await?;
        let fragments = drain(first).await;
        assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
        assert_eq!(text_of(&fragments), "Public explanation.Signed context.");
        let bytes = fragments
            .iter()
            .find_map(|fragment| match fragment {
                AttemptFragment::TextClosed { state, .. } => state.capture_bytes(),
                _ => None,
            })
            .ok_or("signed text state")?;
        let replay = ProviderReplay::restore(
            selected.provider().clone(),
            selected.model().clone(),
            None,
            std::str::from_utf8(bytes)?,
        )?;
        native.messages.extend([
            CodexInputItem::Message(crate::InputMessage {
                role: crate::MessageRole::Assistant,
                content: "Public explanation.".into(),
            }),
            CodexInputItem::ProviderReplay(replay),
            user("Continue").into(),
        ]);
        let second = provider
            .open(input("gemini-second", &base, family, request(native)))
            .await?;
        assert_eq!(finish_of(&drain(second).await), Some(FinishReason::Stop));
        let head = fixture.head.lock().map_err(|_| "header lock")?.clone();
        assert!(head.starts_with(if family == ProtocolFamily::VertexGemini {
            "post /v1/projects/project-a/locations/global/publishers/google/models/gemini-3-flash-preview:streamgeneratecontent?alt=sse "
        } else { "post /models/gemini-3-flash-preview:streamgeneratecontent?alt=sse " }));
        assert!(head.contains(if family == ProtocolFamily::VertexGemini {
            "authorization: bearer "
        } else {
            "x-goog-api-key: "
        }));
        let raw = fixture.body.lock().map_err(|_| "body lock")?.clone();
        let body: serde_json::Value = serde_json::from_str(&raw)?;
        let signed = body["contents"][1]["parts"]
            .as_array()
            .ok_or("model parts")?
            .iter()
            .find(|part| part.get("thoughtSignature").is_some())
            .ok_or("signed part on follow-up")?;
        // The signature belongs to its original part, never the neighboring text.
        assert_eq!(signed["text"], "Signed context.");
        assert_eq!(signed["thoughtSignature"], "synthetic-signature");
    }
    Ok(())
}

#[tokio::test]
async fn pc_signed_tool_followup_posts_exact_signature_and_call_result_order(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = serve(200, &[], vec![sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"inspect"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"PC_WIRE_SIGNATURE"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_pc_wire","name":"read","input":{}}}"#,
        r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\"}"}}"#,
        r#"{"type":"content_block_stop","index":1}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
        r#"{"type":"message_stop"}"#,
    ])]).await;
    let provider = adapter(1);
    let first = provider
        .open(input(
            "pc-wire-first",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await?;
    let fragments = drain(first).await;
    assert_eq!(finish_of(&fragments), Some(FinishReason::ToolCalls));
    assert!(!format!("{fragments:?}").contains("PC_WIRE_SIGNATURE"));
    let bytes = fragments
        .iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ReasoningClosed { state, .. } => state.capture_bytes(),
            _ => None,
        })
        .ok_or("signed state")?;
    let mut request = anthropic_request();
    let ProviderRequest::AnthropicMessages(anthropic) = &mut request else {
        return Err("family".into());
    };
    let model = &anthropic.model;
    let replay = ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        model.variant().cloned(),
        std::str::from_utf8(bytes)?,
    )?;
    let call = grapt_types::ToolCallId::new("call_pc_wire").ok_or("call")?;
    anthropic.messages.extend([
        CodexInputItem::ProviderReplay(replay),
        CodexInputItem::ToolCall {
            call: call.clone(),
            tool: ToolName::new("read").ok_or("tool")?,
            arguments: CodexToolText::try_new(r#"{"path":"a"}"#.into())?,
        },
        CodexInputItem::ToolOutput {
            call,
            output: CodexToolText::try_new("read failed".into())?,
            is_error: true,
        },
    ]);
    let second = provider
        .open(input(
            "pc-wire-followup",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            request,
        ))
        .await?;
    assert_eq!(
        finish_of(&drain(second).await),
        Some(FinishReason::ToolCalls)
    );
    let raw = fixture.body.lock().map_err(|_| "body lock")?.clone();
    let body: serde_json::Value = serde_json::from_str(&raw)?;
    assert_eq!(
        body["messages"][1]["content"][0],
        serde_json::json!({"type":"thinking","thinking":"inspect","signature":"PC_WIRE_SIGNATURE"})
    );
    assert_eq!(body["messages"][1]["content"][1]["id"], "call_pc_wire");
    assert_eq!(
        body["messages"][2]["content"][0]["tool_use_id"],
        "call_pc_wire"
    );
    assert_eq!(body["messages"][2]["content"][0]["is_error"], true);
    assert!(!raw.contains("binding"));
    Ok(())
}
