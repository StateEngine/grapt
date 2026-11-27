//! The one provider stream, driving one route decoder.
//!
//! Every route family in this crate reads its bytes through this type. What
//! differs between them is [`RouteDecoder`] — what a decoded payload means —
//! and nothing else.
//!
//! # What is shared, and why it must be
//!
//! Reading a chunk, feeding the bounded SSE decoder, deciding that EOF without
//! a terminal is a transport failure, deciding that an outstanding UTF-8 tail
//! at EOF is `InvalidProviderOutput` rather than truncation, releasing the
//! capacity slot on every terminal path, and closing open blocks before a
//! failure — none of that is protocol. The Codex route's history records that
//! each of those was got wrong once and fixed once. Four
//! copies would be four chances to reintroduce any of them, and the three new
//! routes have no independent evidence that would catch a regression.
//!
//! # Cancellation
//!
//! Dropping the stream drops the response, the socket and the capacity slot
//! with it. The adapter starts no task, so there is nothing to join and nothing
//! can outlive the attempt supervisor's token. Terminal release is explicit
//! rather than deferred to that drop: a finished stream frees its slot
//! when it finishes, not when the caller happens to let go of it.

use crate::fragment::{AttemptFragment, ProviderFailureClass};
use crate::transport::blocks::BlockState;
use crate::transport::capture::{CaptureOutcome, CaptureTap};
use crate::transport::classify::failure;
use crate::transport::framing::Framing;
use crate::transport::sink::{DecodeSink, ToolAccumulations};
use crate::transport::slot::AttemptSlot;

/// Which route's meaning to apply to a decoded payload.
///
/// Enum dispatch rather than a boxed trait object: the contract forbids
/// "service/layer graph" and fiber-shaped abstraction, the set of routes is
/// closed and known at compile time, and an exhaustive `match` is what makes
/// adding a family a compile error at every place that must handle it.
#[derive(Debug)]
pub(crate) enum RouteDecoder {
    Gemini(Box<crate::gemini::Decoder>),
    Cohere(Box<crate::cohere::Decoder>),
    /// The OpenAI Responses wire shape, used by both the public Responses API
    /// and the ChatGPT/Codex backend. One decoder, because it is genuinely one
    /// protocol — the families differ in endpoint, headers and request field
    /// set, all of which are settled before a byte is decoded.
    /// Boxed: `ResponsesDecoder` is zero-sized and `AnthropicDecoder` carries a
    /// map and three retained fields, so an unboxed enum would make every
    /// Responses stream pay Anthropic's footprint.
    Responses(Box<crate::routes::responses::ResponsesDecoder>),
    /// Anthropic Messages.
    Anthropic(Box<crate::routes::anthropic::AnthropicDecoder>),
    /// OpenAI-compatible chat completions, including Grok.
    Compatible(Box<crate::routes::compat::CompatibleDecoder>),
    Bedrock(Box<crate::routes::bedrock::BedrockDecoder>),
}

impl RouteDecoder {
    fn decode(&mut self, payload: &str, sink: &mut DecodeSink<'_>) {
        match self {
            Self::Responses(decoder) => decoder.decode(payload, sink),
            Self::Anthropic(decoder) => decoder.decode(payload, sink),
            Self::Compatible(decoder) => decoder.decode(payload, sink),
            Self::Bedrock(decoder) => decoder.decode(payload, sink),
            Self::Gemini(decoder) => decoder.decode(payload, sink),
            Self::Cohere(decoder) => decoder.decode(payload, sink),
        }
    }

    fn usage_paths(&self) -> &'static [&'static str] {
        match self {
            Self::Responses(_) => &["/response/usage"],
            Self::Anthropic(_) => &["/message/usage", "/usage"],
            Self::Compatible(_) => &["/usage"],
            Self::Bedrock(_) => &["/metadata/usage"],
            Self::Gemini(_) => &["/usageMetadata"],
            Self::Cohere(_) => &["/delta/usage"],
        }
    }

    /// End of stream, before the transport decides the stream ended without a
    /// terminal.
    ///
    /// Only the chat-completions family uses it, and it must: that protocol
    /// sends its `finish_reason` chunk BEFORE its usage-only chunk, so a
    /// decoder that terminated on the finish would latch the terminal one chunk
    /// early and permanently discard the only usage a streamed response
    /// reports. The other two routes have an explicit end event and have
    /// nothing to flush.
    fn finish(&mut self, sink: &mut DecodeSink<'_>) {
        match self {
            Self::Responses(_) | Self::Anthropic(_) | Self::Bedrock(_) | Self::Cohere(_) => {}
            Self::Compatible(decoder) => decoder.finish(sink),
            Self::Gemini(decoder) => decoder.finish(sink),
        }
    }
}

/// One provider stream for one PhysicalAttempt.
#[derive(Debug)]
pub struct ProviderStream {
    response: Option<reqwest::Response>,
    http_status: u16,
    framing: Framing,
    blocks: BlockState,
    tools: ToolAccumulations,
    pending: std::collections::VecDeque<AttemptFragment>,
    route: RouteDecoder,
    finished: bool,
    redactor: super::terminal_detail::ErrorRedactor,
    error_detail: Option<crate::ProviderErrorDetail>,
    unmodeled_events: u32,
    reported_usage: Option<crate::TokenUsage>,
    usage_warning: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    /// Set only by the sink's invalid-usage path: an invalid usage drops its
    /// leaves at the terminal, while a non-fatal warning preserves them.
    usage_invalid: bool,
    slot: Option<AttemptSlot>,
    /// This attempt's capture tap, if the deployment configured an exchange
    /// and it accepted this attempt.
    ///
    /// `Option`, because capture is a deployment decision and an attempt
    /// without one must behave identically. Settlement is the tap's own `Drop`
    /// obligation, so a stream dropped mid-flight settles Incomplete without
    /// this type doing anything.
    capture: Option<CaptureTap>,
    timing: Option<crate::transport::dispatch::DispatchTiming>,
}

impl ProviderStream {
    pub(crate) fn new(
        response: reqwest::Response,
        route: RouteDecoder,
        slot: AttemptSlot,
        capture: Option<CaptureTap>,
        timing: Option<crate::transport::dispatch::DispatchTiming>,
    ) -> Self {
        let http_status = response.status().as_u16();
        let mut pending = std::collections::VecDeque::new();
        pending.push_back(AttemptFragment::AttemptOpened);
        Self {
            response: Some(response),
            http_status,
            framing: Framing::new(matches!(route, RouteDecoder::Bedrock(_))),
            blocks: BlockState::default(),
            tools: ToolAccumulations::new(),
            pending,
            route,
            finished: false,
            redactor: super::terminal_detail::ErrorRedactor::new(None),
            error_detail: None,
            unmodeled_events: 0,
            reported_usage: None,
            usage_warning: None,
            usage_invalid: false,
            slot: Some(slot),
            capture,
            timing,
        }
    }

    pub(crate) fn redacting(mut self, lease: Option<crate::credential::CredentialLease>) -> Self {
        self.redactor = super::terminal_detail::ErrorRedactor::new(lease);
        self
    }

    pub fn take_error_detail(&mut self) -> Option<crate::ProviderErrorDetail> {
        self.error_detail.take()
    }

    fn diagnose_terminal(&mut self, from: usize, payload: Option<&str>) {
        if !self
            .pending
            .iter()
            .skip(from)
            .any(AttemptFragment::is_terminal)
        {
            return;
        }
        let failed = self.pending.iter().skip(from).any(|fragment| {
            matches!(
                fragment,
                AttemptFragment::ProviderFailure { .. }
                    | AttemptFragment::ProviderTerminal {
                        finish: crate::FinishReason::ContentFilter,
                        ..
                    }
            )
        });
        let incomplete = payload
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
            .is_some_and(|value| value["type"] == "response.incomplete");
        if failed || incomplete {
            self.error_detail = Some(self.redactor.detail(self.http_status, payload));
        }
        for fragment in self.pending.iter_mut().skip(from) {
            if let AttemptFragment::ProviderFailure { message, .. } = fragment {
                *message = self.redactor.text(message);
            }
        }
    }

    /// Hand the raw chunk to capture, before any decoding.
    ///
    /// The bytes the socket produced, not the bytes this crate managed to
    /// parse: a capture taken after normalization could not explain a
    /// normalization failure, which is the case it is most needed for.
    fn capture_chunk(&mut self, bytes: &[u8]) {
        if let Some(capture) = self.capture.as_mut() {
            capture.chunk(bytes);
        }
    }

    /// Settle capture, exactly once.
    ///
    /// Taking the tap relinquishes its sender immediately, so a saturated feed
    /// observes disconnection even while this settled stream object stays
    /// alive. Drop remains the backstop for paths that never reach settlement —
    /// cancellation above all.
    fn capture_finish(&mut self, outcome: CaptureOutcome) {
        if let Some(mut capture) = self.capture.take() {
            capture.settle(outcome);
        }
    }

    /// How many events this adapter did not model.
    ///
    /// A diagnostic, not a durable fact, per the contract's "Unknown transport
    /// frames become diagnostics".
    pub fn unmodeled_events(&self) -> u32 {
        self.unmodeled_events
    }

    /// The next fragment, or `None` once the stream is over.
    pub async fn next_fragment(&mut self) -> Option<AttemptFragment> {
        loop {
            if let Some(fragment) = self.pending.pop_front() {
                return Some(fragment);
            }
            if self.finished {
                return None;
            }
            let Some(response) = self.response.as_mut() else {
                self.finished = true;
                return None;
            };
            match response.chunk().await {
                Ok(Some(chunk)) => self.feed_chunk(&chunk),
                Ok(None) => {
                    // EOF. An outstanding partial code point is its own typed
                    // refusal, distinct from the ordinary "ended without a
                    // terminal event" transport failure — a truncated character
                    // and a truncated stream are different facts.
                    match self.framing.finish() {
                        Ok(payloads) => {
                            // A frame only end-of-stream could resolve is
                            // decoded before the stream is called truncated.
                            for payload in payloads {
                                self.decode(&payload);
                            }
                            // Let the route flush a terminal it was holding for
                            // exactly this moment, BEFORE the stream calls
                            // itself truncated.
                            self.finish_route();
                            if !self.finished {
                                self.finish_with(failure(
                                    ProviderFailureClass::Transport,
                                    "the provider stream ended without a terminal event",
                                ));
                            }
                        }
                        Err(error) => self.finish_with(failure(
                            ProviderFailureClass::InvalidProviderOutput,
                            error,
                        )),
                    }
                }
                Err(_error) => {
                    // Every read fault is a transport fault. A stalled — as
                    // opposed to broken — stream is not observed here at all:
                    // `StepPolicy::no_progress_timeout` owns it. The reqwest
                    // error is not rendered into the message; it can carry the
                    // request URL.
                    self.finish_with(failure(
                        ProviderFailureClass::Transport,
                        "the provider stream read failed",
                    ));
                }
            }
        }
    }

    /// The exact socket read enters one shared framing/route path. Keeping
    /// this separate lets chunk-boundary witnesses drive that same path.
    fn feed_chunk(&mut self, chunk: &[u8]) {
        if !chunk.is_empty() {
            if let Some(timing) = self.timing.as_mut() {
                timing.first_stream_byte();
            }
        }
        self.capture_chunk(chunk);
        match self.framing.feed(chunk) {
            Ok(payloads) => {
                for payload in payloads {
                    self.decode(&payload);
                }
            }
            Err(error) => {
                self.finish_with(failure(ProviderFailureClass::InvalidProviderOutput, error))
            }
        }
    }

    /// Hand one payload to the route, then observe whether it terminated.
    ///
    /// The terminal latch lives in the sink, so a route cannot emit two; this
    /// is where the stream learns the latch closed and stops reading.
    fn decode(&mut self, payload: &str) {
        if self.finished {
            return;
        }
        let warned = self.usage_warning.is_some();
        let pending = self.pending.len();
        let mut terminated = false;
        let mut sink = DecodeSink::new(
            &mut self.blocks,
            &mut self.pending,
            &mut self.tools,
            &mut self.unmodeled_events,
            &mut terminated,
            &mut self.reported_usage,
            &mut self.usage_warning,
            &mut self.usage_invalid,
        );
        self.route.decode(payload, &mut sink);
        if !warned {
            if let Some(warning) = self.usage_warning {
                grapt_diagnostics::report(format!(
                    "provider usage warning: {} is {}; response preserved",
                    warning.field_word(),
                    warning.issue.word()
                ));
                if let Some(timing) = &mut self.timing {
                    timing.usage_warning(
                        warning,
                        self.redactor
                            .usage_detail(payload, self.route.usage_paths()),
                    );
                }
            }
        }

        self.diagnose_terminal(pending, Some(payload));
        self.observe_capacity(pending);
        // Raw SSE bytes, comments and unknown events cannot keep a wedged
        // holder alive. Only normalized provider output advances the queue.
        if self.pending.len() > pending {
            if let Some(slot) = &self.slot {
                slot.progress();
            }
        }
        self.observe_usage(pending);
        if terminated {
            self.finished = true;
            self.response = None;
            // A terminal frees capacity here, not when the caller
            // eventually drops the object.
            self.slot = None;
            self.timing = None;
            self.redactor = super::terminal_detail::ErrorRedactor::new(None);
            self.capture_finish(CaptureOutcome::Terminal);
        }
    }

    fn observe_capacity(&self, pending: usize) {
        let Some(slot) = &self.slot else {
            return;
        };
        let mut failed = false;
        for fragment in self.pending.iter().skip(pending) {
            if let AttemptFragment::ProviderFailure { class, .. } = fragment {
                failed = true;
                if let crate::ProviderFailureClass::RateLimit { retry_after } = class {
                    slot.rate_limited(*retry_after);
                }
            }
        }
        if !failed
            && self
                .pending
                .iter()
                .skip(pending)
                .any(|fragment| matches!(fragment, AttemptFragment::ProviderTerminal { .. }))
        {
            slot.successful();
        }
    }

    /// Give the route its end-of-stream hook, then observe the latch exactly
    /// as `decode` does.
    fn finish_route(&mut self) {
        if self.finished {
            return;
        }
        let pending = self.pending.len();
        let mut terminated = false;
        {
            let mut sink = DecodeSink::new(
                &mut self.blocks,
                &mut self.pending,
                &mut self.tools,
                &mut self.unmodeled_events,
                &mut terminated,
                &mut self.reported_usage,
                &mut self.usage_warning,
                &mut self.usage_invalid,
            );
            self.route.finish(&mut sink);
        }
        self.diagnose_terminal(pending, None);
        self.observe_capacity(pending);
        self.observe_usage(pending);
        if terminated {
            self.finished = true;
            self.response = None;
            self.slot = None;
            self.timing = None;
            self.redactor = super::terminal_detail::ErrorRedactor::new(None);
            self.capture_finish(CaptureOutcome::Terminal);
        }
    }

    fn observe_usage(&mut self, from: usize) {
        if let Some(timing) = &mut self.timing {
            for fragment in self.pending.iter().skip(from) {
                timing.observe(fragment);
            }
        }
    }

    /// A failure the transport itself decided, rather than one the route
    /// decoded. Closes open blocks and refuses dangling tool inputs first, for
    /// the same reason the sink's terminals do.
    fn finish_with(&mut self, fragment: AttemptFragment) {
        if self.finished {
            return;
        }
        let pending = self.pending.len();
        let mut terminated = false;
        {
            let mut sink = DecodeSink::new(
                &mut self.blocks,
                &mut self.pending,
                &mut self.tools,
                &mut self.unmodeled_events,
                &mut terminated,
                &mut self.reported_usage,
                &mut self.usage_warning,
                &mut self.usage_invalid,
            );
            sink.close_before_failure(&fragment);
        }
        self.observe_usage(pending);
        if let Some(timing) = &mut self.timing {
            timing.observe(&fragment);
        }
        self.pending.push_back(fragment);
        self.diagnose_terminal(pending, None);
        self.finished = true;
        self.response = None;
        self.slot = None;
        self.timing = None;
        self.redactor = super::terminal_detail::ErrorRedactor::new(None);
        // Incomplete: the bytes captured so far are not a whole response, and a
        // capture consumer must be able to tell that from a clean terminal.
        self.capture_finish(CaptureOutcome::Incomplete);
    }
}

#[cfg(test)]
#[path = "sse/coalesce_adapter.rs"]
mod coalesce_adapter;
