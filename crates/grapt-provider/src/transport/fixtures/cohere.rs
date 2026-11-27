use super::*;

#[tokio::test]
async fn cohere_timeout_before_output_remains_retryable() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"type":"message-start","delta":{"message":{"role":"assistant","content":[]}}}"#,
            r#"{"type":"message-end","delta":{"finish_reason":"TIMEOUT"}}"#,
        ])],
    )
    .await;
    let request = crate::ProviderRequest::CohereChat(crate::CohereChatRequest {
        model: model("cohere", "command-a-03-2025"),
        messages: vec![user("Hello").into()],
        max_tokens: None,
        tools: Vec::new(),
        tool_choice: None,
    });
    let fragments = drain(
        adapter(1)
            .open(input(
                "cohere-timeout",
                &fixture.base,
                ProtocolFamily::CohereChat,
                request,
            ))
            .await?,
    )
    .await;
    assert!(text_of(&fragments).is_empty());
    let failure = failure_of(&fragments).ok_or("timeout failure")?;
    assert!(
        failure.is_retryable(),
        "provider timeout must remain retryable: {failure:?}"
    );
    Ok(())
}
