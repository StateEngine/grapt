use super::support::*;
use super::*;

#[tokio::test]
async fn a_text_stream_normalizes_into_the_alphabet_and_validates() {
    let base = serve_once(TEXT_STREAM, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter.open(input_for(&base, "att_text")).await {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    let mut stream = match opened {
        ProviderOpen::Streaming(stream) => stream,
        ProviderOpen::Failed(fragment) | ProviderOpen::FailedWithDetail { fragment, .. } => {
            panic!("expected a stream, got {fragment:?}")
        }
    };
    let fragments = drain(&mut stream).await;

    // Every fragment must be legal at its position — the adapter is checked
    // by the crate's own validator, not by this test's opinion.
    let mut validator = StreamValidator::new();
    for fragment in &fragments {
        if validator.accept(fragment).is_err() {
            panic!("the adapter emitted an illegal sequence: {fragments:?}");
        }
    }
    assert!(
        matches!(fragments.first(), Some(AttemptFragment::AttemptOpened)),
        "{fragments:?}"
    );
    assert!(
        matches!(
            fragments.last(),
            Some(AttemptFragment::ProviderTerminal {
                finish: FinishReason::Stop,
                ..
            })
        ),
        "{fragments:?}"
    );
    let text: String = fragments
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::TextDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn an_incomplete_response_is_length_and_closes_its_open_block() {
    let base = serve_once(LENGTH_STREAM, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter.open(input_for(&base, "att_len")).await {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    let mut stream = match opened {
        ProviderOpen::Streaming(stream) => stream,
        ProviderOpen::Failed(fragment) | ProviderOpen::FailedWithDetail { fragment, .. } => {
            panic!("expected a stream, got {fragment:?}")
        }
    };
    let fragments = drain(&mut stream).await;
    assert!(
        fragments
            .iter()
            .any(|fragment| matches!(fragment, AttemptFragment::TextClosed { .. })),
        "an open text block must be closed before the terminal: {fragments:?}"
    );
    assert!(
        matches!(
            fragments.last(),
            Some(AttemptFragment::ProviderTerminal {
                finish: FinishReason::Length,
                ..
            })
        ),
        "{fragments:?}"
    );
}

#[tokio::test]
async fn a_tool_call_fails_closed_rather_than_being_dropped() {
    let base = serve_once(TOOL_STREAM, "HTTP/1.1 200 OK").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter.open(input_for(&base, "att_tool")).await {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    let mut stream = match opened {
        ProviderOpen::Streaming(stream) => stream,
        ProviderOpen::Failed(fragment) | ProviderOpen::FailedWithDetail { fragment, .. } => {
            panic!("expected a stream, got {fragment:?}")
        }
    };
    let fragments = drain(&mut stream).await;
    // The decisive assertion is that the stream did NOT reach a successful
    // terminal: a dropped tool call would look like a clean stop.
    assert!(
        !fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderTerminal {
                finish: FinishReason::Stop,
                ..
            }
        )),
        "an undecodable tool call must never produce a clean stop: {fragments:?}"
    );
    assert!(
        fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderFailure {
                class: ProviderFailureClass::InvalidProviderOutput,
                ..
            }
        )),
        "{fragments:?}"
    );
}

#[tokio::test]
async fn a_401_is_terminal_and_issues_no_second_request() {
    let base = serve_once("", "HTTP/1.1 401 Unauthorized").await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter.open(input_for(&base, "att_401")).await {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    match opened {
        ProviderOpen::Failed(fragment) => match *fragment {
            AttemptFragment::ProviderFailure { class, message, .. } => {
                assert_eq!(class, ProviderFailureClass::Authentication);
                assert!(!class.is_retryable(), "401 is terminal");
                assert!(
                    !message.contains("Bearer") && !message.contains("token-value"),
                    "no credential material may reach a diagnostic: {message}"
                );
            }
            other => panic!("expected an authentication failure, got {other:?}"),
        },
        other => panic!("expected an authentication failure, got {other:?}"),
    }
    // The failed open released its slot, so the registry is empty and the
    // same id may legitimately be admitted again — the adapter bounds
    // CONCURRENT use, not lifetime use (see the ProviderAdapter type note).
    assert_eq!(adapter.active_len(), 0);
}

#[tokio::test]
async fn a_429_carries_its_retry_after_and_stays_retryable() {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => panic!("loopback bind: {error}"),
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("loopback addr: {error}"),
    };
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut scratch = [0_u8; 4096];
            let _ = socket.read(&mut scratch).await;
            let _ = socket
                    .write_all(
                        b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 7\r\ncontent-length: 0\r\n\r\n",
                    )
                    .await;
            let _ = socket.flush().await;
        }
    });
    let base = format!("http://{address}");
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter.open(input_for(&base, "att_429")).await {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    match opened {
        ProviderOpen::Failed(fragment) => match *fragment {
            AttemptFragment::ProviderFailure { class, .. } => {
                assert_eq!(
                    class,
                    ProviderFailureClass::RateLimit {
                        retry_after: Some(Duration::from_secs(7))
                    }
                );
                assert!(class.is_retryable());
            }
            other => panic!("expected a rate-limit failure, got {other:?}"),
        },
        other => panic!("expected a rate-limit failure, got {other:?}"),
    }
}

#[tokio::test]
async fn a_refused_connection_is_a_retryable_transport_failure() {
    // Port 1 on loopback is not listening; nothing leaves the machine.
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter
        .open(input_for("http://127.0.0.1:1", "att_refused"))
        .await
    {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    match opened {
        ProviderOpen::Failed(fragment) => match *fragment {
            AttemptFragment::ProviderFailure { class, .. } => {
                assert_eq!(class, ProviderFailureClass::Transport);
                assert!(
                    class.is_retryable(),
                    "a pre-output transport failure retries"
                );
            }
            other => panic!("expected a transport failure, got {other:?}"),
        },
        other => panic!("expected a transport failure, got {other:?}"),
    }
}

#[tokio::test]
async fn an_eof_without_a_terminal_never_becomes_a_clean_stop() {
    let base = serve_once(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"half\"}\n\n",
        "HTTP/1.1 200 OK",
    )
    .await;
    let adapter = match loopback_adapter(deadlines(), limits()) {
        Ok(adapter) => adapter,
        Err(error) => panic!("loopback adapter: {error}"),
    };
    let opened = match adapter.open(input_for(&base, "att_eof")).await {
        Ok(opened) => opened,
        Err(error) => panic!("open: {error}"),
    };
    let mut stream = match opened {
        ProviderOpen::Streaming(stream) => stream,
        ProviderOpen::Failed(fragment) | ProviderOpen::FailedWithDetail { fragment, .. } => {
            panic!("expected a stream, got {fragment:?}")
        }
    };
    let fragments = drain(&mut stream).await;
    assert!(
        fragments.iter().any(|fragment| matches!(
            fragment,
            AttemptFragment::ProviderFailure {
                class: ProviderFailureClass::Transport,
                ..
            }
        )),
        "a truncated stream is a typed failure, not a success: {fragments:?}"
    );
}

#[test]
fn usage_absence_is_not_zero() {
    let empty = serde_json::json!({ "usage": {} });
    let usage = match usage_from(&empty) {
        Ok(Some(usage)) => usage,
        Ok(None) => panic!("a present usage object decodes"),
        Err(error) => panic!("empty usage object is valid: {error}"),
    };
    assert_eq!(usage.input_total().value(), None, "absent is not zero");
    assert_eq!(usage.input_fresh().value(), None);
    let full = serde_json::json!({
        "usage": {
            "input_tokens": 10,
            "input_tokens_details": { "cached_tokens": 4, "cache_write_tokens": 1 },
            "output_tokens": 2,
            "total_tokens": 12
        }
    });
    let usage = match usage_from(&full) {
        Ok(Some(usage)) => usage,
        Ok(None) => panic!("usage decodes"),
        Err(error) => panic!("complete usage is valid: {error}"),
    };
    assert_eq!(usage.input_total().value(), Some(10));
    assert_eq!(usage.cache_read().value(), Some(4));
    assert_eq!(usage.cache_write().value(), Some(1));
    assert_eq!(usage.input_fresh().value(), Some(5));

    for details in [
        serde_json::json!({"cached_tokens": 4}),
        serde_json::json!({"cache_write_tokens": 4}),
    ] {
        let partial = serde_json::json!({
            "usage": {"input_tokens": 10, "input_tokens_details": details}
        });
        let usage = match usage_from(&partial) {
            Ok(Some(usage)) => usage,
            Ok(None) => panic!("a present usage object decodes"),
            Err(error) => panic!("partial usage is valid: {error}"),
        };
        assert_eq!(
            usage.input_fresh().value(),
            None,
            "partial cache details invented fresh input"
        );
    }

    let inconsistent = serde_json::json!({
        "usage": {
            "input_tokens": 10,
            "input_tokens_details": {"cached_tokens": 8, "cache_write_tokens": 3}
        }
    });
    let error = match usage_from(&inconsistent) {
        Err(error) => error,
        Ok(_) => panic!("an over-partitioned cache observation must refuse"),
    };
    assert_eq!(
        error.reason(),
        &crate::usage::UsageNormalizationReason::PartitionExceedsTotal,
        "a cache partition above inclusive input must fail as a partition contradiction"
    );
}
