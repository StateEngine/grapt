//! Deterministic fixture servers, and the wire witnesses that drive them.
//!
//! `#[cfg(test)]`. Every byte these servers send is a byte this crate authored;
//! none is a capture of a live provider, and none is described as one. What
//! they establish is that the SAME decoders the unit witnesses drive also work
//! when the bytes arrive over a socket in arbitrary chunk boundaries, through
//! the real client, the real staging path and the real capacity registry.
//!
//! # What only a wire fixture can witness
//!
//! - The headers each family actually places, read off the request line.
//! - Chunk boundaries falling inside a UTF-8 character and inside an SSE frame
//!   delimiter, where independent chunk decoding and framing can fail.
//! - A capacity slot released on a real terminal, and on a real cancellation.
//! - Capture receiving the transport's bytes rather than the decoder's.
//! - A non-success status becoming a classified failure before any stream.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::codex::{CodexDeadlines, CodexLimits};
use crate::credential::{CredentialLease, CredentialPlacement};
use crate::fragment::{AttemptFragment, FinishReason};
use crate::request::ProtocolFamily;
use crate::route::ProviderEndpoint;
use crate::transport::adapter::{AttemptInput, ProviderAdapter, ProviderOpen};
use crate::transport::capture::{capture_exchange, AttemptCapture, CaptureEvent, CaptureOutcome};

/// What one fixture server observed and will reply with.
pub(crate) struct Fixture {
    pub(crate) base: String,
    /// The full request head the client sent, so header placement is witnessed
    /// from the wire rather than from the code that built it.
    pub(crate) head: Arc<std::sync::Mutex<String>>,
    pub(crate) body: Arc<std::sync::Mutex<String>>,
    pub(crate) requests: Arc<AtomicUsize>,
}

/// Start a one-shot server that replies with `status` and `chunks`.
///
/// The chunks are written with a flush between each, so the client genuinely
/// sees separate transport reads — which is what lets a fixture split a code
/// point or a frame delimiter across a boundary.
pub(crate) async fn serve(status: u16, headers: &[(&str, &str)], chunks: Vec<Vec<u8>>) -> Fixture {
    serve_sequence(status, headers, vec![chunks]).await
}

/// Reply in order, retaining the last reply for existing repeat-request cases.
async fn serve_sequence(
    status: u16,
    headers: &[(&str, &str)],
    replies: Vec<Vec<Vec<u8>>>,
) -> Fixture {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => panic!("fixture listener: {error}"),
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(error) => panic!("fixture addr: {error}"),
    };
    let head = Arc::new(std::sync::Mutex::new(String::new()));
    let body = Arc::new(std::sync::Mutex::new(String::new()));
    let requests = Arc::new(AtomicUsize::new(0));

    let content_type = if headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    {
        ""
    } else {
        "content-type: text/event-stream\r\n"
    };
    let extra: String = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let (head_w, body_w, requests_w) = (head.clone(), body.clone(), requests.clone());

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let ordinal = requests_w.fetch_add(1, Ordering::SeqCst);
            let Some(chunks) = replies.get(ordinal).or_else(|| replies.last()) else {
                return;
            };

            // Read the request head, then whatever body the content-length
            // announced. Enough to witness headers and body without a full
            // HTTP implementation.
            let mut raw = Vec::new();
            let mut buffer = [0_u8; 2048];
            while let Ok(read) = socket.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let (request_head, rest) = text.split_at(split);
                    let rest = rest.trim_start_matches("\r\n\r\n");
                    let length = request_head
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))
                        })
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if rest.len() >= length {
                        if let Ok(mut slot) = head_w.lock() {
                            slot.clone_from(&request_head.to_lowercase());
                        }
                        if let Ok(mut slot) = body_w.lock() {
                            *slot = rest.to_owned();
                        }
                        break;
                    }
                }
            }

            let reason = if status == 200 { "OK" } else { "ERR" };
            let response =
                format!("HTTP/1.1 {status} {reason}\r\n{content_type}{extra}transfer-encoding: chunked\r\n\r\n");
            if socket.write_all(response.as_bytes()).await.is_err() {
                continue;
            }
            for chunk in chunks {
                let framed = format!("{:x}\r\n", chunk.len());
                if socket.write_all(framed.as_bytes()).await.is_err() {
                    break;
                }
                if socket.write_all(chunk).await.is_err() {
                    break;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    break;
                }
                let _ = socket.flush().await;
                // A real boundary between reads, so a split character or a
                // split delimiter is genuinely split at the transport.
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
            let _ = socket.flush().await;
        }
    });

    Fixture {
        base: format!("http://127.0.0.1:{port}"),
        head,
        body,
        requests,
    }
}

fn deadlines() -> CodexDeadlines {
    match CodexDeadlines::new(Duration::from_secs(2), Duration::from_secs(5)) {
        Ok(deadlines) => deadlines,
        Err(error) => panic!("test deadlines: {error}"),
    }
}

fn limits(capacity: usize) -> CodexLimits {
    match std::num::NonZeroUsize::new(capacity) {
        Some(capacity) => CodexLimits::new(capacity),
        None => panic!("a non-zero test capacity"),
    }
}

/// Drain one attempt feed to completion.
pub(crate) fn drain_capture(announced: &AttemptCapture) -> Vec<CaptureEvent> {
    let mut events = Vec::new();
    while let Ok(event) = announced.feed().try_recv() {
        events.push(event);
    }
    events
}

/// The captured bytes, concatenated, and the settlement if one arrived.
pub(crate) fn captured(
    events: &[CaptureEvent],
) -> (
    Vec<u8>,
    Option<(CaptureOutcome, crate::transport::capture::CaptureLoss)>,
) {
    let mut bytes = Vec::new();
    let mut settled = None;
    for event in events {
        match event {
            CaptureEvent::Chunk(chunk) => bytes.extend_from_slice(chunk),
            CaptureEvent::Finished { outcome, loss } => settled = Some((*outcome, *loss)),
        }
    }
    (bytes, settled)
}

pub(crate) fn adapter(capacity: usize) -> ProviderAdapter {
    ProviderAdapter::new(
        crate::transport::client::build_loopback_client,
        deadlines(),
        limits(capacity).active_attempts(),
    )
}

fn lease(placement: CredentialPlacement) -> CredentialLease {
    let (account, version) = match (
        grapt_types::AccountId::new("acct_fixture"),
        grapt_types::CredentialVersion::new("7"),
    ) {
        (Some(account), Some(version)) => (account, version),
        _ => panic!("fixture credential coordinates"),
    };
    match CredentialLease::issue(
        account,
        version,
        placement,
        grapt_types::Secret::new("token-value-never-printed".to_owned()),
    ) {
        Ok(lease) => lease,
        Err(error) => panic!("fixture lease: {error}"),
    }
}

fn route() -> grapt_types::ModelRouteId {
    match grapt_types::ModelRouteId::new("route_fixture") {
        Some(route) => route,
        None => panic!("fixture route"),
    }
}

fn model(vendor: &str, name: &str) -> grapt_types::ModelRef {
    use grapt_types::{
        BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
        ModelRef, ProviderId,
    };
    let (provider, model, base, family, version) = match (
        ProviderId::new(vendor),
        ModelId::new(name),
        BaseModelId::new(name),
        ModelFamilyId::new(vendor),
        ModelCatalogVersion::new("fixture-catalog"),
    ) {
        (Some(p), Some(m), Some(b), Some(f), Some(v)) => (p, m, b, f, v),
        _ => panic!("fixture model coordinates"),
    };
    let record = ModelCatalogRecord::new(provider.clone(), model.clone(), base, family, None);
    let catalog = match ModelCatalog::load(version, vec![record]) {
        Some(catalog) => catalog,
        None => panic!("fixture catalog"),
    };
    match catalog.entry(&provider, &model, None) {
        Some(entry) => ModelRef::from_catalog(entry),
        None => panic!("fixture catalog entry"),
    }
}

/// Build an attempt input directly.
///
/// The carrier's binding law is witnessed by `crate::opener::tests` and
/// `crate::authority::tests`; these fixtures exercise the wire below it, so
/// they assemble the input rather than re-proving binding.
pub(crate) fn input(
    id: &str,
    base: &str,
    family: ProtocolFamily,
    request: crate::request::ProviderRequest,
) -> AttemptInput {
    let base = match crate::route::LoopbackBase::parse(base) {
        Ok(base) => base,
        Err(error) => panic!("fixture loopback base: {error}"),
    };
    let endpoint = ProviderEndpoint::loopback(&base, family, route());
    let attempt = match grapt_types::PhysicalAttemptId::new(id) {
        Some(attempt) => attempt,
        None => panic!("fixture attempt id"),
    };
    AttemptInput {
        attempt,
        endpoint,
        lease: lease(crate::transport::adapter::placement_for(family)),
        request,
        user_agent: "secode-fixture/0.1".to_owned(),
    }
}

pub(crate) fn user(text: &str) -> crate::request::InputMessage {
    crate::request::InputMessage {
        role: crate::request::MessageRole::User,
        content: text.to_owned(),
    }
}

pub(crate) fn anthropic_request() -> crate::request::ProviderRequest {
    let max_tokens = match crate::request::MaxOutputTokens::new(1024) {
        Some(bound) => bound,
        None => panic!("a non-zero bound"),
    };
    crate::request::ProviderRequest::AnthropicMessages(crate::request::AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![user("Hello").into()],
        max_tokens,
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: Vec::new(),
        tool_choice: None,
    })
}

pub(crate) fn compat_request() -> crate::request::ProviderRequest {
    crate::request::ProviderRequest::OpenAiCompatible(crate::request::OpenAiCompatibleRequest {
        reasoning_replay: Default::default(),
        prompt_cache: crate::CompatiblePromptCache::Off,
        model: model("xai", "grok-4"),
        messages: vec![user("Hello").into()],
        max_tokens: None,
        tools: Vec::new(),
        tool_choice: None,
        routing_session: None,
    })
}

pub(crate) fn responses_request() -> crate::request::ProviderRequest {
    crate::request::ProviderRequest::OpenAiResponses(crate::request::OpenAiResponsesRequest {
        model: model("openai", "gpt-5"),
        input: vec![user("Hello").into()],
        max_output_tokens: None,
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
    })
}

pub(crate) async fn drain(open: ProviderOpen) -> Vec<AttemptFragment> {
    match open {
        ProviderOpen::Streaming(mut stream) => {
            let mut fragments = Vec::new();
            while let Some(fragment) = stream.next_fragment().await {
                fragments.push(fragment);
            }
            fragments
        }
        ProviderOpen::Failed(fragment) | ProviderOpen::FailedWithDetail { fragment, .. } => {
            vec![*fragment]
        }
    }
}

/// SSE frames, joined with the standard blank-line delimiter.
pub(crate) fn sse(events: &[&str]) -> Vec<u8> {
    let mut out = String::new();
    for event in events {
        out.push_str("data: ");
        out.push_str(event);
        out.push_str("\n\n");
    }
    out.into_bytes()
}

fn text_of(fragments: &[AttemptFragment]) -> String {
    fragments
        .iter()
        .filter_map(|fragment| match fragment {
            AttemptFragment::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn finish_of(fragments: &[AttemptFragment]) -> Option<FinishReason> {
    fragments.iter().find_map(|fragment| match fragment {
        AttemptFragment::ProviderTerminal { finish, .. } => Some(*finish),
        _ => None,
    })
}

mod anthropic_search;
mod azure;
mod request_growth;

#[cfg(test)]
mod tests;

mod pooling;
mod pooling_tls;

mod bedrock;
mod messages_auth;
