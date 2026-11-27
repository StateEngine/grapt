//! History growth reaches the real transport without weakening shape validation.
use super::*;
use crate::{CodexInputItem, ProviderRequest, ToolDefinition, ToolInputFormat};
use serde_json::Value;

fn history(request: &mut ProviderRequest) -> &mut Vec<CodexInputItem> {
    match request {
        ProviderRequest::OpenAiResponses(request) => &mut request.input,
        ProviderRequest::AnthropicMessages(request) => &mut request.messages,
        _ => panic!("unsupported growth fixture family"),
    }
}

async fn receive_request(listener: TcpListener, anthropic: bool) -> Value {
    let (mut socket, _) = listener
        .accept()
        .await
        .unwrap_or_else(|e| panic!("accept: {e}"));
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    let (start, length) = loop {
        let n = socket
            .read(&mut buffer)
            .await
            .unwrap_or_else(|e| panic!("head: {e}"));
        assert!(n > 0 && raw.len() + n <= 32 * 1024 * 1024);
        raw.extend_from_slice(&buffer[..n]);
        if let Some(end) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let head =
                std::str::from_utf8(&raw[..end]).unwrap_or_else(|e| panic!("head UTF8: {e}"));
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_else(|| panic!("content length missing"));
            assert!(length <= 32 * 1024 * 1024);
            break (end + 4, length);
        }
    };
    while raw.len() - start < length {
        let n = socket
            .read(&mut buffer)
            .await
            .unwrap_or_else(|e| panic!("body: {e}"));
        assert!(n > 0 && raw.len() + n <= 32 * 1024 * 1024);
        raw.extend_from_slice(&buffer[..n]);
    }
    let request = serde_json::from_slice(&raw[start..start + length])
        .unwrap_or_else(|e| panic!("body JSON: {e}"));
    let body = if anthropic {
        sse(&[
            r#"{"type":"message_start","message":{"id":"reply","type":"message","role":"assistant","content":[],"model":"fixture","usage":{"input_tokens":1,"output_tokens":0}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
            r#"{"type":"message_stop"}"#,
        ])
    } else {
        sse(&[
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
        ])
    };
    let head = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    socket
        .write_all(head.as_bytes())
        .await
        .unwrap_or_else(|e| panic!("reply: {e}"));
    socket
        .write_all(&body)
        .await
        .unwrap_or_else(|e| panic!("reply body: {e}"));
    request
}

fn occurrences(value: &Value, needle: &str) -> usize {
    match value {
        Value::String(text) => text.matches(needle).count(),
        Value::Array(values) => values.iter().map(|v| occurrences(v, needle)).sum(),
        Value::Object(values) => values.values().map(|v| occurrences(v, needle)).sum(),
        _ => 0,
    }
}

#[tokio::test]
async fn history_growth_crosses_the_real_transport_without_trimming() {
    exercise_growth(false).await;
}

#[tokio::test]
async fn item_growth_crosses_the_real_transport_without_trimming() {
    exercise_growth(true).await;
}

async fn exercise_growth(many_items: bool) {
    for anthropic in [false, true] {
        let mut request = if anthropic {
            anthropic_request()
        } else {
            responses_request()
        };
        let (count, text) = if many_items {
            (4_097, "growth-marker".to_owned())
        } else {
            (17, "growth-marker".repeat(85_000))
        };
        *history(&mut request) = vec![user(&text).into(); count];
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|e| panic!("bind: {e}"));
        let base = format!(
            "http://{}",
            listener
                .local_addr()
                .unwrap_or_else(|e| panic!("addr: {e}"))
        );
        let peer = tokio::spawn(receive_request(listener, anthropic));
        let family = request.family();
        let adapter = adapter(1);
        let open = adapter.open(input("growth-wire", &base, family, request));
        let open = tokio::time::timeout(Duration::from_secs(10), open)
            .await
            .unwrap_or_else(|e| panic!("open timeout: {e}"))
            .unwrap_or_else(|e| panic!("growth refused before transport: {e}"));
        let fragments = drain(open).await;
        assert!(fragments
            .iter()
            .any(|f| matches!(f, AttemptFragment::ProviderTerminal { .. })));
        assert!(!fragments
            .iter()
            .any(|f| matches!(f, AttemptFragment::ProviderFailure { .. })));
        let received = tokio::time::timeout(Duration::from_secs(10), peer)
            .await
            .unwrap_or_else(|e| panic!("peer timeout: {e}"))
            .unwrap_or_else(|e| panic!("peer: {e}"));
        assert_eq!(
            occurrences(&received, "growth-marker"),
            count * if many_items { 1 } else { 85_000 }
        );
    }
}

#[test]
fn malformed_shapes_still_refuse_staging() {
    for schema in ["[", "[]"] {
        let mut request = responses_request();
        let ProviderRequest::OpenAiResponses(responses) = &mut request else {
            panic!("family")
        };
        responses.tools.push(ToolDefinition {
            name: "read".into(),
            description: None,
            input_schema: schema.into(),
            input_format: ToolInputFormat::Json,
        });
        assert_refused(request);
    }
    let mut request = responses_request();
    *history(&mut request) = vec![
        CodexInputItem::AssistantResponseStart,
        CodexInputItem::ToolOutput {
            call: grapt_types::ToolCallId::new("unmatched").unwrap_or_else(|| panic!("call")),
            output: crate::CodexToolText::try_new("result".into())
                .unwrap_or_else(|e| panic!("text: {e}")),
            is_error: false,
        },
    ];
    assert_refused(request);
}

fn assert_refused(request: ProviderRequest) {
    assert!(adapter(1)
        .stage(input(
            "invalid-growth-shape",
            "http://127.0.0.1:1",
            request.family(),
            request
        ))
        .is_err());
}
