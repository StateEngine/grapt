//! The OpenAI Responses wire shape.
//!
//! Serves two families: the public OpenAI Responses API
//! ([`ProtocolFamily::OpenAiResponses`]) and the ChatGPT/Codex backend
//! ([`ProtocolFamily::ChatGptCodex`]). They are one protocol — the same event
//! names, the same `incomplete_details.reason` finish mapping, the same usage
//! field names. Where they differ is settled before a byte is decoded: a
//! different base URL, different headers, and a request type that on the Codex
//! route cannot name `max_output_tokens` at all.
//!
//! # Provenance
//!
//! Every event name and JSON key below is read from the frozen oracle
//! oracle at `42e4888cc0`, not from a live request:
//! The oracle's `open-responses.ts` (the SSE event vocabulary, the
//! `incomplete_details.reason` mapping at 614-624, the usage field names, and
//! the `response.function_call_arguments.*` tool events).
//!
//! **No live request has been made and no credential store has been read.**
//! The fixtures driving this decoder are loopback bytes this crate authored;
//! they are never described as a capture. Validating the mapping against a real
//! credential-backed transcript is the separately human-gated smoke command in
//! `tests/smoke`.
//!
//! # Tool calls are decoded here, and that closes a disclosed gap
//!
//! The Codex adapter previously refused tool-shaped events outright:
//!
//! ```text
//! "this adapter does not yet decode Codex tool calls; refusing rather than
//!  dropping one"
//! ```
//!
//! That refusal was the right call while decoding was absent — a silently
//! dropped tool call produces a Step that looks complete while the model was
//! asking for a tool. Both production routes decode tool calls through this
//! shared decoder.

use grapt_types::ToolCallId;

use crate::fragment::{FinishReason, ProviderFailureClass, ToolExecution};
use crate::replay::{compaction_state, reasoning_state, ReplayBinding};
use crate::transport::sink::DecodeSink;
use crate::usage::{normalize_responses_usage, UsageRoute};

/// Decodes the Responses event vocabulary. Tool input accumulation lives in
/// the sink; this decoder retains only whether the response emitted a complete
/// local tool call, because a clean `response.completed` does not carry a
/// separate finish-reason field from which `AssertedContinue` can be recovered.
#[derive(Debug)]
pub(crate) struct ResponsesDecoder {
    emitted_tool_call: bool,
    usage_route: UsageRoute,
    /// The attempt's replay binding; a completed reasoning item with encrypted
    /// content is retained for replay only when one exists.
    binding: Option<ReplayBinding>,
    /// The binding a provider compaction item is retained under.
    compaction: Option<ReplayBinding>,
    search: Option<ReplayBinding>,
    /// The request ends in a compaction_trigger. Every output item other than
    /// its compaction then reaches the compaction call, even one this decoder
    /// would drop, so the call can refuse a response that is not exactly one
    /// item.
    triggered: bool,
    citations: crate::citations::CitationTracker,
}

impl Default for ResponsesDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsesDecoder {
    pub(crate) fn new() -> Self {
        Self {
            emitted_tool_call: false,
            usage_route: UsageRoute::OpenAiResponses,
            binding: None,
            compaction: None,
            search: None,
            triggered: false,
            citations: crate::citations::CitationTracker::default(),
        }
    }

    pub(crate) fn for_route(usage_route: UsageRoute) -> Self {
        Self {
            emitted_tool_call: false,
            usage_route,
            binding: None,
            compaction: None,
            search: None,
            triggered: false,
            citations: crate::citations::CitationTracker::default(),
        }
    }

    pub(crate) fn searching(mut self, binding: Option<ReplayBinding>) -> Self {
        self.search = binding;
        self
    }

    pub(crate) fn triggered(mut self, triggered: bool) -> Self {
        self.triggered = triggered;
        self
    }

    /// An output item this decoder drops. In a compaction call it is still
    /// emitted, as an empty reasoning block with no state: never replayed, and
    /// counted by the compaction call as an item beside its compaction.
    fn dropped(&self, sink: &mut DecodeSink<'_>) {
        if self.triggered {
            sink.reasoning_delta("");
            sink.close_reasoning();
        }
    }

    pub(crate) fn compacting(mut self, binding: Option<ReplayBinding>) -> Self {
        self.compaction = binding;
        self
    }

    pub(crate) fn replaying(mut self, binding: Option<ReplayBinding>) -> Self {
        self.binding = binding;
        self
    }

    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
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

        if self.citations.observe(kind, &value).is_err() {
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "provider citation is malformed or exceeds its bound",
            );
            return;
        }
        if matches!(
            kind,
            "response.output_text.done"
                | "response.content_part.done"
                | "response.output_item.done"
                | "response.completed"
                | "response.incomplete"
                | "response.failed"
        ) {
            if let Some(links) = self.citations.take_pending() {
                sink.text_delta(&links);
            }
        }
        match kind {
            "response.output_text.delta" => {
                sink.text_delta(delta_text(&value));
            }
            "response.output_text.done" => sink.close_text(),
            "response.output_text.annotation.added" | "response.content_part.done" => {}

            "response.reasoning_text.delta"
            | "response.reasoning.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary.delta" => {
                sink.reasoning_delta(delta_text(&value));
            }
            "response.reasoning_text.done"
            | "response.reasoning.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_summary.done" => sink.close_reasoning(),

            // ---- Tool calls -------------------------------------------------
            //
            // The Responses API announces a function call as an output item,
            // streams its arguments as deltas keyed by `item_id`, and then
            // marks them done. `item_id` is the attempt-local accumulation key;
            // `call_id` is the durable handle.
            "response.output_item.added" => {
                let Some(item) = value.get("item") else {
                    sink.unmodeled();
                    return;
                };
                if !is_tool_call(item) {
                    sink.unmodeled();
                    return;
                }
                let (Some(key), Some(call), Some(name)) = (
                    item.get("id").and_then(|id| id.as_str()),
                    call_id(item),
                    item.get("name").and_then(|name| name.as_str()),
                ) else {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "the provider announced a tool call without an id, call id or name",
                    );
                    return;
                };
                if item["type"] == "custom_tool_call" {
                    sink.text_tool_input_opened(key, call, name);
                } else {
                    sink.tool_input_opened(key, call, name, ToolExecution::Local);
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let Some(key) = value.get("item_id").and_then(|id| id.as_str()) else {
                    sink.unmodeled();
                    return;
                };
                sink.tool_input_delta(key, delta_text(&value));
            }
            "response.function_call_arguments.done" | "response.custom_tool_call_input.done" => {
                let Some(key) = value.get("item_id").and_then(|id| id.as_str()) else {
                    sink.unmodeled();
                    return;
                };
                // The event carries the complete arguments: the input itself
                // when the provider streamed no deltas.
                let field = if kind == "response.custom_tool_call_input.done" {
                    "input"
                } else {
                    "arguments"
                };
                if value.get(field).is_some_and(|input| !input.is_string()) {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "the provider's final tool input must be a string",
                    );
                    return;
                }
                let arguments = value.get(field).and_then(|text| text.as_str());
                if !sink.tool_input_final(key, arguments) {
                    return;
                }
                if sink.tool_input_closed(key) {
                    self.emitted_tool_call = true;
                }
            }
            "response.output_item.done" if value["item"]["type"] == "web_search_call" => {
                let Some(binding) = &self.search else {
                    sink.fail(
                        ProviderFailureClass::InvalidProviderOutput,
                        "the provider returned an unadvertised native web search",
                    );
                    return;
                };
                let item =
                    match crate::native_search::SearchHistoryItem::from_output(&value["item"]) {
                        Ok(item) => item,
                        Err(_) => {
                            sink.fail(
                                ProviderFailureClass::InvalidProviderOutput,
                                "native web search history is malformed or exceeds its bound",
                            );
                            return;
                        }
                    };
                let state = match crate::replay::search_state(&item, binding) {
                    Ok(state) => state,
                    Err(reason) => {
                        sink.fail(ProviderFailureClass::InvalidProviderOutput, reason);
                        return;
                    }
                };
                let key = item.call().as_str();
                sink.tool_input_opened(
                    key,
                    item.call().clone(),
                    "web_search",
                    ToolExecution::ProviderHosted,
                );
                let arguments = item
                    .item()
                    .get("action")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}))
                    .to_string();
                if !sink.tool_input_final(key, Some(&arguments)) || !sink.tool_input_closed(key) {
                    return;
                }
                let text = item.fallback_text();
                sink.hosted_tool_terminal(
                    item.call().clone(),
                    "web_search",
                    if item.failed() { Err(text) } else { Ok(text) },
                    state,
                );
            }
            // A completed reasoning item is retained, under this attempt's
            // binding, so the next request can carry it back. One that cannot
            // be retained is simply not replayed.
            "response.output_item.done" if value["item"]["type"].as_str() == Some("reasoning") => {
                if let Some(state) = self
                    .binding
                    .as_ref()
                    .and_then(|binding| reasoning_state(&value["item"], binding))
                {
                    sink.reasoning_delta("");
                    sink.close_reasoning_with_state(state);
                } else {
                    self.dropped(sink);
                }
            }
            // A provider compaction item, asked for by a compaction_trigger, is
            // retained whole under the compaction binding; a compaction call
            // decides whether it is usable.
            "response.output_item.done" if value["item"]["type"].as_str() == Some("compaction") => {
                if let Some(binding) = &self.compaction {
                    sink.reasoning_delta("");
                    sink.close_reasoning_with_state(compaction_state(&value["item"], binding));
                }
            }
            // The item's own completion. The arguments event already closed the
            // accumulation, so this is ordinarily a no-op; it is handled rather
            // than counted as unmodeled so a provider that omits the arguments
            // `done` event still settles its call.
            "response.output_item.done" => {
                if !is_tool_call(&value["item"]) {
                    self.dropped(sink);
                }
                let Some(key) = value
                    .get("item")
                    .and_then(|item| item.get("id"))
                    .and_then(|id| id.as_str())
                else {
                    sink.unmodeled();
                    return;
                };
                // A completed function call item carries its arguments too.
                if is_tool_call(&value["item"]) {
                    let field = if value["item"]["type"] == "custom_tool_call" {
                        "input"
                    } else {
                        "arguments"
                    };
                    if value["item"]
                        .get(field)
                        .is_some_and(|input| !input.is_string())
                    {
                        sink.fail(
                            ProviderFailureClass::InvalidProviderOutput,
                            "the provider's final tool input must be a string",
                        );
                        return;
                    }
                    let arguments = value["item"].get(field).and_then(|text| text.as_str());
                    if !sink.tool_input_final(key, arguments) {
                        return;
                    }
                }
                if sink.tool_input_closed(key) {
                    self.emitted_tool_call = true;
                }
            }

            "response.completed" | "response.incomplete" => {
                let finish = match finish_reason_for(kind, &value) {
                    FinishReason::Stop if self.emitted_tool_call => FinishReason::ToolCalls,
                    finish => finish,
                };
                let response = &value["response"];
                let incomplete = kind == "response.incomplete";
                let usage = match normalize_responses_usage(response, self.usage_route) {
                    Ok(usage) => usage,
                    Err(error) => {
                        sink.invalid_usage(error);
                        None
                    }
                };
                if incomplete && finish == FinishReason::Unknown {
                    if let Some(usage) = usage {
                        sink.remember_usage(usage);
                    }
                    super::responses_terminal::fail(kind, response, sink);
                } else {
                    sink.terminal(finish, usage);
                }
            }
            "response.failed" => {
                // Failed responses can legitimately have no accounting. A malformed
                // snapshot must never replace the backend's primary failure.
                match normalize_responses_usage(&value["response"], self.usage_route) {
                    Ok(Some(usage)) => sink.remember_usage(usage),
                    Ok(None) => (),
                    Err(error) => sink.invalid_usage(error),
                }
                super::responses_terminal::fail(kind, &value["response"], sink);
            }
            _ => sink.unmodeled(),
        }
    }
}

/// A delta's text, defaulting to empty.
///
/// An absent `delta` on a delta event is a provider defect, but an empty string
/// is the honest normalization of it: the event happened, and it carried no
/// text. Failing the attempt over it would be harsher than the contract, which
/// reserves failure for output it cannot represent.
fn delta_text(value: &serde_json::Value) -> &str {
    value
        .get("delta")
        .and_then(|delta| delta.as_str())
        .unwrap_or("")
}

fn is_tool_call(item: &serde_json::Value) -> bool {
    item.get("type")
        .and_then(|kind| kind.as_str())
        .map(|kind| matches!(kind, "function_call" | "custom_tool_call"))
        .unwrap_or(false)
}

fn call_id(item: &serde_json::Value) -> Option<ToolCallId> {
    item.get("call_id")
        .and_then(|id| id.as_str())
        .and_then(ToolCallId::new)
}

/// Map a terminal event's `incomplete_details.reason` to a finish reason.
///
/// Transcribed from the oracle's `mapFinishReason`
/// (`open-responses.ts:614-624`): `max_output_tokens` is `length`,
/// `content_filter` is `content-filter`, a `response.incomplete` with no reason
/// is `unknown`, and a clean `response.completed` is `stop`.
pub(crate) fn finish_reason_for(kind: &str, value: &serde_json::Value) -> FinishReason {
    let reason = value
        .get("response")
        .and_then(|response| response.get("incomplete_details"))
        .and_then(|details| details.get("reason"))
        .and_then(|reason| reason.as_str());
    match reason {
        Some("max_output_tokens") => FinishReason::Length,
        Some("content_filter") => FinishReason::ContentFilter,
        Some(_) => FinishReason::Unknown,
        None => {
            if kind == "response.incomplete" {
                FinishReason::Unknown
            } else {
                FinishReason::Stop
            }
        }
    }
}

/// Read the Responses usage block.
///
/// Absent is not zero (`UsageField` law): every field stays `None` unless the
/// provider stated it, and `non_cached_input` is derived only when every input
/// it subtracts is actually present.
#[cfg(test)]
pub(crate) fn usage_from(
    value: &serde_json::Value,
) -> Result<Option<crate::usage::TokenUsage>, crate::usage::UsageNormalizationError> {
    normalize_responses_usage(value, UsageRoute::ChatGptCodex)
}
