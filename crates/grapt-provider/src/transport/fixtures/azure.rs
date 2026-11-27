//! Azure v1's deployment name and API prefix through the native Responses client.
use super::*;

#[tokio::test]
async fn azure_deployments_stream_over_the_native_responses_route(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = serve(200, &[], vec![sse(&[
        r#"{"type":"response.output_text.delta","delta":"Hello"}"#,
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15}}}"#,
    ])]).await;
    let mut request = responses_request();
    let crate::ProviderRequest::OpenAiResponses(native) = &mut request else {
        return Err("fixture requires native Responses".into());
    };
    native.model = model("azure", "coding-deployment");
    let open = adapter(1)
        .open(input(
            "att_azure",
            &format!("{}/openai/v1", fixture.base),
            ProtocolFamily::OpenAiResponses,
            request,
        ))
        .await?;
    let fragments = drain(open).await;
    assert_eq!(text_of(&fragments), "Hello");
    assert!(fragments
        .iter()
        .any(|fragment| matches!(fragment, AttemptFragment::ProviderTerminal { .. })));
    let head = fixture.head.lock().map_err(|_| "fixture head lock")?;
    assert!(head.starts_with("post /openai/v1/responses "));
    assert!(head.contains("authorization: bearer token-value-never-printed"));
    let body = fixture.body.lock().map_err(|_| "fixture body lock")?;
    let body: serde_json::Value = serde_json::from_str(&body)?;
    assert_eq!(body["model"], "coding-deployment");
    assert_eq!(body["stream"], true);
    Ok(())
}
