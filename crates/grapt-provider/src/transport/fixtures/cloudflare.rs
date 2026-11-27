//! Scoped gateway requests reach the real adapter with their own authentication.
use super::*;
use grapt_types::cloudflare::{
    CloudflareAccountId, CloudflareGatewayId, CloudflareGatewayScope, CloudflareProviderId,
};

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn cloudflare_three_wire_modes_preserve_scoped_authentication_and_destination(
) -> Result<(), Box<dyn std::error::Error>> {
    for (provider, family, model_id, wire_model, payload) in [
        (
            CloudflareProviderId::Openai,
            ProtocolFamily::OpenAiResponses,
            "openai/gpt-5",
            "gpt-5",
            vec![
                r#"{"type":"response.output_text.delta","delta":"ok"}"#,
                r#"{"type":"response.completed","response":{}}"#,
            ],
        ),
        (
            CloudflareProviderId::Anthropic,
            ProtocolFamily::AnthropicMessages,
            "anthropic/claude-sonnet-4.5",
            "claude-sonnet-4-5",
            vec![
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
                r#"{"type":"content_block_stop","index":0}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
                r#"{"type":"message_stop"}"#,
            ],
        ),
        (
            CloudflareProviderId::Xai,
            ProtocolFamily::OpenAiCompatible,
            "xai/grok-4",
            "xai/grok-4",
            vec![
                r#"{"choices":[{"delta":{"content":"ok"}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            ],
        ),
    ] {
        let fixture = serve(200, &[], vec![sse(&payload)]).await;
        let scope = CloudflareGatewayScope::new(
            CloudflareAccountId::try_from("a".repeat(32))?,
            CloudflareGatewayId::try_from("fixture-gateway".to_owned())?,
            provider,
        );
        let base = format!("{}{}", fixture.base, scope.api_path());
        let mut request = match family {
            ProtocolFamily::OpenAiResponses => responses_request(),
            ProtocolFamily::AnthropicMessages => anthropic_request(),
            ProtocolFamily::OpenAiCompatible => compat_request(),
            _ => return Err("unsupported fixture family".into()),
        };
        let selected = model("cloudflare-ai-gateway", model_id);
        match &mut request {
            crate::ProviderRequest::OpenAiResponses(request) => request.model = selected,
            crate::ProviderRequest::AnthropicMessages(request) => request.model = selected,
            crate::ProviderRequest::OpenAiCompatible(request) => request.model = selected,
            _ => return Err("unsupported fixture request".into()),
        }
        let mut call = input("att_gateway", &base, family, request);
        call.endpoint = crate::EndpointConfig::new()
            .with_configured_profile(&base)
            .with_cloudflare(scope.clone())
            .resolve(family)?
            .bind(family, route());
        let placement =
            crate::credential_placement(family, Some(&scope), crate::MessagesAuthMode::ApiKey)
                .ok_or("gateway placement")?;
        let credential = crate::ProviderCredential::inject(
            call.lease.account().clone(),
            call.lease.credential_version().clone(),
            placement,
            grapt_types::Secret::new("synthetic-gateway-token".into()),
        )?
        .bind_destination(&base)?
        .bind_cloudflare(scope.clone())?;
        call.lease = credential.lease()?;
        let adapter = adapter(1);
        let fragments = drain(adapter.open(call).await?).await;
        assert_eq!(text_of(&fragments), "ok", "{provider:?}");
        assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
        let head = fixture.head.lock().map_err(|_| "head lock")?.clone();
        let body = fixture.body.lock().map_err(|_| "body lock")?.clone();
        assert!(head.starts_with(&format!(
            "post {}{} ",
            scope.api_path(),
            scope.request_path()
        )));
        let gateway_header = provider != CloudflareProviderId::Xai;
        assert_eq!(
            head.matches("cf-aig-authorization: bearer synthetic-gateway-token")
                .count(),
            usize::from(gateway_header)
        );
        assert_eq!(
            head.lines()
                .filter(|line| line.starts_with("authorization:"))
                .count(),
            usize::from(!gateway_header)
        );
        assert_eq!(
            head.matches("\r\nauthorization: bearer synthetic-gateway-token")
                .count(),
            usize::from(!gateway_header)
        );
        assert!(!head.contains("x-api-key:"));
        assert_eq!(
            head.contains("anthropic-version: 2023-06-01"),
            provider == CloudflareProviderId::Anthropic
        );
        let parsed: serde_json::Value = serde_json::from_str(&body)?;
        assert_eq!(parsed["model"], wire_model);
        assert!(!body.contains("synthetic-gateway-token"));

        // The capability checks the final URL even after endpoint resolution.
        let client = crate::transport::client::build_loopback_client(deadlines())?;
        let foreign = client.post(format!("{}/foreign/messages", fixture.base));
        assert!(credential.lease()?.place(foreign).is_err());
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        assert_eq!(adapter.active_len(), 0);
    }
    Ok(())
}
