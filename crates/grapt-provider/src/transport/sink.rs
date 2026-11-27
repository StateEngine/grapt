//! The seam every route normalizes through.
//!
//! A route decoder is handed a [`DecodeSink`] and a decoded `data:` payload.
//! It says what the payload MEANS — "this is a text delta", "this closes the
//! tool input for call X", "this is the terminal" — and the sink owns
//! everything about how that becomes an [`AttemptFragment`]: ordinal
//! allocation, implicit block opening, tool-input accumulation, and the
//! terminal latch.
//!
//! # Why the routes do not build fragments themselves
//!
//! The Codex route's history records three defects that are all *bookkeeping*
//! rather than protocol: a block losing its ordinal, a delta arriving
//! without its opening, and a terminal arriving twice. Each of the three new
//! routes could reproduce all three independently, and on Anthropic the
//! interleaving that corrupts block identity is not incidental —
//! `content_block_start` makes it the normal shape.
//!
//! So the sink is the only thing in this crate that pushes an
//! `AttemptFragment`, and a route decoder cannot express the defects: it cannot
//! choose an ordinal, cannot emit a delta without the sink opening the block
//! first, and cannot emit a second terminal because [`DecodeSink::terminal`]
//! latches.
//!
//! # Tool input accumulation
//!
//! The contract requires `ToolCalled` to occur once, to close the matching
//! accumulation first, and to be "the only conversion from attempt-local
//! `ProviderToolInputKey` to provider-scoped durable `ProviderToolHandle`". All
//! three new routes stream tool arguments as text deltas that must be
//! reassembled before the call is emitted, and all three key that reassembly
//! differently (Responses by `item_id`, Anthropic by content-block index,
//! chat-completions by choice-local tool-call index). The sink therefore owns
//! the accumulation keyed by the route's own key string, bounded, and refuses
//! a name or execution owner that changes mid-accumulation — which the contract
//! names explicitly: "a tool input's name and execution owner cannot change".

use std::collections::BTreeMap;

use grapt_types::ToolCallId;

use crate::fragment::{
    AdapterState, AttemptFragment, FinishReason, ProviderFailureClass, TokenUsage, ToolExecution,
};
use crate::transport::blocks::BlockState;
use crate::transport::classify::failure;

/// The most tool-input accumulations one attempt may hold open at once.
///
/// A bound rather than a `BTreeMap` that grows with whatever the provider
/// sends: the contract's "bounded parser/output" law applies to the parser's
/// own working memory, not only to the byte buffer. `ATTEMPT_TOOL_BUDGET = 16`
/// in `grapt-runtime` bounds the tools one attempt may *execute*; this is the
/// same number for the same attempt, because an accumulation that could never
/// become an executable call is not worth retaining.
pub(crate) const MAX_OPEN_TOOL_INPUTS: usize = 16;

/// The most bytes one tool-input accumulation may reach before it is refused.
///
/// Shares [`AdapterState::MAX_BYTES`] with the stream buffer for one reason: a
/// tool input is provider-controlled text, and the capture bound is a statement
/// about how much provider-controlled text this process will hold, not about
/// which field it arrived in.
pub(crate) const MAX_TOOL_INPUT_BYTES: usize = AdapterState::MAX_BYTES;

/// One tool input being reassembled.
#[derive(Debug)]
pub(crate) struct ToolAccumulation {
    call: ToolCallId,
    tool: String,
    execution: ToolExecution,
    input: String,
    text_input: bool,
    /// Set once the accumulation exceeded its bound. The overflow is reported
    /// once, as `ToolInputInvalid`, and further deltas for that key are
    /// dropped rather than growing the buffer — the refusal has already been
    /// made, and continuing to append would be the unbounded growth the
    /// refusal exists to prevent.
    refused: bool,
}

/// What a route decoder writes into.
///
/// Borrowed for one payload. It cannot outlive the decode call, so a route
/// cannot retain it and push a fragment later from somewhere the ordering
/// rules do not apply.
#[derive(Debug)]
pub(crate) struct DecodeSink<'a> {
    blocks: &'a mut BlockState,
    pending: &'a mut std::collections::VecDeque<AttemptFragment>,
    tools: &'a mut BTreeMap<String, ToolAccumulation>,
    unmodeled: &'a mut u32,
    terminated: &'a mut bool,
    reported_usage: &'a mut Option<TokenUsage>,
    usage_warning: &'a mut Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    usage_invalid: &'a mut bool,
}

impl<'a> DecodeSink<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        blocks: &'a mut BlockState,
        pending: &'a mut std::collections::VecDeque<AttemptFragment>,
        tools: &'a mut BTreeMap<String, ToolAccumulation>,
        unmodeled: &'a mut u32,
        terminated: &'a mut bool,
        reported_usage: &'a mut Option<TokenUsage>,
        usage_warning: &'a mut Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
        usage_invalid: &'a mut bool,
    ) -> Self {
        Self {
            blocks,
            pending,
            tools,
            unmodeled,
            terminated,
            reported_usage,
            usage_warning,
            usage_invalid,
        }
    }

    /// An event this route does not model. Becomes a diagnostic count, per the
    /// contract's "Unknown transport frames become diagnostics".
    pub(crate) fn unmodeled(&mut self) {
        *self.unmodeled = self.unmodeled.saturating_add(1);
    }

    // -----------------------------------------------------------------------
    // Text and reasoning
    // -----------------------------------------------------------------------

    /// Append a text delta, opening the block first if it is not open.
    ///
    /// The implicit open is why a route cannot emit a delta without its
    /// opening: there is no method that appends a delta to a closed block.
    pub(crate) fn text_delta(&mut self, text: &str) {
        if *self.terminated {
            return;
        }
        let (block, opened) = self.blocks.open_text();
        if opened {
            self.pending.push_back(AttemptFragment::TextOpened {
                block,
                state: AdapterState::none(),
            });
        }
        self.pending.push_back(AttemptFragment::TextDelta {
            block,
            text: text.to_owned(),
            state: AdapterState::none(),
        });
    }

    /// Close the text block if one is open. A close with nothing open is a
    /// no-op rather than a stray fragment: providers repeat stop events, and a
    /// second close would be an ordering violation the validator would refuse.
    pub(crate) fn close_text(&mut self) {
        self.close_text_with_state(AdapterState::none());
    }

    pub(crate) fn close_text_with_state(&mut self, state: AdapterState) {
        if *self.terminated {
            return;
        }
        if let Some(block) = self.blocks.close_text() {
            self.pending
                .push_back(AttemptFragment::TextClosed { block, state });
        }
    }

    pub(crate) fn reasoning_delta(&mut self, text: &str) {
        if *self.terminated {
            return;
        }
        let (block, opened) = self.blocks.open_reasoning();
        if opened {
            self.pending.push_back(AttemptFragment::ReasoningOpened {
                block,
                state: AdapterState::none(),
            });
        }
        self.pending.push_back(AttemptFragment::ReasoningDelta {
            block,
            text: text.to_owned(),
            state: AdapterState::none(),
        });
    }

    pub(crate) fn close_reasoning(&mut self) {
        self.close_reasoning_with_state(AdapterState::none());
    }

    pub(crate) fn close_reasoning_with_state(&mut self, state: AdapterState) {
        if *self.terminated {
            return;
        }
        if let Some(block) = self.blocks.close_reasoning() {
            self.pending
                .push_back(AttemptFragment::ReasoningClosed { block, state });
        }
    }

    // -----------------------------------------------------------------------
    // Tool input accumulation
    // -----------------------------------------------------------------------

    /// Begin accumulating a tool input under this route's own key.
    ///
    /// `key` is attempt-local and route-shaped (a Responses `item_id`, an
    /// Anthropic block index, a chat-completions tool-call index). `call` is
    /// the provider's own call identifier, which is what becomes durable.
    /// Keeping them separate is the contract's `ProviderToolInputKey` /
    /// `ProviderToolHandle` distinction: the accumulation key is bookkeeping
    /// and dies with the attempt, the call id is the durable handle.
    pub(crate) fn tool_input_opened(
        &mut self,
        key: &str,
        call: ToolCallId,
        tool: &str,
        execution: ToolExecution,
    ) {
        if *self.terminated {
            return;
        }
        if self.tools.contains_key(key) {
            // A duplicate opening for a key already accumulating. Not a
            // second fragment: the contract makes a duplicate start invalid,
            // and re-opening would silently discard whatever had accumulated.
            self.unmodeled();
            return;
        }
        if self.tools.len() >= MAX_OPEN_TOOL_INPUTS {
            self.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "the provider opened more concurrent tool inputs than this attempt admits",
            );
            return;
        }
        self.pending.push_back(AttemptFragment::ToolInputOpened {
            call: call.clone(),
            tool: tool.to_owned(),
            execution,
            state: AdapterState::none(),
        });
        self.tools.insert(
            key.to_owned(),
            ToolAccumulation {
                call,
                tool: tool.to_owned(),
                execution,
                input: String::new(),
                text_input: false,
                refused: false,
            },
        );
    }

    /// Text tools enter the same bounded accumulation and execution path. Their
    /// committed arguments use one stable JSON object on every provider family.
    pub(crate) fn text_tool_input_opened(&mut self, key: &str, call: ToolCallId, tool: &str) {
        if self.tools.contains_key(key) {
            self.unmodeled();
            return;
        }
        self.tool_input_opened(key, call, tool, ToolExecution::Local);
        if let Some(accumulation) = self.tools.get_mut(key) {
            accumulation.text_input = true;
        }
    }

    /// Append to an open accumulation, bounded.
    ///
    /// An unknown key is a diagnostic, not a failure: providers emit argument
    /// deltas for items this adapter chose not to model, and failing the whole
    /// attempt over one would be a harsher rule than the contract's.
    pub(crate) fn tool_input_delta(&mut self, key: &str, text: &str) {
        if *self.terminated {
            return;
        }
        let Some(accumulation) = self.tools.get_mut(key) else {
            *self.unmodeled = self.unmodeled.saturating_add(1);
            return;
        };
        if accumulation.refused {
            return;
        }
        // Checked BEFORE growth, against the projected length, so the bound is
        // a bound on allocation rather than a report made after the fact.
        if accumulation.input.len().saturating_add(text.len()) > MAX_TOOL_INPUT_BYTES {
            accumulation.refused = true;
            let call = accumulation.call.clone();
            let tool = accumulation.tool.clone();
            accumulation.input.clear();
            self.pending.push_back(AttemptFragment::ToolInputInvalid {
                call,
                tool,
                raw: String::new(),
            });
            return;
        }
        accumulation.input.push_str(text);
        let call = accumulation.call.clone();
        let tool = accumulation.tool.clone();
        self.pending.push_back(AttemptFragment::ToolInputDelta {
            call,
            tool,
            text: text.to_owned(),
        });
    }

    /// The complete arguments a provider reports as it finishes a call.
    ///
    /// A provider may send a call's arguments only here, with no deltas; they
    /// are then the call's input, admitted through the same bounded delta
    /// path. When deltas did arrive, the final string must equal them: a
    /// disagreement fails the attempt rather than guessing which one the model
    /// meant. Returns false when it failed the attempt.
    pub(crate) fn tool_input_final(&mut self, key: &str, arguments: Option<&str>) -> bool {
        if *self.terminated {
            return true;
        }
        let Some(arguments) = arguments else {
            return true;
        };
        let Some(accumulation) = self.tools.get(key) else {
            return true;
        };
        if accumulation.refused {
            return true;
        }
        if accumulation.input.is_empty() {
            self.tool_input_delta(key, arguments);
            return true;
        }
        if accumulation.input != arguments {
            self.fail(
                ProviderFailureClass::InvalidProviderOutput,
                "the provider's final tool arguments disagree with its streamed ones",
            );
            return false;
        }
        true
    }

    /// Close the accumulation and emit the call.
    ///
    /// This is the contract's single conversion point: `ToolInputClosed` then
    /// `ToolCalled`, in that order, exactly once. The accumulation is REMOVED,
    /// so a second close for the same key finds nothing and is a diagnostic —
    /// it cannot produce a second `ToolCalled`.
    ///
    /// A refused (over-bound) accumulation is closed WITHOUT a `ToolCalled`:
    /// its `ToolInputInvalid` already told the truth, and calling a tool with
    /// input this adapter admits it lost would be worse than not calling it.
    pub(crate) fn tool_input_closed(&mut self, key: &str) -> bool {
        self.tool_input_closed_with_state(key, AdapterState::none())
    }
    pub(crate) fn tool_input_closed_with_state(&mut self, key: &str, state: AdapterState) -> bool {
        if *self.terminated {
            return false;
        }
        let Some(accumulation) = self.tools.remove(key) else {
            self.unmodeled();
            return false;
        };
        self.pending.push_back(AttemptFragment::ToolInputClosed {
            call: accumulation.call.clone(),
            tool: accumulation.tool.clone(),
            state: AdapterState::none(),
        });
        if accumulation.refused {
            return false;
        }
        // A call with no arguments is a call with an empty object, as
        // the frozen engine reads it; models send "" for parameterless tools.
        let input = if accumulation.text_input {
            serde_json::json!({"input":accumulation.input}).to_string()
        } else if accumulation.input.trim().is_empty() {
            "{}".to_owned()
        } else {
            accumulation.input
        };
        // JSON escaping can grow text by at most six times its bounded raw
        // length. The normalized input must still fit the executor's bound.
        if input.len() > MAX_TOOL_INPUT_BYTES {
            self.pending.push_back(AttemptFragment::ToolInputInvalid {
                call: accumulation.call,
                tool: accumulation.tool,
                raw: String::new(),
            });
            return false;
        }
        self.pending.push_back(AttemptFragment::ToolCalled {
            call: accumulation.call,
            tool: accumulation.tool,
            input,
            execution: accumulation.execution,
            state,
        });
        true
    }

    /// Record a provider-executed terminal after its ordinary call fragments.
    pub(crate) fn hosted_tool_terminal(
        &mut self,
        call: ToolCallId,
        tool: &str,
        result: Result<String, String>,
        state: AdapterState,
    ) {
        if *self.terminated {
            return;
        }
        self.pending.push_back(match result {
            Ok(result) => AttemptFragment::HostedToolResult {
                call,
                tool: tool.to_owned(),
                result,
                state,
            },
            Err(error) => AttemptFragment::HostedToolFailed {
                call,
                tool: tool.to_owned(),
                error,
                state,
            },
        });
    }

    /// Close every still-open accumulation.
    ///
    /// Called when the stream reaches a terminal with tool inputs outstanding.
    /// A provider that ends mid-argument has produced a tool input this adapter
    /// cannot complete, and dropping it silently would leave a Step that looks
    /// finished while the model was asking for a tool.
    pub(crate) fn close_dangling_tool_inputs(&mut self) {
        let keys: Vec<String> = self.tools.keys().cloned().collect();
        for key in keys {
            let Some(accumulation) = self.tools.remove(&key) else {
                continue;
            };
            self.pending.push_back(AttemptFragment::ToolInputInvalid {
                call: accumulation.call.clone(),
                tool: accumulation.tool.clone(),
                raw: String::new(),
            });
            self.pending.push_back(AttemptFragment::ToolInputClosed {
                call: accumulation.call,
                tool: accumulation.tool,
                state: AdapterState::none(),
            });
        }
    }

    // -----------------------------------------------------------------------
    // Terminals
    // -----------------------------------------------------------------------

    /// The provider's own terminal. Latches: a second one is impossible.
    ///
    /// Blocks are closed first, and dangling tool inputs are refused first, so
    /// the terminal is genuinely last — which is what the validator's "no
    /// fragment follows either terminal fragment" rule requires.
    pub(crate) fn terminal(&mut self, finish: FinishReason, usage: Option<TokenUsage>) {
        if *self.terminated {
            return;
        }
        self.close_dangling_tool_inputs();
        self.close_text();
        self.close_reasoning();
        *self.reported_usage = None;
        *self.terminated = true;
        self.pending.push_back(AttemptFragment::ProviderTerminal {
            finish,
            // Only a declared-invalid usage drops the leaves; a non-fatal
            // accounting warning (the family conflict) preserves them.
            usage: usage.filter(|_| !*self.usage_invalid),
            state: AdapterState::none(),
        });
    }

    /// A typed failure terminal. Latches, exactly as [`Self::terminal`] does.
    pub(crate) fn fail(&mut self, class: ProviderFailureClass, message: impl Into<String>) {
        self.fail_classified(class, None, message);
    }

    pub(crate) fn fail_classified(
        &mut self,
        class: ProviderFailureClass,
        classification: Option<crate::FailureClassification>,
        message: impl Into<String>,
    ) {
        if *self.terminated {
            return;
        }
        let mut fragment = failure(class, message);
        if let AttemptFragment::ProviderFailure {
            classification: held,
            ..
        } = &mut fragment
        {
            *held = classification;
        }
        self.close_before_failure(&fragment);
        *self.terminated = true;
        self.pending.push_back(fragment);
    }

    /// Settle open state ahead of a failure terminal. Dangling tool inputs are
    /// always refused. Open text and reasoning close into durable items only
    /// when the failure ends the Step's attempts: a failure that may retry
    /// leaves them unfinished, so the owner discards them.
    pub(crate) fn close_before_failure(&mut self, failure: &AttemptFragment) {
        if let Some(usage) = self.reported_usage.take() {
            self.pending.push_back(AttemptFragment::Usage { usage });
        }
        self.close_dangling_tool_inputs();
        if !failure.may_retry() {
            self.close_text();
            self.close_reasoning();
        }
    }

    /// Usage problems are recorded as unknown and never fail a Step.
    /// Retain one closed warning here without payload or native I/O, and
    /// mark the usage invalid so no leaf reaches a fragment.
    pub(crate) fn invalid_usage(&mut self, error: crate::usage::UsageNormalizationError) {
        if *self.terminated {
            return;
        }
        self.usage_warning.get_or_insert(error.diagnostic());
        *self.usage_invalid = true;
        *self.reported_usage = None;
    }

    /// Record the family conflict a merge reduced to an unstamped total.
    /// This is the existing non-fatal channel: the sample carries the
    /// closed warning, the turn stays a success, and the valid usage leaves
    /// are preserved — a conflict does not make the usage invalid.
    pub(crate) fn usage_protocol_conflict(&mut self) {
        if !*self.terminated {
            self.usage_warning
                .get_or_insert(grapt_types::failure_diagnostic::UsageDiagnostic {
                    field: None,
                    issue: grapt_types::failure_diagnostic::UsageIssue::ConflictingSnapshots,
                });
        }
    }

    /// Keep one validated snapshot until completion or failure. A failure
    /// flushes it once, ahead of the terminal; successive cumulative reports
    /// must not become conflicting observations in the same durable phase.
    /// A non-fatal accounting warning keeps the channel open: only a
    /// declared-invalid usage bars the snapshot, the same invalidity the
    /// terminal honors, so a warning never freezes a stale count.
    pub(crate) fn remember_usage(&mut self, usage: TokenUsage) {
        if !*self.terminated && !*self.usage_invalid {
            *self.reported_usage = Some(usage);
        }
    }
}

/// The tool accumulation map's type, named so the stream can own one without
/// naming the private accumulation struct.
pub(crate) type ToolAccumulations = BTreeMap<String, ToolAccumulation>;
