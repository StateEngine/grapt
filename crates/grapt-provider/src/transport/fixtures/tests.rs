//! Wire witnesses: the real client, the real staging path, the real capacity
//! registry, real chunk boundaries.

use super::*;
use crate::fragment::{FinishReason, ProviderFailureClass};

#[path = "renewal.rs"]
mod renewal;

#[path = "replay.rs"]
mod replay;

#[path = "timing.rs"]
mod timing;

#[path = "vertex_anthropic.rs"]
mod vertex_anthropic;

#[path = "anthropic_usage.rs"]
mod anthropic_usage;

#[path = "routing_headers.rs"]
mod routing_headers;

#[path = "cohere.rs"]
mod cohere;
#[path = "usage_exhaustion.rs"]
mod usage_exhaustion;

fn failure_of(fragments: &[AttemptFragment]) -> Option<ProviderFailureClass> {
    fragments.iter().find_map(|fragment| match fragment {
        AttemptFragment::ProviderFailure { class, .. } => Some(class.clone()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Each family, end to end over a socket
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anthropic_streams_over_the_wire_with_its_own_headers() {
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

    let adapter = adapter(4);
    let open = match adapter
        .open(input(
            "att_anthropic",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    assert_eq!(text_of(&fragments), "Hi");
    assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));

    // Headers read off the WIRE, not off the builder that made them. This is
    // the difference between asserting the code and asserting the request.
    let head = match fixture.head.lock() {
        Ok(head) => head.clone(),
        Err(_) => panic!("fixture head lock"),
    };
    assert!(
        head.contains("x-api-key: token-value-never-printed"),
        "Anthropic authenticates with x-api-key, not a bearer token: {head}"
    );
    assert!(
        !head.contains("authorization:"),
        "an Anthropic request must not also carry a bearer header: {head}"
    );
    assert!(
        head.contains("anthropic-version: 2023-06-01"),
        "the Messages API requires its version header: {head}"
    );
    assert!(head.contains("post /messages"), "wrong path: {head}");
}

#[tokio::test]
async fn a_compatible_endpoint_streams_and_carries_a_bearer_token() {
    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"choices":[{"delta":{"content":"Grok"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
        ])],
    )
    .await;

    let adapter = adapter(4);
    let open = match adapter
        .open(input(
            "att_compat",
            &fixture.base,
            ProtocolFamily::OpenAiCompatible,
            compat_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    assert_eq!(text_of(&fragments), "Grok");
    assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));

    // The whole point of the retained-finish design, witnessed over a socket:
    // the usage chunk arrives AFTER the finish chunk, and the terminal still
    // carries it.
    let usage = fragments.iter().find_map(|fragment| match fragment {
        AttemptFragment::ProviderTerminal { usage, .. } => usage.as_ref(),
        _ => None,
    });
    let usage = match usage {
        Some(usage) => usage,
        None => panic!("the terminal must carry the trailing usage"),
    };
    assert_eq!(usage.total().value(), Some(4));

    let head = match fixture.head.lock() {
        Ok(head) => head.clone(),
        Err(_) => panic!("fixture head lock"),
    };
    assert!(head.contains("authorization: bearer token-value-never-printed"));
    assert!(
        head.contains("post /chat/completions"),
        "wrong path: {head}"
    );
    assert!(
        !head.contains("anthropic-version"),
        "a compatible endpoint must not receive Anthropic's version header: {head}"
    );

    // `stream_options.include_usage` must actually be on the wire, or the
    // trailing usage chunk this test depends on would never be sent by a real
    // server.
    let body = match fixture.body.lock() {
        Ok(body) => body.clone(),
        Err(_) => panic!("fixture body lock"),
    };
    assert!(body.contains("include_usage"), "body was: {body}");
}

#[tokio::test]
async fn the_openai_responses_route_streams_and_posts_to_responses() {
    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"type":"response.output_text.delta","delta":"ok"}"#,
            r#"{"type":"response.completed","response":{}}"#,
        ])],
    )
    .await;

    let adapter = adapter(4);
    let open = match adapter
        .open(input(
            "att_responses",
            &fixture.base,
            ProtocolFamily::OpenAiResponses,
            responses_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    assert_eq!(text_of(&fragments), "ok");

    let head = match fixture.head.lock() {
        Ok(head) => head.clone(),
        Err(_) => panic!("fixture head lock"),
    };
    assert!(head.contains("post /responses"), "wrong path: {head}");
    // The public Responses route sends NO ChatGPT-backend headers: those are
    // the Codex arm's, and sending an account id to api.openai.com would be
    // leaking an account identifier to a service that never asked for it.
    assert!(!head.contains("chatgpt-account-id"), "{head}");
    assert!(!head.contains("originator"), "{head}");
    assert!(!head.contains("thread-id"), "{head}");
    assert!(!head.contains("x-client-request-id"), "{head}");
    assert!(!head.contains("\r\nversion:"), "{head}");
}

/// The actual HTTP header follows the session cache-affinity wire shape in
/// `codex-api/src/requests/headers.rs::build_session_headers` at bb64b894.
#[tokio::test]
async fn the_codex_route_names_its_session_for_cache_affinity() {
    let key = match crate::PromptCacheKey::try_new("0123456789abcdef".to_owned()) {
        Ok(key) => key,
        Err(error) => panic!("cache key: {error}"),
    };
    for (cache, expected) in [(Some(key), true), (None, false)] {
        let fixture = serve(
            200,
            &[],
            vec![sse(&[r#"{"type":"response.completed","response":{}}"#])],
        )
        .await;
        let request = crate::request::ProviderRequest::ChatGptCodex(crate::ChatGptCodexRequest {
            model: model("openai", "gpt-5"),
            input: vec![user("Hello").into()],
            prompt_cache_key: cache,
            tools: Vec::new(),
            web_search: None,
            tool_choice: None,
            reasoning: Default::default(),
            turn: Default::default(),
        });
        let open = match adapter(4)
            .open(input(
                "att_codex_affinity",
                &fixture.base,
                ProtocolFamily::ChatGptCodex,
                request,
            ))
            .await
        {
            Ok(open) => open,
            Err(error) => panic!("open: {error}"),
        };
        let _ = drain(open).await;
        let head = match fixture.head.lock() {
            Ok(head) => head.clone(),
            Err(_) => panic!("fixture head lock"),
        };
        assert_eq!(
            head.contains("session-id: 0123456789abcdef"),
            expected,
            "{head}"
        );
        assert!(!head.contains("session_id:"), "{head}");
        assert!(head.contains("originator: secode\r\n"), "{head}"); // grapt-rename: keep
        assert!(head.contains("version: 0.159.0\r\n"), "{head}");
        assert!(
            head.contains("user-agent: secode-fixture/0.1\r\n"),
            "{head}"
        );
    }
}

// ---------------------------------------------------------------------------
// Chunk boundaries on the shared routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_character_split_across_transport_chunks_survives() {
    // "é" is two bytes; the fixture writes them in separate chunks, so a
    // decoder calling from_utf8 per chunk reports malformed
    // provider output on an ordinary transport event.
    let payload = sse(&[
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"café"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_stop"}"#,
    ]);
    let split = payload
        .iter()
        .position(|byte| *byte == 0xC3)
        .unwrap_or(payload.len() / 2)
        + 1;
    let (first, second) = payload.split_at(split);

    let fixture = serve(200, &[], vec![first.to_vec(), second.to_vec()]).await;
    let adapter = adapter(2);
    let open = match adapter
        .open(input(
            "att_split",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    assert_eq!(
        text_of(&fragments),
        "café",
        "a code point split across chunks must be carried, not refused"
    );
    assert_eq!(failure_of(&fragments), None);
}

#[tokio::test]
async fn crlf_framed_events_are_decoded_over_the_wire() {
    // A standards-valid `\r\n\r\n` delimiter must frame on a shared route.
    let mut payload = String::new();
    payload.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"crlf\"}}]}\r\n\r\n");
    payload.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\n");

    let fixture = serve(200, &[], vec![payload.into_bytes()]).await;
    let adapter = adapter(2);
    let open = match adapter
        .open(input(
            "att_crlf",
            &fixture.base,
            ProtocolFamily::OpenAiCompatible,
            compat_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    assert_eq!(text_of(&fragments), "crlf");
    assert_eq!(finish_of(&fragments), Some(FinishReason::Stop));
}

// ---------------------------------------------------------------------------
// Status classification and rate-limit metadata
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_429_carries_its_retry_after_and_stays_retryable() {
    let fixture = serve(429, &[("retry-after", "17")], Vec::new()).await;
    let adapter = adapter(2);
    let open = match adapter
        .open(input(
            "att_429",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    match failure_of(&fragments) {
        Some(ProviderFailureClass::RateLimit { retry_after }) => {
            assert_eq!(
                retry_after,
                Some(Duration::from_secs(17)),
                "the provider's own delay must reach the retry policy"
            );
        }
        other => panic!("a 429 must be a rate limit carrying its delay: {other:?}"),
    }
    // The slot must be free: a pre-stream failure that leaked capacity would
    // starve the adapter after a handful of rate limits.
    assert_eq!(adapter.active_len(), 0);
}

#[tokio::test]
async fn a_401_is_terminal_and_no_stream_is_opened() {
    let fixture = serve(401, &[], Vec::new()).await;
    let adapter = adapter(2);
    let open = match adapter
        .open(input(
            "att_401",
            &fixture.base,
            ProtocolFamily::OpenAiCompatible,
            compat_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    let class = match failure_of(&fragments) {
        Some(class) => class,
        None => panic!("a 401 must produce a failure"),
    };
    assert_eq!(class, ProviderFailureClass::Authentication);
    assert!(
        !class.is_retryable(),
        "a rejected credential must not be retried; refresh belongs to the credential owner"
    );
    assert_eq!(adapter.active_len(), 0);
}

#[tokio::test]
async fn an_error_body_reaches_the_failure_only_after_masking() {
    // A refusal states the vendor's reason, never the credential or another
    // secret-shaped value the body echoes.
    let fixture = serve(
        403,
        &[],
        vec![b"{\"error\":{\"message\":\"account is suspended for token-value-never-printed; Authorization: Bearer echoed-bearer sk-echoed-key\"}}".to_vec()],
    )
    .await;
    let adapter = adapter(2);
    let open = match adapter
        .open(input(
            "att_body",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    let message = fragments.iter().find_map(|fragment| match fragment {
        AttemptFragment::ProviderFailure { message, .. } => Some(message.clone()),
        _ => None,
    });
    let message = match message {
        Some(message) => message,
        None => panic!("a 403 must produce a failure"),
    };
    assert!(message.contains("account is suspended"), "{message}");
    for secret in ["token-value", "echoed-bearer", "sk-echoed-key"] {
        assert!(
            !message.contains(secret),
            "{secret} reached the failure: {message}"
        );
    }
}

#[tokio::test]
async fn a_refusal_keeps_only_the_vendors_machine_words() {
    // The volatile refusal diagnostic carries the status and the vendor's
    // error type and code, each only as a known word; a secret in the code
    // becomes `unlisted`, and the message, which echoes it too, reaches
    // neither the diagnostic nor the durable failure.
    #[derive(Debug)]
    struct Clock;
    impl grapt_types::ClockAuthority for Clock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(0)
        }
    }
    let fixture = serve(
        400,
        &[],
        vec![br#"{"type":"error","error":{"type":"invalid_request_error","code":"sk-secret-xyz","message":"key sk-secret-xyz is bad"}}"#.to_vec()],
    )
    .await;
    let (observer, samples) = crate::DispatchObserver::bounded(std::sync::Arc::new(Clock));
    let adapter = adapter(2).with_dispatch(observer);
    let open = match adapter
        .open(input(
            "att_refused",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let fragments = drain(open).await;
    let refusal = match samples.try_next_refusal() {
        Ok(refusal) => refusal,
        Err(error) => panic!("a 400 must be reported as a refusal: {error}"),
    };
    assert_eq!(refusal.attempt.as_str(), "att_refused");
    assert_eq!(refusal.status, 400);
    assert_eq!(
        refusal.error_type.as_ref().map(crate::VendorCode::as_str),
        Some("invalid_request_error")
    );
    assert_eq!(
        refusal.error_code.as_ref().map(crate::VendorCode::as_str),
        Some("unlisted")
    );
    assert!(!format!("{refusal:?}").contains("sk-secret"), "{refusal:?}");
    assert!(
        !format!("{fragments:?}").contains("sk-secret"),
        "the error body must not travel into the failure"
    );
}

// ---------------------------------------------------------------------------
// Capacity, cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dropping_a_live_stream_releases_its_slot() {
    // Cancellation, as the runtime performs it: the supervisor drops the
    // reader. Nothing else is called, so release must be a Drop obligation.
    let fixture = serve(
        200,
        &[],
        vec![sse(&[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#,
        ])],
    )
    .await;

    let adapter = adapter(1);
    let open = match adapter
        .open(input(
            "att_cancel",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await
    {
        Ok(open) => open,
        Err(error) => panic!("open: {error}"),
    };
    let mut stream = match open {
        ProviderOpen::Streaming(stream) => stream,
        ProviderOpen::Failed(fragment) | ProviderOpen::FailedWithDetail { fragment, .. } => {
            panic!("expected a stream: {fragment:?}")
        }
    };
    // Read one fragment so the stream is genuinely live and mid-response.
    let _ = stream.next_fragment().await;
    assert_eq!(adapter.active_len(), 1, "held while the stream is running");

    drop(stream);
    assert_eq!(
        adapter.active_len(),
        0,
        "an abandoned stream must free its slot without a call the caller could forget"
    );

    // And the capacity is genuinely reusable afterwards.
    let reopened = adapter
        .open(input(
            "att_cancel_2",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await;
    assert!(reopened.is_ok(), "the freed slot must be reusable");
}

#[tokio::test]
async fn one_attempt_id_cannot_open_two_concurrent_streams() {
    // Contract rule 1, on the general adapter: one PhysicalAttempt performs
    // exactly one provider stream call.
    let fixture = serve(200, &[], vec![sse(&[r#"{"type":"ping"}"#])]).await;
    let adapter = adapter(4);
    let first = adapter
        .open(input(
            "att_dup",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await;
    let _held = match first {
        Ok(ProviderOpen::Streaming(stream)) => stream,
        other => panic!("expected a live stream: {other:?}"),
    };
    let second = adapter
        .open(input(
            "att_dup",
            &fixture.base,
            ProtocolFamily::AnthropicMessages,
            anthropic_request(),
        ))
        .await;
    assert!(
        second.is_err(),
        "a second concurrent stream under one attempt id must be refused"
    );
}

#[tokio::test]
async fn a_request_whose_family_disagrees_with_its_endpoint_is_refused_before_any_byte() {
    let fixture = serve(200, &[], vec![sse(&[r#"{"type":"message_stop"}"#])]).await;
    let adapter = adapter(2);

    // An Anthropic body against an endpoint bound to the compatible family.
    let mut mismatched = input(
        "att_mismatch",
        &fixture.base,
        ProtocolFamily::OpenAiCompatible,
        anthropic_request(),
    );
    // The lease placement follows the endpoint's family; the request's does not.
    mismatched.request = anthropic_request();

    let refused = adapter.open(mismatched).await;
    assert!(
        refused.is_err(),
        "an Anthropic body must not be posted to a chat-completions path"
    );
    assert_eq!(
        fixture.requests.load(Ordering::SeqCst),
        0,
        "the refusal must happen before any byte leaves"
    );
    assert_eq!(adapter.active_len(), 0, "and must not leak the slot");
}

#[tokio::test]
async fn scc_http_overflow_code_reaches_the_live_adapter_and_large_errors_refuse_classification() {
    for oversized in [false, true] {
        let body = serde_json::json!({"error":{"code":"context_length_exceeded","message":if oversized {"x".repeat(16384)} else {"SCC_PRIVATE".to_owned()}}}).to_string();
        let fixture = serve(400, &[], vec![body.into_bytes()]).await;
        let adapter = adapter(2);
        let open = adapter
            .open(input(
                "att_scc_http",
                &fixture.base,
                ProtocolFamily::OpenAiCompatible,
                compat_request(),
            ))
            .await
            .unwrap_or_else(|error| panic!("fixture open failed: {error}"));
        let fragments = drain(open).await;
        let classified = fragments.iter().find_map(|fragment| match fragment {
            AttemptFragment::ProviderFailure { classification, .. } => *classification,
            _ => None,
        });
        assert_eq!(
            classified,
            if oversized {
                None
            } else {
                Some(crate::FailureClassification::ContextOverflow)
            }
        );
        assert_eq!(format!("{fragments:?}").contains("SCC_PRIVATE"), !oversized);
        assert_eq!(adapter.active_len(), 0);
    }
}

#[path = "turn_tests.rs"]
mod turn_tests;

#[path = "capture.rs"]
mod capture;

#[path = "error_text.rs"]
mod error_text;

#[path = "terminal_usage.rs"]
mod terminal_usage;

#[path = "origin_binding.rs"]
mod origin_binding;

#[path = "watsonx.rs"]
mod watsonx;

#[cfg(feature = "loopback-testkit")]
#[path = "cloudflare.rs"]
mod cloudflare;

#[tokio::test]
async fn default_failure_text_masks_every_basic_scheme_case() {
    for (scheme, token) in [
        ("Basic", "dXNlcjpwYXNz"),
        ("basic", "dXNlcjpwYXNz"),
        ("BASIC", "dXNlcjpwYXNz"),
        ("Basic", "YTpi"),
        ("basic", "YTpi"),
        ("BASIC", "YTpi"),
        ("basic", "ajph"),
    ] {
        let body = format!(
            "credential rejected; send {scheme} {token} to retry; Basic authentication; Basic auth"
        );
        let fixture = serve(403, &[], vec![body.into_bytes()]).await;
        let adapter = adapter(2);
        let opened = adapter
            .open(input(
                "att_basic_case",
                &fixture.base,
                ProtocolFamily::AnthropicMessages,
                anthropic_request(),
            ))
            .await
            .unwrap_or_else(|error| panic!("open: {error}"));
        let fragments = drain(opened).await;
        let message = fragments
            .iter()
            .find_map(|fragment| match fragment {
                AttemptFragment::ProviderFailure { message, .. } => Some(message),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a 403 must produce a failure"));
        assert!(
            message.contains("credential rejected"),
            "the refusal reason was lost"
        );
        assert!(message.contains("Basic authentication"));
        assert!(message.contains("Basic auth"));
        assert!(
            !message.contains(token),
            "encoded credentials reached the default failure text for {scheme}"
        );
    }
}
