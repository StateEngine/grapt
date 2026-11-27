//! The Anthropic Messages wire shape.
//!
//! # Provenance
//!
//! Every event name and JSON key below is read from the frozen oracle
//! oracle at `42e4888cc0`, not from a live request:
//! The oracle's `anthropic-messages.ts` (the `message_start` /
//! `content_block_start` / `content_block_delta` / `content_block_stop` /
//! `message_delta` / `message_stop` event vocabulary, the `stop_reason`
//! mapping, and the `usage` field names), and
//! The oracle's `anthropic.ts` (the `x-api-key` and
//! `anthropic-version` headers).
//!
//! **No live Anthropic request has been made and no credential store has been
//! read.** The fixtures driving this decoder are loopback bytes this crate
//! authored; they are never described as a capture. Validating the mapping
//! against a real credential-backed transcript is the separately human-gated
//! smoke command in `tests/smoke`.
//!
//! # Why this route is the one that needed the shared block allocator
//!
//! Anthropic addresses content by **index**: `content_block_start` carries an
//! index, every delta names it, and `content_block_stop` closes it. Text,
//! extended thinking and `tool_use` blocks interleave freely under those
//! indices. That is exactly the shape corrupted by shared block ordinals —
//! one shared `next_ordinal` derived as `next_ordinal - 1` for two kinds — and
//! here the interleaving is not an edge case, it is the normal transcript of
//! any request with thinking enabled.
//!
//! The provider's index is therefore never used as an ordinal. It is a lookup
//! key into what this decoder opened at that index, and the ordinal comes from
//! the shared allocator, which gives a block its own ordinal for its whole life.
//!
//! # `stop_reason` arrives separately from the stream's end
//!
//! `message_delta` carries `stop_reason` and the output usage; `message_stop`
//! ends the stream. A decoder that terminated on `message_delta` would truncate
//! the stream a frame early, and one that ignored `message_delta` would lose
//! both the finish reason and the output-token count. This decoder retains them
//! from `message_delta` and emits the terminal at `message_stop`.

use std::collections::BTreeMap;

use grapt_types::ToolCallId;

use crate::fragment::{FinishReason, ProviderFailureClass, TokenUsage, ToolExecution};
use crate::transport::sink::DecodeSink;
use crate::usage::{normalize_anthropic_usage, UsageNormalizationError, UsageRoute};

/// What this decoder opened at one provider content-block index.
enum OpenBlock {
    Redacted {
        data: String,
    },
    Text {
        native: Option<Box<super::anthropic_search::SearchText>>,
    },
    Thinking {
        text: String,
        signature: String,
    },
    /// A `tool_use` block. Its accumulation key in the sink is the index,
    /// rendered; the durable call id came from the block's `id`.
    ToolUse,
    ServerUse,
    SearchResult(serde_json::Value),
}

impl std::fmt::Debug for OpenBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Redacted { .. } => "Redacted([REDACTED])",
            Self::Text { .. } => "Text",
            Self::Thinking { .. } => "Thinking([REDACTED])",
            Self::ToolUse => "ToolUse",
            Self::ServerUse => "ServerUse",
            Self::SearchResult(_) => "SearchResult([REDACTED])",
        })
    }
}

/// The most concurrently open content blocks one attempt admits.
///
/// Anthropic indices are provider-chosen, so the map they key must be bounded
/// like every other provider-controlled structure in this crate. The number
/// matches the sink's tool-input bound for the same reason: a block that could
/// never become an executable call or a rendered transcript is not worth
/// retaining.
const MAX_OPEN_BLOCKS: usize = 16;

/// Decodes the Anthropic Messages event vocabulary.
#[derive(Debug)]
pub(crate) struct AnthropicDecoder {
    binding: crate::replay::ReplayBinding,
    instruction_mode: crate::AnthropicInstructionMode,
    /// Provider index -> what this decoder opened there. Bounded above.
    open: BTreeMap<u64, OpenBlock>,
    /// Retained from `message_delta` until `message_stop`. See the module note.
    pending_finish: Option<FinishReason>,
    /// Latest known leaves across every cumulative snapshot, including start.
    observed_usage: Option<TokenUsage>,
    search: super::anthropic_search::SearchTurn,
}

impl AnthropicDecoder {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_binding(crate::replay::ReplayBinding::fixture())
    }

    pub(crate) fn with_binding(binding: crate::replay::ReplayBinding) -> Self {
        Self {
            binding,
            instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
            open: BTreeMap::new(),
            pending_finish: None,
            observed_usage: None,
            search: super::anthropic_search::SearchTurn::default(),
        }
    }

    pub(crate) fn instruction_mode(mut self, mode: crate::AnthropicInstructionMode) -> Self {
        self.instruction_mode = mode;
        self
    }

    pub(crate) fn searching(mut self, max_uses: Option<u8>) -> Self {
        self.search.configure(max_uses);
        self
    }

    pub(crate) fn decode(&mut self, payload: &str, sink: &mut DecodeSink<'_>) {
        let value: serde_json::Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            Err(_) => {
                sink.fail(
                    ProviderFailureClass::InvalidProviderOutput,
                    "the provider sent an SSE frame that is not JSON",
                );
                return;
            }
        };
        let Some(kind) = value.get("type").and_then(|kind| kind.as_str()) else {
            sink.unmodeled();
            return;
        };

        match kind {
            "message_start" => {
                let usage = match value.get("message") {
                    Some(message) => usage_from(message),
                    None => Ok(None),
                };
                self.observe_usage(usage, sink);
            }

            "content_block_start" => self.block_start(&value, sink),
            "content_block_delta" => self.block_delta(&value, sink),
            "content_block_stop" => self.block_stop(&value, sink),

            "message_delta" => {
                if let Some(reason) = value
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(|reason| reason.as_str())
                {
                    self.pending_finish = Some(finish_reason_for(reason));
                }
                self.observe_usage(usage_from(&value), sink);
            }

            "message_stop" => {
                if !self.open.is_empty() || self.search.pending() {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "the provider ended with unclosed content blocks",
                    );
                    return;
                }
                let finish = self.pending_finish.take().unwrap_or(FinishReason::Unknown);
                sink.terminal(finish, self.observed_usage.take());
            }

            // A mid-stream error frame. Anthropic's `overloaded_error` is the
            // documented backpressure signal and is retryable; the others are
            // classified by their own type rather than folded into one class.
            "error" => {
                let kind = value
                    .get("error")
                    .and_then(|error| error.get("type"))
                    .and_then(|kind| kind.as_str())
                    .unwrap_or("");
                let (class, message) = error_class(kind);
                let classification = value
                    .get("error")
                    .and_then(crate::transport::classify::anthropic_context_classification);
                sink.fail_classified(class, classification, message);
            }

            // `ping` and anything else this decoder does not model.
            _ => sink.unmodeled(),
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn block_start(&mut self, value: &serde_json::Value, sink: &mut DecodeSink<'_>) {
        let Some(index) = block_index(value) else {
            sink.unmodeled();
            return;
        };
        let Some(block) = value.get("content_block") else {
            sink.unmodeled();
            return;
        };
        let kind = block
            .get("type")
            .and_then(|kind| kind.as_str())
            .unwrap_or("");

        if self.open.contains_key(&index) {
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "the provider reopened an active content block index",
            );
            return;
        }
        if self.open.len() >= MAX_OPEN_BLOCKS {
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "the provider opened more concurrent content blocks than this attempt admits",
            );
            return;
        }
        if !self.search.permits_block(kind) {
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "Anthropic search result must immediately follow its server call",
            );
            return;
        }

        match kind {
            "text" => {
                if self
                    .open
                    .values()
                    .any(|block| matches!(block, OpenBlock::Text { .. }))
                {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "overlapping text blocks cannot share a normalized block",
                    );
                    return;
                }
                let native = if self.search.enabled() {
                    match super::anthropic_search::SearchText::new(block, sink) {
                        Ok(text) => Some(Box::new(text)),
                        Err(reason) => {
                            sink.fail(ProviderFailureClass::InvalidProviderOutput, reason);
                            return;
                        }
                    }
                } else {
                    None
                };
                self.open.insert(index, OpenBlock::Text { native });
            }
            "server_tool_use" => {
                if !self.open.is_empty() {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "Anthropic server search cannot overlap another content block",
                    );
                    return;
                }
                match self.search.open_use(block, &key(index), sink) {
                    Ok(()) => {
                        self.open.insert(index, OpenBlock::ServerUse);
                    }
                    Err(reason) => sink.fail(ProviderFailureClass::InvalidProviderOutput, reason),
                }
            }
            "web_search_tool_result" => {
                if !self.open.is_empty() {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "Anthropic search result cannot overlap another content block",
                    );
                    return;
                }
                match self.search.result(block) {
                    Ok(()) => {
                        self.open
                            .insert(index, OpenBlock::SearchResult(block.clone()));
                    }
                    Err(reason) => sink.fail(ProviderFailureClass::InvalidProviderOutput, reason),
                }
            }
            "thinking" | "redacted_thinking" => {
                if self.open.values().any(|block| {
                    matches!(
                        block,
                        OpenBlock::Thinking { .. }
                            | OpenBlock::Redacted { .. }
                            | OpenBlock::ToolUse
                    )
                }) {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "thinking cannot overlap another thinking or tool block",
                    );
                    return;
                }
                if kind == "redacted_thinking" {
                    let Some(data) =
                        block
                            .get("data")
                            .and_then(|value| value.as_str())
                            .filter(|data| {
                                !data.is_empty() && data.len() <= crate::AdapterState::MAX_BYTES
                            })
                    else {
                        sink.fail(
                            ProviderFailureClass::InvalidProviderOutput,
                            "redacted thinking payload is missing or oversized",
                        );
                        return;
                    };
                    self.open
                        .insert(index, OpenBlock::Redacted { data: data.into() });
                    return;
                }
                let text = text_of(block, "thinking");
                let signature = text_of(block, "signature");
                if text.len().saturating_add(signature.len()) > crate::AdapterState::MAX_BYTES {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "thinking exceeds its replay bound",
                    );
                    return;
                }
                self.open.insert(
                    index,
                    OpenBlock::Thinking {
                        text: text.into(),
                        signature: signature.into(),
                    },
                );
                if !text.is_empty() {
                    sink.reasoning_delta(text);
                }
            }
            "tool_use" => {
                if self.open.values().any(|block| {
                    matches!(
                        block,
                        OpenBlock::Thinking { .. } | OpenBlock::Redacted { .. }
                    )
                }) {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "tool input began before signed thinking closed",
                    );
                    return;
                }
                let (Some(id), Some(name)) = (
                    block
                        .get("id")
                        .and_then(|id| id.as_str())
                        .and_then(ToolCallId::new),
                    block.get("name").and_then(|name| name.as_str()),
                ) else {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "the provider opened a tool_use block without an id or name",
                    );
                    return;
                };
                self.open.insert(index, OpenBlock::ToolUse);
                sink.tool_input_opened(&key(index), id, name, ToolExecution::Local);
            }
            _ => sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "the provider returned content this history projection cannot replay",
            ),
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn block_delta(&mut self, value: &serde_json::Value, sink: &mut DecodeSink<'_>) {
        let Some(index) = block_index(value) else {
            sink.unmodeled();
            return;
        };
        let Some(open) = self.open.get_mut(&index) else {
            // A delta for a block this decoder never opened. The contract makes
            // "a delta/end requires the matching opened block" a validator rule;
            // rather than emit a fragment the validator would refuse, the
            // adapter refuses here, where the provider's own frame is still in
            // hand to explain it.
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "the provider sent a content-block delta for a block it never opened",
            );
            return;
        };
        let Some(delta) = value.get("delta") else {
            sink.unmodeled();
            return;
        };

        match open {
            OpenBlock::Redacted { .. } => sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "redacted thinking must arrive as one complete block",
            ),
            OpenBlock::Text { native } => {
                if let Some(native) = native {
                    if let Err(reason) = native.delta(delta, sink) {
                        sink.fail(ProviderFailureClass::InvalidProviderOutput, reason);
                    }
                } else {
                    sink.text_delta(text_of(delta, "text"));
                }
            }
            OpenBlock::ServerUse => {
                if delta["type"] != "input_json_delta" {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "unsupported Anthropic server input delta",
                    );
                    return;
                }
                let Some(text) = delta["partial_json"].as_str() else {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "Anthropic server input delta has no JSON",
                    );
                    return;
                };
                match self.search.input_delta(text) {
                    Ok(()) => sink.tool_input_delta(&key(index), text),
                    Err(reason) => sink.fail(ProviderFailureClass::InvalidProviderOutput, reason),
                }
            }
            OpenBlock::SearchResult(_) => sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "Anthropic search result must arrive as a complete block",
            ),
            OpenBlock::Thinking { text, signature } => {
                // `thinking_delta` carries `thinking`; a `signature_delta`
                // carries an opaque signature that is NOT transcript text and
                // must not be rendered as reasoning.
                if delta
                    .get("type")
                    .and_then(|kind| kind.as_str())
                    .map(|kind| kind == "signature_delta")
                    .unwrap_or(false)
                {
                    let delta = text_of(delta, "signature");
                    if text
                        .len()
                        .saturating_add(signature.len())
                        .saturating_add(delta.len())
                        > crate::AdapterState::MAX_BYTES
                    {
                        sink.fail(
                            ProviderFailureClass::InvalidProviderOutput,
                            "thinking exceeds its replay bound",
                        );
                        return;
                    }
                    signature.push_str(delta);
                    return;
                }
                let delta = text_of(delta, "thinking");
                if !signature.is_empty() {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "thinking text followed its signature",
                    );
                    return;
                }
                if text
                    .len()
                    .saturating_add(signature.len())
                    .saturating_add(delta.len())
                    > crate::AdapterState::MAX_BYTES
                {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "thinking exceeds its replay bound",
                    );
                    return;
                }
                text.push_str(delta);
                sink.reasoning_delta(delta);
            }
            OpenBlock::ToolUse => {
                sink.tool_input_delta(&key(index), text_of(delta, "partial_json"));
            }
        }
    }

    fn block_stop(&mut self, value: &serde_json::Value, sink: &mut DecodeSink<'_>) {
        let Some(index) = block_index(value) else {
            sink.unmodeled();
            return;
        };
        match self.open.remove(&index) {
            Some(OpenBlock::Redacted { data }) => {
                match crate::replay::redacted_state(data, &self.binding, self.instruction_mode) {
                    Ok(state) => {
                        sink.reasoning_delta("");
                        sink.close_reasoning_with_state(state);
                    }
                    Err(reason) => sink.fail(ProviderFailureClass::InvalidProviderOutput, reason),
                }
            }
            Some(OpenBlock::Text { native }) => {
                match native.map(|text| text.finish(&self.binding)).transpose() {
                    Ok(state) => {
                        sink.close_text_with_state(state.unwrap_or_else(crate::AdapterState::none))
                    }
                    Err(reason) => sink.fail(ProviderFailureClass::InvalidProviderOutput, reason),
                }
            }
            Some(OpenBlock::ServerUse) => {
                if let Err(reason) = self.search.close_use(&key(index), sink) {
                    sink.fail(ProviderFailureClass::InvalidProviderOutput, reason);
                }
            }
            Some(OpenBlock::SearchResult(result)) => {
                if let Err(reason) = self.search.close_result(result, &self.binding, sink) {
                    sink.fail(ProviderFailureClass::InvalidProviderOutput, reason);
                }
            }
            Some(OpenBlock::Thinking { text, signature }) => {
                if signature.is_empty() {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "thinking block closed without its replay signature",
                    );
                } else {
                    if text.is_empty() {
                        sink.reasoning_delta("");
                    }
                    match crate::replay::thinking_state(
                        text,
                        signature,
                        &self.binding,
                        self.instruction_mode,
                    ) {
                        Ok(state) => sink.close_reasoning_with_state(state),
                        Err(reason) => {
                            sink.fail(ProviderFailureClass::InvalidProviderOutput, reason)
                        }
                    }
                }
            }
            Some(OpenBlock::ToolUse) => {
                sink.tool_input_closed(&key(index));
            }
            None => sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "the provider stopped a content block it never opened",
            ),
        }
    }

    /// Reconcile each snapshot immediately, preserving omitted leaves.
    fn observe_usage(
        &mut self,
        snapshot: Result<Option<TokenUsage>, UsageNormalizationError>,
        sink: &mut DecodeSink<'_>,
    ) {
        let merged = snapshot.and_then(|usage| match (&self.observed_usage, usage) {
            (_, None) => Ok(self.observed_usage.clone()),
            (None, Some(usage)) => Ok(Some(usage)),
            (Some(previous), Some(usage)) => previous
                .merge(&usage, UsageRoute::AnthropicMessages)
                .map(Some),
        });
        match merged {
            Ok(usage) => {
                if let Some(usage) = &usage {
                    sink.remember_usage(usage.clone());
                    // A family conflict reduces the merge to unstamped: the
                    // leaves stay valid, and the conflict is recorded on the
                    // non-fatal warning channel instead of disappearing.
                    if usage.protocol_conflict() {
                        sink.usage_protocol_conflict();
                    }
                }
                self.observed_usage = usage;
            }
            Err(error) => {
                self.observed_usage = None;
                sink.invalid_usage(error);
            }
        }
    }
}

/// The accumulation key for one provider block index.
///
/// Prefixed so it can never collide with a Responses `item_id` or a
/// chat-completions index if the sink is ever shared across a multiplexed
/// stream. Attempt-local either way — the durable handle is the call id.
fn key(index: u64) -> String {
    format!("anthropic:{index}")
}

fn block_index(value: &serde_json::Value) -> Option<u64> {
    value.get("index").and_then(|index| index.as_u64())
}

fn text_of<'a>(delta: &'a serde_json::Value, field: &str) -> &'a str {
    delta
        .get(field)
        .and_then(|text| text.as_str())
        .unwrap_or("")
}

/// Map Anthropic's `stop_reason` to the closed finish vocabulary.
///
/// `max_tokens` is `Length` — the contract requires "`length`/equivalent maps
/// to `Truncated`, never ordinary idle", and `FinishReason::Length` is what
/// normalizes to that.
fn finish_reason_for(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    }
}

/// Classify an Anthropic mid-stream error frame.
///
/// `overloaded_error` is the documented backpressure signal, so it carries the
/// retryable provider-internal class; the runtime's pre-output rule then decides
/// whether this attempt may actually retry. `api_error` is equally
/// provider-side. Known credential, request and policy failures stay terminal;
/// unrecognized error types use the same bounded provider-internal retry path.
fn error_class(kind: &str) -> (ProviderFailureClass, &'static str) {
    match kind {
        "overloaded_error" => (
            ProviderFailureClass::ProviderInternal { retry_after: None },
            "the provider reported it is overloaded",
        ),
        "api_error" => (
            ProviderFailureClass::ProviderInternal { retry_after: None },
            "the provider reported an internal api_error",
        ),
        "rate_limit_error" => (
            ProviderFailureClass::RateLimit { retry_after: None },
            "the provider reported a rate limit mid-stream",
        ),
        "authentication_error" | "permission_error" => (
            ProviderFailureClass::Authentication,
            "the provider refused the credential mid-stream",
        ),
        "invalid_request_error" => (
            ProviderFailureClass::InvalidRequest,
            "the provider reported the request was invalid",
        ),
        "content_filter" => (
            ProviderFailureClass::ContentPolicy,
            "the provider refused content mid-stream",
        ),
        "not_found_error" => (
            ProviderFailureClass::NoRoute,
            "the provider reported the requested resource does not exist",
        ),
        // Unknown nonempty types are provider failures; missing or malformed
        // types stay invalid output. Never echo the provider-controlled type.
        _ => (
            if kind.is_empty() {
                ProviderFailureClass::InvalidProviderOutput
            } else {
                ProviderFailureClass::ProviderInternal { retry_after: None }
            },
            "the provider reported an error type this adapter does not model",
        ),
    }
}

/// Read an Anthropic usage block.
///
/// Absent is not zero. Anthropic reports cache reads and writes as separate
/// counters, and `input_tokens` on this API excludes them — so
/// `non_cached_input` is `input_tokens` itself rather than a subtraction, which
/// is the opposite of the Responses route's arithmetic and the reason the two
/// readers are not shared.
fn usage_from(value: &serde_json::Value) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    normalize_anthropic_usage(value)
}
