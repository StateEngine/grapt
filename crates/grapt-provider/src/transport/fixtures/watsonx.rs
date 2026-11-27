//! Native IBM routing, project scope and tool decoding over actual HTTP.
use super::*;

#[tokio::test]
async fn native_chat_binds_project_and_refuses_missing_scope_before_send(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = serve(200, &[], vec![sse(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"read","arguments":"{\"path\":\"example.txt\"}"}}]}}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":8,"completion_tokens":1,"total_tokens":9}}"#,
    ])]).await;
    let request = || {
        crate::ProviderRequest::WatsonxChat(crate::OpenAiCompatibleRequest {
            reasoning_replay: Default::default(),
            model: model("watsonx", "ibm/granite-4-h-small"),
            messages: vec![user("Read example.txt").into()],
            max_tokens: crate::MaxOutputTokens::new(128),
            prompt_cache: crate::CompatiblePromptCache::Off,
            tools: vec![crate::ToolDefinition {
                name: "read".into(),
                description: None,
                input_schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
                input_format: crate::ToolInputFormat::Json,
            }],
            tool_choice: Some(crate::ToolChoice::Auto),
            routing_session: None,
        })
    };
    let adapter = adapter(1);
    assert!(adapter
        .stage(input(
            "att_missing_project",
            &fixture.base,
            ProtocolFamily::WatsonxChat,
            request()
        ))
        .is_err());
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    let mut scoped = input(
        "att_watsonx",
        &fixture.base,
        ProtocolFamily::WatsonxChat,
        request(),
    );
    scoped.endpoint = scoped.endpoint.with_watsonx_project(Some(
        grapt_types::WatsonxProjectId::new("fixture-project").ok_or("project")?,
    ));
    let fragments = drain(adapter.open(scoped).await?).await;
    assert_eq!(finish_of(&fragments), Some(FinishReason::ToolCalls));
    assert!(fragments
        .iter()
        .any(|fragment| matches!(fragment, AttemptFragment::ToolCalled { call, tool, input, .. } if call.as_str() == "att_watsonx-tool-0" && tool == "read" && input.contains("example.txt"))));
    let usage = fragments
        .iter()
        .find_map(|fragment| match fragment {
            AttemptFragment::ProviderTerminal { usage, .. } => usage.as_ref(),
            _ => None,
        })
        .ok_or("terminal usage")?;
    assert_eq!(usage.total().value(), Some(9));
    let head = fixture.head.lock().map_err(|_| "head lock")?.clone();
    assert!(head.contains("post /ml/v1/text/chat_stream?version=2026-04-20 "));
    assert!(head.contains("authorization: bearer token-value-never-printed"));
    let body: serde_json::Value =
        serde_json::from_str(&fixture.body.lock().map_err(|_| "body lock")?)?;
    assert_eq!(body["project_id"], "fixture-project");
    assert_eq!(body["model_id"], "ibm/granite-4-h-small");
    assert_eq!(body["parallel_tool_calls"], false);
    assert!(body["tools"]
        .as_array()
        .is_some_and(|tools| tools.len() == 1));
    for field in ["model", "stream", "stream_options"] {
        assert!(body.get(field).is_none());
    }
    Ok(())
}
