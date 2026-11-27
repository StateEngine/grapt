use super::super::fixtures::*;
use crate::fragment::AttemptFragment;
use crate::request::ProtocolFamily;

#[tokio::test]
async fn anthropic_overflow_is_typed_and_bounded_on_the_wire() {
    for (status, family, kind, message, expected) in [
        (
            400,
            ProtocolFamily::AnthropicMessages,
            "invalid_request_error",
            "prompt is too long",
            true,
        ),
        (
            413,
            ProtocolFamily::AnthropicMessages,
            "invalid_request_error",
            "prompt is too long: 213462 tokens > 200000 maximum",
            true,
        ),
        (
            422,
            ProtocolFamily::AnthropicMessages,
            "invalid_request_error",
            "prompt is too long: 200001 tokens > 200000 maximum",
            true,
        ),
        (
            400,
            ProtocolFamily::AnthropicMessages,
            "invalid_request_error",
            "max_tokens must be positive SCC_PRIVATE",
            false,
        ),
        (
            401,
            ProtocolFamily::AnthropicMessages,
            "invalid_request_error",
            "prompt is too long",
            false,
        ),
        (
            500,
            ProtocolFamily::AnthropicMessages,
            "invalid_request_error",
            "prompt is too long",
            false,
        ),
        (
            400,
            ProtocolFamily::AnthropicMessages,
            "authentication_error",
            "prompt is too long",
            false,
        ),
        (
            400,
            ProtocolFamily::OpenAiResponses,
            "invalid_request_error",
            "prompt is too long",
            false,
        ),
    ] {
        let body = serde_json::json!({"error":{"type":kind,"message":message}}).to_string();
        let fixture = serve(status, &[], vec![body.into_bytes()]).await;
        let adapter = adapter(2);
        let request = if family == ProtocolFamily::AnthropicMessages {
            anthropic_request()
        } else {
            // Every non-Anthropic row here exists to prove a route does NOT
            // read Anthropic's prose; the Responses family is that route's
            // real owner, so it keeps the discriminator meaningful.
            responses_request()
        };
        let open = adapter
            .open(input("att_rel05", &fixture.base, family, request))
            .await
            .unwrap_or_else(|error| panic!("fixture open failed: {error}"));
        let fragments = drain(open).await;
        assert!(fragments
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::ProviderFailure { .. })));
        let classification = fragments.iter().find_map(|fragment| match fragment {
            AttemptFragment::ProviderFailure { classification, .. } => *classification,
            _ => None,
        });
        assert_eq!(
            classification,
            expected.then_some(crate::FailureClassification::ContextOverflow),
            "{status} {family:?} {kind} {message}"
        );
        // The vendor's reason is quoted for a server error as well as a client
        // error now: the row explains the refusal by default, and only masking
        // and the excerpt bound constrain it.
        let diagnostic = format!("{fragments:?}");
        assert!(
            diagnostic.contains(message),
            "{status} {message}: the row must quote the vendor's reason"
        );
        assert_eq!(adapter.active_len(), 0);
    }
}

#[tokio::test]
async fn anthropic_incomplete_or_oversized_errors_do_not_authorize_rebuild() {
    for body in [
        r#"{"error":{"type":"invalid_request_error","message":"prompt is too long"}"#.to_owned(),
        serde_json::json!({"error":{"type":"invalid_request_error","message":format!("prompt is too long: {}", "x".repeat(16384))}}).to_string(),
    ] {
        let fixture = serve(400, &[], vec![body.into_bytes()]).await;
        let adapter = adapter(2);
        let open = adapter.open(input("att_rel05_bound", &fixture.base, ProtocolFamily::AnthropicMessages, anthropic_request())).await
            .unwrap_or_else(|error| panic!("fixture open failed: {error}"));
        let fragments = drain(open).await;
        assert!(fragments.iter().any(|fragment| matches!(fragment, AttemptFragment::ProviderFailure { classification: None, .. })));
        assert_eq!(adapter.active_len(), 0);
    }
}
