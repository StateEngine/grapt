//! The OpenAI-compatible chat-completions wire shape.
//!
//! The family with the widest membership and the least authority: Grok
//! (`api.x.ai`), gateways, and self-hosted servers all speak it. Nothing in
//! this decoder is Grok-specific, which
//! is the point — a decoder that special-cased one vendor would be a fourth
//! route wearing this one's name.
//!
//! # Provenance
//!
//! The event shape below is the OpenAI chat-completions streaming format as
//! carried in the frozen oracle at `42e4888cc0`
//! (the oracle's `openai-chat.ts`: the `choices[].delta`
//! structure, the `finish_reason` vocabulary, the `tool_calls[].index`
//! accumulation, and the optional trailing `usage` chunk).
//!
//! **No live request to any compatible endpoint has been made and no credential
//! store has been read.** The fixtures driving this decoder are loopback bytes
//! this crate authored. Validating the mapping against a real credential-backed
//! transcript is the separately human-gated smoke command in `tests/smoke`.
//!
//! # Two things this protocol does that the others do not
//!
//! **`finish_reason` arrives on the chunk, not as its own event.** A chunk
//! carrying both a content delta and a non-null `finish_reason` is ordinary,
//! so the delta must be applied BEFORE the terminal is latched or the last
//! token of every response is lost. The sink's terminal closes open blocks, so
//! ordering here is the whole correctness argument.
//!
//! **There is no explicit end-of-stream event, and usage arrives AFTER the
//! finish reason.** The stream ends with `data: [DONE]`, which the SSE decoder
//! drops as a non-payload. With `stream_options.include_usage`, the server
//! sends the `finish_reason` chunk FIRST and a usage-only chunk (empty
//! `choices`) after it.
//!
//! So this decoder cannot terminate when it sees `finish_reason`: the terminal
//! fragment carries the usage, the terminal latches, and a terminal emitted one
//! chunk early would permanently discard the only usage the protocol reports
//! for a streamed response. It retains the finish instead and emits the
//! terminal at end-of-stream, through [`CompatibleDecoder::finish`].
//!
//! A response whose `finish_reason` never arrives therefore reaches EOF with
//! nothing retained, and the shared stream turns that into the typed transport
//! failure "ended without a terminal event" — truthful, and better than
//! synthesizing a `Stop` this decoder was never told about.
//!
//! # Reasoning is a vendor extension here, and is read as one
//!
//! Chat-completions has no standard reasoning channel. Several compatible
//! servers add one under `delta.reasoning_content` (DeepSeek-style) or
//! `delta.reasoning` or `delta.reasoning_text`. These are read to retain
//! visible model output; none is required, because a server without them is
//! fully conformant.

use std::collections::BTreeSet;

use grapt_types::ToolCallId;

use crate::fragment::{FinishReason, ProviderFailureClass, TokenUsage, ToolExecution};
use crate::transport::sink::DecodeSink;
use crate::usage::{normalize_compatible_usage, UsageNormalizationError};

/// Decodes the chat-completions streaming vocabulary.
#[derive(Debug, Default)]
pub(crate) struct CompatibleDecoder {
    /// Retained from the `finish_reason` chunk until end-of-stream. See the
    /// module note: terminating early would discard the trailing usage.
    pending_finish: Option<FinishReason>,
    // Native IBM omits tool indices/IDs on some models and finishes before EOF.
    watsonx: Option<grapt_types::PhysicalAttemptId>,
    /// The last usage this stream reported, from whichever chunk carried it.
    pending_usage: Option<TokenUsage>,
    /// Tool-call indices this decoder opened and has not yet settled.
    ///
    /// This protocol has NO per-tool "arguments done" event — see
    /// [`CompatibleDecoder::settle_tool_calls`]. The indices are retained so a
    /// `finish_reason` can settle them as COMPLETE, which is the only signal
    /// the protocol gives that they are.
    open_tools: BTreeSet<u64>,
}

impl CompatibleDecoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn watsonx(attempt: &grapt_types::PhysicalAttemptId) -> Self {
        Self {
            watsonx: Some(attempt.clone()),
            ..Self::default()
        }
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

        // Retain reported usage before any error or output handling can end
        // this response. Missing fields remain absent.
        match usage_from(&value) {
            Ok(Some(usage)) => {
                sink.remember_usage(usage.clone());
                self.pending_usage = Some(usage);
            }
            Ok(None) => {}
            Err(error) => {
                sink.invalid_usage(error);
            }
        }

        // A vendor error object delivered in-band instead of by status code.
        // Preserve its typed failure instead of losing the error at stream EOF.
        if let Some(error) = value.get("error") {
            let classification = crate::transport::classify::compat_context_classification(error);
            let class = if classification.is_some() {
                ProviderFailureClass::InvalidRequest
            } else {
                error
                    .get("type")
                    .and_then(|kind| kind.as_str())
                    .map(compat_error_class)
                    .unwrap_or(ProviderFailureClass::InvalidProviderOutput)
            };
            // The vendor's message is NOT interpolated: it is provider-
            // controlled text on an error path, and the contract requires a
            // diagnostic to reveal "neither payload nor secret-dependent
            // length".
            sink.fail_classified(
                class,
                classification,
                "the provider returned an error object in the stream",
            );
            return;
        }

        // A trailing usage-only chunk (`stream_options.include_usage`) has an
        // empty `choices` array. It is read before the choice walk so it is not
        // mistaken for an unmodeled frame.
        let choices = value.get("choices").and_then(|choices| choices.as_array());
        let has_choices = choices.map(|choices| !choices.is_empty()).unwrap_or(false);
        if !has_choices {
            if value.get("usage").is_none() {
                sink.unmodeled();
            }
            return;
        }

        let Some(choices) = choices else {
            sink.unmodeled();
            return;
        };

        // Only choice 0 is normalized. `n > 1` is not requestable through this
        // crate's request types, so a second choice is a provider surprise
        // rather than something the runtime asked for, and inventing a
        // transcript position for it would be worse than counting it.
        for choice in choices.iter().skip(1) {
            let _ = choice;
            sink.unmodeled();
        }
        let Some(choice) = choices.first() else {
            sink.unmodeled();
            return;
        };

        if let Some(delta) = choice.get("delta") {
            self.apply_delta(delta, sink);
        }

        // AFTER the delta: a chunk may carry both, and latching the terminal
        // first would drop the last token of every response.
        if let Some(reason) = choice
            .get("finish_reason")
            .and_then(|reason| reason.as_str())
        {
            // The provider has said this response is complete, which is the
            // ONLY completion signal this protocol gives a tool call. Settle
            // them here, while the accumulations are still open.
            let native_tool_stop =
                self.watsonx.is_some() && reason == "stop" && !self.open_tools.is_empty();
            self.settle_tool_calls(sink);
            // RETAINED, not emitted. The terminal carries the usage, and the
            // usage-only chunk has not arrived yet.
            self.pending_finish = Some(if native_tool_stop {
                FinishReason::ToolCalls
            } else {
                finish_reason_for(reason)
            });
            // IBM reports usage with the finishing chunk and may keep SSE open.
            if self.watsonx.is_some() {
                self.finish(sink);
            }
        }
    }

    /// Close every open tool accumulation as COMPLETE.
    ///
    /// # The defect this closes
    ///
    /// Chat-completions has no per-tool "arguments done" event. The arguments
    /// stream as `tool_calls[].function.arguments` deltas and simply stop; the
    /// chunk carrying `finish_reason` is the provider stating the response —
    /// and therefore every tool call in it — is complete.
    ///
    /// The first implementation never settled them. The accumulations were
    /// still open when the terminal ran, so `DecodeSink::terminal` swept them
    /// through `close_dangling_tool_inputs`, and a **valid, complete** tool
    /// call was normalized as `ToolInputInvalid` + `ToolInputClosed` with **no
    /// `ToolCalled` at all**. A Step would then terminate looking successful
    /// while the tool the model asked for was never dispatched — which is
    /// precisely the failure the Codex route's old blanket refusal existed to
    /// prevent, reintroduced through a different door.
    ///
    /// Worse, my own witness asserted that sequence as CORRECT, with a comment
    /// calling it "truthful". A test can certify a defect; this one did.
    ///
    /// A stream that ends WITHOUT a `finish_reason` is genuinely truncated, and
    /// its accumulations must still be refused. That case is untouched: nothing
    /// calls this, the terminal never runs, and the shared stream reports the
    /// typed "ended without a terminal event" transport failure.
    fn settle_tool_calls(&mut self, sink: &mut DecodeSink<'_>) {
        for index in std::mem::take(&mut self.open_tools) {
            sink.tool_input_closed(&key(index));
        }
    }

    /// End of stream: emit the terminal this decoder has been holding.
    ///
    /// Called by the shared stream at EOF, before it decides the stream ended
    /// without a terminal. If nothing was retained, nothing is emitted and that
    /// truthful transport failure stands.
    pub(crate) fn finish(&mut self, sink: &mut DecodeSink<'_>) {
        if let Some(finish) = self.pending_finish.take() {
            sink.terminal(finish, self.pending_usage.take());
        }
    }

    fn apply_delta(&mut self, delta: &serde_json::Value, sink: &mut DecodeSink<'_>) {
        // A vendor reasoning channel, if this server has one. Applied before
        // content so an interleaved chunk keeps its natural order. Aliases
        // share one channel; the first non-empty string wins.
        if let Some(text) = ["reasoning_content", "reasoning", "reasoning_text"]
            .into_iter()
            .filter_map(|field| delta.get(field).and_then(|text| text.as_str()))
            .find(|text| !text.is_empty())
        {
            sink.reasoning_delta(text);
        }

        if let Some(text) = delta.get("content").and_then(|text| text.as_str()) {
            if !text.is_empty() {
                sink.text_delta(text);
            }
        }

        let Some(tool_calls) = delta.get("tool_calls").and_then(|calls| calls.as_array()) else {
            return;
        };
        if self.watsonx.is_some() && tool_calls.len() > 1 {
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "watsonx returned parallel tools despite serial tool admission",
            );
            return;
        }
        for call in tool_calls {
            self.apply_tool_call(call, sink);
        }
    }

    /// Apply one entry of a `tool_calls` delta array.
    ///
    /// The protocol's accumulation key is the entry's `index`, which is stable
    /// across chunks for one call. The `id` and `function.name` appear on the
    /// first chunk for that index and are absent afterwards, so the opening is
    /// keyed on their presence rather than on the chunk being the first.
    fn apply_tool_call(&mut self, call: &serde_json::Value, sink: &mut DecodeSink<'_>) {
        let index = match call.get("index") {
            Some(value) => value.as_u64(),
            None => self.watsonx.as_ref().map(|_| 0),
        };
        let Some(index) = index else {
            if self.watsonx.is_some() {
                sink.fail(
                    ProviderFailureClass::InvalidProviderOutput,
                    "watsonx returned an invalid tool index",
                );
            } else {
                sink.unmodeled();
            }
            return;
        };
        if self.watsonx.is_some() && index != 0 {
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "watsonx returned a nonserial tool index",
            );
            return;
        }
        let key = key(index);
        let function = call.get("function");

        let id = match call.get("id") {
            Some(value) => value.as_str().and_then(ToolCallId::new),
            None => self
                .watsonx
                .as_ref()
                .and_then(|attempt| ToolCallId::new(format!("{}-tool-0", attempt.as_str()))),
        };
        if self.watsonx.is_some() && call.get("id").is_some() && id.is_none() {
            sink.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "watsonx returned an invalid tool identity",
            );
            return;
        }
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(|name| name.as_str());

        if let (Some(id), Some(name)) = (id, name) {
            sink.tool_input_opened(&key, id, name, ToolExecution::Local);
            self.open_tools.insert(index);
        }

        if let Some(arguments) = function
            .and_then(|function| function.get("arguments"))
            .and_then(|arguments| arguments.as_str())
        {
            if !arguments.is_empty() {
                sink.tool_input_delta(&key, arguments);
            }
        }
    }
}

/// Map a chat-completions `finish_reason`.
///
/// `length` is `Length`, which normalizes to `Truncated` — never ordinary idle,
/// per the contract. A `stop` that follows a tool call stays `Stop`: some
/// compatible servers report `stop` where OpenAI reports `tool_calls`, and
/// promoting it would be this adapter asserting a finish the provider did not
/// state. The outstanding call is settled by its own terminal fact, which is
/// what actually keeps the Step honest.
fn finish_reason_for(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Unknown,
    }
}

fn key(index: u64) -> String {
    format!("compat:{index}")
}

/// Map a vendor error `type` string onto the closed failure vocabulary.
fn compat_error_class(kind: &str) -> ProviderFailureClass {
    match kind {
        "rate_limit_error" | "rate_limit_exceeded" => {
            ProviderFailureClass::RateLimit { retry_after: None }
        }
        "server_error" | "internal_error" => {
            ProviderFailureClass::ProviderInternal { retry_after: None }
        }
        "authentication_error" | "invalid_api_key" | "permission_error" => {
            ProviderFailureClass::Authentication
        }
        "insufficient_quota" | "billing_error" => ProviderFailureClass::QuotaExceeded,
        "invalid_request_error" | "invalid_value" | "unsupported_parameter" => {
            ProviderFailureClass::InvalidRequest
        }
        "content_filter" => ProviderFailureClass::ContentPolicy,
        "model_not_found" => ProviderFailureClass::NoRoute,
        "" => ProviderFailureClass::InvalidProviderOutput,
        _ => ProviderFailureClass::ProviderInternal { retry_after: None },
    }
}

/// Read a chat-completions usage block.
///
/// Absent is not zero. `prompt_tokens_details.cached_tokens` is the widely
/// implemented cache counter; `non_cached_input` is derived only when the total
/// it subtracts from is actually present.
fn usage_from(value: &serde_json::Value) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    normalize_compatible_usage(value)
}
