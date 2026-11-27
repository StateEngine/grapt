//! Vertex uses Google's bearer and resource path with the native Claude decoder.
use super::*;

#[tokio::test]
async fn streams_native_claude_through_vertex() {
    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":5}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
            r#"{"type":"message_stop"}"#,
        ])],
    )
    .await;
    let crate::ProviderRequest::AnthropicMessages(mut request) = anthropic_request() else {
        panic!("native Anthropic fixture");
    };
    request.model = model("google-vertex-anthropic", "claude-sonnet-4-5@20250929");
    let base = format!(
        "{}/v1/projects/project-a/locations/global/publishers/anthropic/models",
        fixture.base
    );
    let open = adapter(4)
        .open(input(
            "att_vertex_claude",
            &base,
            ProtocolFamily::VertexAnthropic,
            crate::ProviderRequest::VertexAnthropic(request),
        ))
        .await
        .unwrap_or_else(|error| panic!("open: {error}"));
    let fragments = drain(open).await;
    assert_eq!(text_of(&fragments), "Hi");
    assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
    let head = fixture
        .head
        .lock()
        .unwrap_or_else(|error| panic!("head lock: {error}"));
    assert!(head.contains("authorization: bearer token-value-never-printed"));
    assert!(!head.contains("x-api-key:"));
    assert!(
        head.contains("/publishers/anthropic/models/claude-sonnet-4-5@20250929:streamrawpredict")
    );
    let body = fixture
        .body
        .lock()
        .unwrap_or_else(|error| panic!("body lock: {error}"));
    let body: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|error| panic!("body JSON: {error}"));
    assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
    assert!(body.get("model").is_none());
}
