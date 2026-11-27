//! Native AWS wire fixture, including byte-split frames and late usage.
use super::*;

fn frame(kind: &str, body: serde_json::Value) -> Vec<u8> {
    let mut headers = Vec::new();
    for (name, value) in [(":message-type", "event"), (":event-type", kind)] {
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7);
        headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
    }
    let body = body.to_string();
    let mut frame = Vec::new();
    frame.extend_from_slice(&((16 + headers.len() + body.len()) as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    frame.extend_from_slice(&checksum(&frame).to_be_bytes());
    frame.extend(headers);
    frame.extend_from_slice(body.as_bytes());
    frame.extend_from_slice(&checksum(&frame).to_be_bytes());
    frame
}
fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn native_bedrock_preserves_tools_late_usage_and_frame_integrity(
) -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::json;
    let mut bytes = Vec::new();
    for (kind, body) in [
        ("messageStart", json!({"role":"assistant"})),
        (
            "contentBlockDelta",
            json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"Plan"}}}),
        ),
        (
            "contentBlockDelta",
            json!({"contentBlockIndex":0,"delta":{"reasoningContent":{"signature":"synthetic-bedrock-signature"}}}),
        ),
        ("contentBlockStop", json!({"contentBlockIndex":0})),
        (
            "contentBlockDelta",
            json!({"contentBlockIndex":1,"delta":{"reasoningContent":{"redactedContent":"AA=="}}}),
        ),
        (
            "contentBlockDelta",
            json!({"contentBlockIndex":1,"delta":{"reasoningContent":{"redactedContent":"AQI="}}}),
        ),
        ("contentBlockStop", json!({"contentBlockIndex":1})),
        (
            "contentBlockDelta",
            json!({"contentBlockIndex":4,"delta":{"reasoningContent":{"text":"Unsigned"}}}),
        ),
        ("contentBlockStop", json!({"contentBlockIndex":4})),
        (
            "contentBlockDelta",
            json!({"contentBlockIndex":2,"delta":{"text":"Café"}}),
        ),
        ("contentBlockStop", json!({"contentBlockIndex":2})),
        (
            "contentBlockStart",
            json!({"contentBlockIndex":3,"start":{"toolUse":{"toolUseId":"call-1","name":"lookup"}}}),
        ),
        (
            "contentBlockDelta",
            json!({"contentBlockIndex":3,"delta":{"toolUse":{"input":"{\"query\":\"ok\"}"}}}),
        ),
        ("contentBlockStop", json!({"contentBlockIndex":3})),
        ("messageStop", json!({"stopReason":"tool_use"})),
        (
            "metadata",
            json!({"usage":{"inputTokens":5,"outputTokens":3,"cacheReadInputTokens":7,"cacheWriteInputTokens":0,"totalTokens":15}}),
        ),
    ] {
        bytes.extend(frame(kind, body));
    }
    for corrupted in [false, true] {
        let mut wire = bytes.clone();
        if corrupted {
            wire[8] ^= 1;
        }
        let fixture = serve(
            200,
            &[("content-type", "application/vnd.amazon.eventstream")],
            wire.chunks(7).map(<[u8]>::to_vec).collect(),
        )
        .await;
        let request = crate::ProviderRequest::BedrockConverse(crate::BedrockConverseRequest {
            model: model("amazon-bedrock", "anthropic.claude-sonnet-5-5"),
            messages: vec![user("Hello").into()],
            max_tokens: crate::MaxOutputTokens::new(128).ok_or("output bound")?,
            tools: vec![crate::ToolDefinition { name:"lookup".into(), description:None, input_schema:r#"{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}"#.into(), input_format:Default::default() }],
            tool_choice: Some(crate::ToolChoice::Auto),
        });
        let provider = adapter(1);
        let fragments = drain(
            provider
                .open(input(
                    "bedrock-native",
                    &fixture.base,
                    ProtocolFamily::BedrockConverse,
                    request.clone(),
                ))
                .await?,
        )
        .await;
        let head = fixture.head.lock().map_err(|_| "fixture head")?.clone();
        assert!(head.starts_with("post /model/anthropic.claude-sonnet-5-5/converse-stream "));
        assert!(head.to_ascii_lowercase().contains("authorization: bearer"));
        let body: serde_json::Value =
            serde_json::from_str(&fixture.body.lock().map_err(|_| "fixture body")?)?;
        assert_eq!(body["messages"][0]["content"][0]["text"], "Hello");
        assert_eq!(body["toolConfig"]["tools"][0]["toolSpec"]["name"], "lookup");
        assert!(body.get("stream").is_none());
        assert_eq!(
            body["additionalModelRequestFields"]["thinking"]["block_binding"]
                ["prefix_mismatch_behavior"],
            "drop_block"
        );
        if corrupted {
            assert!(fragments.iter().any(|fragment| matches!(
                fragment,
                AttemptFragment::ProviderFailure {
                    class: crate::ProviderFailureClass::InvalidProviderOutput,
                    ..
                }
            )));
        } else {
            assert_eq!(text_of(&fragments), "Café");
            assert_eq!(finish_of(&fragments), Some(FinishReason::ToolCalls));
            let usage = fragments
                .iter()
                .find_map(|f| match f {
                    AttemptFragment::ProviderTerminal { usage, .. } => usage.as_ref(),
                    _ => None,
                })
                .ok_or("terminal usage")?;
            assert_eq!(usage.input_total().value(), Some(12));
            assert_eq!(usage.output().value(), Some(3));
            assert_eq!(usage.cache_read().value(), Some(7));
            assert!(fragments
                .iter()
                .any(|f| matches!(f, AttemptFragment::ToolCalled { .. })));
            let mut followup = request;
            let selected = followup.model().clone();
            let crate::ProviderRequest::BedrockConverse(native) = &mut followup else {
                return Err("family".into());
            };
            for fragment in &fragments {
                if let AttemptFragment::ReasoningClosed { state, .. } = fragment {
                    if !state.is_present() {
                        continue;
                    }
                    let replay = crate::ProviderReplay::restore(
                        selected.provider().clone(),
                        selected.model().clone(),
                        selected.variant().cloned(),
                        std::str::from_utf8(
                            state.capture_bytes().ok_or("missing reasoning artifact")?,
                        )?,
                    )?;
                    native
                        .messages
                        .push(crate::CodexInputItem::ProviderReplay(replay));
                }
            }
            let call = grapt_types::ToolCallId::new("call-1").ok_or("call")?;
            native.messages.extend([
                crate::CodexInputItem::Message(crate::InputMessage {
                    role: crate::MessageRole::Assistant,
                    content: "Café".into(),
                }),
                crate::CodexInputItem::ToolCall {
                    call: call.clone(),
                    tool: crate::ToolName::new("lookup").ok_or("tool")?,
                    arguments: crate::CodexToolText::try_new(r#"{"query":"ok"}"#.into())?,
                },
                crate::CodexInputItem::ToolOutput {
                    call,
                    output: crate::CodexToolText::try_new("OK".into())?,
                    is_error: false,
                },
            ]);
            native.tool_choice = Some(crate::ToolChoice::None);
            let next = drain(
                provider
                    .open(input(
                        "bedrock-replay",
                        &fixture.base,
                        ProtocolFamily::BedrockConverse,
                        followup.clone(),
                    ))
                    .await?,
            )
            .await;
            assert_eq!(finish_of(&next), Some(FinishReason::ToolCalls));
            let body: serde_json::Value =
                serde_json::from_str(&fixture.body.lock().map_err(|_| "fixture body")?)?;
            assert_eq!(
                body["messages"][1]["content"][0]["reasoningContent"]["reasoningText"]["signature"],
                "synthetic-bedrock-signature"
            );
            assert_eq!(
                body["messages"][1]["content"][1]["reasoningContent"]["redactedContent"],
                "AAEC"
            );
            assert!(body["toolConfig"]["tools"].is_array());
            assert!(body["toolConfig"].get("toolChoice").is_none());
            let mut changed = input(
                "bedrock-rekey",
                &fixture.base,
                ProtocolFamily::BedrockConverse,
                followup,
            );
            changed.lease = CredentialLease::issue(
                grapt_types::AccountId::new("acct_fixture").ok_or("account")?,
                grapt_types::CredentialVersion::new("8").ok_or("version")?,
                CredentialPlacement::BearerAuthorization,
                grapt_types::Secret::new("synthetic-changed-key".into()),
            )?;
            drain(provider.open(changed).await?).await;
            assert_eq!(fixture.requests.load(Ordering::SeqCst), 3);
            let body: serde_json::Value =
                serde_json::from_str(&fixture.body.lock().map_err(|_| "fixture body")?)?;
            assert!(!body.to_string().contains("reasoningContent"));
        }
    }
    Ok(())
}

mod auth;
