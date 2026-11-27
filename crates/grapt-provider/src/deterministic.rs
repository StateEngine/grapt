//! Deterministic adapter — the Rust counterpart of the oracle's `TestLLM`.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Initial rulings" — "the first adapter sequence is the deterministic test
//! adapter, then the ChatGPT/Codex route". This slice is the deterministic one;
//! it performs no I/O and reaches no provider.
//!
//! # Determinism
//!
//! No clock, no randomness, no map iteration order reaches the output. A
//! script is an ordered list of fragments and replaying it twice yields the
//! same bytes in the same order, so a fixture failure is reproducible from the
//! script alone.
//!
//! # Legal by construction, illegal on purpose
//!
//! [`ScriptBuilder`] can only emit sequences the validator accepts: it tracks
//! block ordinals and tool state itself, so a fixture author cannot
//! accidentally write an out-of-order transcript. Negative fixtures — the ones
//! that must prove a malformed stream produces one `Step.Failed` rather than a
//! crash — use [`ScriptBuilder::raw`], which is the single deliberate hole in
//! that guarantee and is named so it cannot be used by accident.
//!
//! # One stream per attempt
//!
//! Contract rule 1: "One PhysicalAttempt performs exactly one provider
//! `stream(request)` call. Reconnecting, retrying, compaction recovery, or
//! resuming starts a distinct attempt with a distinct ID." Oracle anchor:
//! `llm.ts:355`, the single stream boundary.
//!
//! [`DeterministicAdapter::open`] takes the [`PhysicalAttemptId`] **by value**
//! and refuses an id it has already served, so a retry that reuses its
//! attempt identity fails at the adapter rather than silently producing a
//! second stream.

use std::collections::HashSet;

use grapt_types::{PhysicalAttemptId, ToolCallId};

use crate::fragment::{
    AdapterState, AttemptFragment, BlockOrdinal, FailureClassification, FinishReason,
    ProviderFailureClass, TokenUsage, ToolExecution,
};

/// An ordered, replayable fragment sequence for exactly one attempt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Script {
    fragments: Vec<AttemptFragment>,
}

impl Script {
    pub fn fragments(&self) -> &[AttemptFragment] {
        &self.fragments
    }

    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

/// Builds a [`Script`] that the validator accepts, without the author having
/// to track ordinals or tool state.
#[derive(Debug, Default)]
pub struct ScriptBuilder {
    fragments: Vec<AttemptFragment>,
    next_text_block: u32,
    next_reasoning_block: u32,
    opened: bool,
}

impl ScriptBuilder {
    /// Start a script. The `AttemptOpened` fragment is emitted here so no
    /// caller can forget it.
    pub fn new() -> Self {
        Self {
            fragments: vec![AttemptFragment::AttemptOpened],
            next_text_block: 0,
            next_reasoning_block: 0,
            opened: true,
        }
    }

    /// Start a script *without* `AttemptOpened`, for the negative fixture that
    /// proves a fragment arriving before the attempt opens is rejected.
    pub fn unopened() -> Self {
        Self {
            fragments: Vec::new(),
            next_text_block: 0,
            next_reasoning_block: 0,
            opened: false,
        }
    }

    /// One complete text block: opened, one delta per chunk, closed.
    pub fn text(mut self, chunks: &[&str]) -> Self {
        let block = BlockOrdinal::new(self.next_text_block);
        self.next_text_block = self.next_text_block.saturating_add(1);
        self.fragments.push(AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        });
        for chunk in chunks {
            self.fragments.push(AttemptFragment::TextDelta {
                block,
                text: (*chunk).to_owned(),
                state: AdapterState::none(),
            });
        }
        self.fragments.push(AttemptFragment::TextClosed {
            block,
            state: AdapterState::none(),
        });
        self
    }

    /// One complete reasoning block.
    pub fn reasoning(mut self, chunks: &[&str]) -> Self {
        let block = BlockOrdinal::new(self.next_reasoning_block);
        self.next_reasoning_block = self.next_reasoning_block.saturating_add(1);
        self.fragments.push(AttemptFragment::ReasoningOpened {
            block,
            state: AdapterState::none(),
        });
        for chunk in chunks {
            self.fragments.push(AttemptFragment::ReasoningDelta {
                block,
                text: (*chunk).to_owned(),
                state: AdapterState::none(),
            });
        }
        self.fragments.push(AttemptFragment::ReasoningClosed {
            block,
            state: AdapterState::none(),
        });
        self
    }

    /// A tool call with a streamed input, ending in `ToolCalled`.
    pub fn tool_call(
        mut self,
        call: &ToolCallId,
        tool: &str,
        execution: ToolExecution,
        input_chunks: &[&str],
    ) -> Self {
        self.fragments.push(AttemptFragment::ToolInputOpened {
            call: call.clone(),
            tool: tool.to_owned(),
            execution,
            state: AdapterState::none(),
        });
        for chunk in input_chunks {
            self.fragments.push(AttemptFragment::ToolInputDelta {
                call: call.clone(),
                tool: tool.to_owned(),
                text: (*chunk).to_owned(),
            });
        }
        self.fragments.push(AttemptFragment::ToolInputClosed {
            call: call.clone(),
            tool: tool.to_owned(),
            state: AdapterState::none(),
        });
        self.fragments.push(AttemptFragment::ToolCalled {
            call: call.clone(),
            tool: tool.to_owned(),
            input: input_chunks.concat(),
            execution,
            state: AdapterState::none(),
        });
        self
    }

    /// Settle a provider-hosted call with a result.
    pub fn hosted_result(mut self, call: &ToolCallId, tool: &str, result: &str) -> Self {
        self.fragments.push(AttemptFragment::HostedToolResult {
            call: call.clone(),
            tool: tool.to_owned(),
            result: result.to_owned(),
            state: AdapterState::none(),
        });
        self
    }

    /// Settle a provider-hosted call with a failure.
    pub fn hosted_failed(mut self, call: &ToolCallId, tool: &str, error: &str) -> Self {
        self.fragments.push(AttemptFragment::HostedToolFailed {
            call: call.clone(),
            tool: tool.to_owned(),
            error: error.to_owned(),
            state: AdapterState::none(),
        });
        self
    }

    pub fn usage(mut self, usage: TokenUsage) -> Self {
        self.fragments.push(AttemptFragment::Usage { usage });
        self
    }

    /// Close the stream cleanly.
    pub fn finish(mut self, finish: FinishReason, usage: Option<TokenUsage>) -> Script {
        self.fragments.push(AttemptFragment::ProviderTerminal {
            finish,
            usage,
            state: AdapterState::none(),
        });
        Script {
            fragments: self.fragments,
        }
    }

    /// Close the stream with a provider failure.
    pub fn fail(
        mut self,
        class: ProviderFailureClass,
        classification: Option<FailureClassification>,
        message: &str,
    ) -> Script {
        self.fragments.push(AttemptFragment::ProviderFailure {
            class,
            classification,
            diagnostic: None,
            message: message.to_owned(),
            state: AdapterState::none(),
        });
        Script {
            fragments: self.fragments,
        }
    }

    /// End the script without any terminal fragment.
    ///
    /// This models a stream that stops producing — the no-progress timeout and
    /// interrupt-midstream fixtures need it. The owner, not the adapter, is
    /// responsible for noticing the silence.
    pub fn truncated(self) -> Script {
        Script {
            fragments: self.fragments,
        }
    }

    /// Append an arbitrary fragment, bypassing every construction guarantee.
    ///
    /// The one deliberate hole in "legal by construction". It exists so the
    /// malformed-fragment fixtures can be written at all, and it is named
    /// `raw` so its use is visible in review.
    pub fn raw(mut self, fragment: AttemptFragment) -> Self {
        if !self.opened {
            // Keep the unopened builder honest: raw must not implicitly open.
            return Self {
                fragments: {
                    let mut f = self.fragments;
                    f.push(fragment);
                    f
                },
                ..self
            };
        }
        self.fragments.push(fragment);
        self
    }
}

/// A single attempt's stream. Consuming it is the only way to read fragments,
/// and it cannot be cloned or restarted.
#[derive(Debug)]
pub struct AttemptStream {
    attempt: PhysicalAttemptId,
    remaining: std::vec::IntoIter<AttemptFragment>,
}

impl AttemptStream {
    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }
}

impl Iterator for AttemptStream {
    type Item = AttemptFragment;

    fn next(&mut self) -> Option<Self::Item> {
        self.remaining.next()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// Contract rule 1 violated: a retry reused its attempt identity instead
    /// of minting a new one.
    #[error("attempt {attempt} was already streamed; a retry must mint a new PhysicalAttemptId")]
    AttemptReused { attempt: String },

    #[error("adapter has no script left to serve")]
    Exhausted,
}

/// Serves scripted streams, one per attempt, in order.
#[derive(Debug, Default)]
pub struct DeterministicAdapter {
    scripts: std::collections::VecDeque<Script>,
    served: HashSet<PhysicalAttemptId>,
}

impl DeterministicAdapter {
    /// One script per attempt the run is expected to make, in order. A step
    /// that retries twice needs three scripts.
    pub fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        Self {
            scripts: scripts.into_iter().collect(),
            served: HashSet::new(),
        }
    }

    pub fn remaining_scripts(&self) -> usize {
        self.scripts.len()
    }

    /// Open the stream for one attempt.
    ///
    /// Takes the id by value and records it, so the same attempt cannot be
    /// streamed twice even if the caller kept a clone.
    pub fn open(&mut self, attempt: PhysicalAttemptId) -> Result<AttemptStream, AdapterError> {
        if self.served.contains(&attempt) {
            return Err(AdapterError::AttemptReused {
                attempt: attempt.as_str().to_owned(),
            });
        }
        let script = self.scripts.pop_front().ok_or(AdapterError::Exhausted)?;
        self.served.insert(attempt.clone());
        Ok(AttemptStream {
            attempt,
            remaining: script.fragments.into_iter(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::{StreamValidator, StreamViolation};
    use grapt_types::usage::UsageField;
    use grapt_types::TokenCount;

    fn reported(value: u64) -> UsageField {
        match TokenCount::new(value) {
            Ok(count) => UsageField::reported(count),
            Err(error) => panic!("fixture token count is bounded: {error}"),
        }
    }

    fn deterministic_usage(
        input: u64,
        output: u64,
        reasoning: Option<u64>,
        total: Option<u64>,
    ) -> TokenUsage {
        TokenUsage::new(
            reported(input),
            reported(input),
            reported(output),
            reasoning.map_or(UsageField::ABSENT, reported),
            UsageField::ABSENT,
            UsageField::ABSENT,
            total.map_or(UsageField::ABSENT, reported),
        )
    }

    fn attempt(id: &str) -> PhysicalAttemptId {
        match PhysicalAttemptId::new(id) {
            Some(value) => value,
            None => panic!("test attempt id must be non-empty"),
        }
    }

    fn call(id: &str) -> ToolCallId {
        match ToolCallId::new(id) {
            Some(value) => value,
            None => panic!("test call id must be non-empty"),
        }
    }

    /// Drive a script through the validator, returning the first violation.
    fn validate(script: &Script) -> Result<StreamValidator, StreamViolation> {
        let mut validator = StreamValidator::new();
        for fragment in script.fragments() {
            validator.accept(fragment)?;
        }
        Ok(validator)
    }

    #[test]
    fn builder_output_is_legal_by_construction() {
        let script = ScriptBuilder::new()
            .reasoning(&["think ", "harder"])
            .text(&["hello ", "world"])
            .text(&["second block"])
            .usage(deterministic_usage(10, 4, None, None))
            .finish(FinishReason::Stop, None);

        match validate(&script) {
            Ok(validator) => {
                assert!(validator.terminated());
                assert!(validator.visible_output());
                assert!(validator.unsettled_calls().is_empty());
            }
            Err(violation) => panic!("builder produced an illegal script: {violation}"),
        }
    }

    #[test]
    fn tool_scripts_are_legal_and_settle_correctly() {
        let local = call("call_local");
        let hosted = call("call_hosted");
        let script = ScriptBuilder::new()
            .tool_call(&local, "read", ToolExecution::Local, &["{\"p\":", "1}"])
            .tool_call(&hosted, "web", ToolExecution::ProviderHosted, &["{}"])
            .hosted_result(&hosted, "web", "ok")
            .finish(FinishReason::ToolCalls, None);

        match validate(&script) {
            Ok(validator) => {
                // The hosted call settled in-stream; the local one is the
                // owner's responsibility and must appear in the sweep.
                let unsettled: Vec<&str> = validator
                    .unsettled_calls()
                    .iter()
                    .map(|id| id.as_str())
                    .collect();
                assert_eq!(unsettled, vec!["call_local"]);
            }
            Err(violation) => panic!("tool script must be legal: {violation}"),
        }
    }

    #[test]
    fn replay_is_deterministic() {
        let build = || {
            ScriptBuilder::new()
                .text(&["a", "b"])
                .tool_call(&call("c1"), "read", ToolExecution::Local, &["{}"])
                .finish(FinishReason::Stop, None)
        };
        assert_eq!(build(), build(), "same script twice must be identical");

        let mut adapter = DeterministicAdapter::new([build(), build()]);
        let first: Vec<AttemptFragment> = match adapter.open(attempt("att_1")) {
            Ok(stream) => stream.collect(),
            Err(error) => panic!("first open must succeed: {error}"),
        };
        let second: Vec<AttemptFragment> = match adapter.open(attempt("att_2")) {
            Ok(stream) => stream.collect(),
            Err(error) => panic!("second open must succeed: {error}"),
        };
        assert_eq!(first, second, "replay must be byte-for-byte identical");
    }

    #[test]
    fn one_stream_per_attempt_is_enforced() {
        // Contract rule 1 / llm.ts:355. A retry must mint a new attempt id.
        let mut adapter = DeterministicAdapter::new([
            ScriptBuilder::new().finish(FinishReason::Stop, None),
            ScriptBuilder::new().finish(FinishReason::Stop, None),
        ]);
        let id = attempt("att_1");
        assert!(adapter.open(id.clone()).is_ok());
        match adapter.open(id) {
            Err(error) => assert_eq!(
                error,
                AdapterError::AttemptReused {
                    attempt: "att_1".into()
                }
            ),
            Ok(_) => panic!("a reused attempt id must not produce a second stream"),
        }
    }

    #[test]
    fn adapter_reports_exhaustion_rather_than_inventing_a_stream() {
        let mut adapter = DeterministicAdapter::new([]);
        match adapter.open(attempt("att_1")) {
            Err(error) => assert_eq!(error, AdapterError::Exhausted),
            Ok(_) => panic!("an exhausted adapter must not invent a stream"),
        }
    }

    #[test]
    fn stream_carries_its_attempt_identity() {
        let mut adapter =
            DeterministicAdapter::new([ScriptBuilder::new().finish(FinishReason::Stop, None)]);
        match adapter.open(attempt("att_7")) {
            Ok(stream) => assert_eq!(stream.attempt().as_str(), "att_7"),
            Err(error) => panic!("open must succeed: {error}"),
        }
    }

    // ---- negative fixtures: malformed must be typed, never a crash --------

    #[test]
    fn malformed_script_yields_one_step_failure_not_a_panic() {
        // Contract fixture #11 (malformed-and-duplicate-fragment), corrected
        // expectation for the oracle's Effect.die defect (red-team B.9).
        let malformed = ScriptBuilder::new()
            .text(&["hello"])
            // A delta for a block that was already closed.
            .raw(AttemptFragment::TextDelta {
                block: BlockOrdinal::new(0),
                text: "late".into(),
                state: AdapterState::none(),
            })
            .finish(FinishReason::Stop, None);

        match validate(&malformed) {
            Ok(_) => panic!("malformed script must be rejected"),
            Err(violation) => {
                let failure = violation.attempt_failure();
                assert_eq!(
                    failure.reason(),
                    &grapt_types::AttemptTerminalReason::ProviderFailure {
                        class: grapt_types::ProviderFailureClass::InvalidProviderOutput
                    }
                );
                assert!(failure.detail().as_str().contains("text block 0"));
            }
        }
    }

    #[test]
    fn duplicate_terminal_script_is_rejected() {
        let mut script = ScriptBuilder::new().finish(FinishReason::Stop, None);
        script.fragments.push(AttemptFragment::ProviderTerminal {
            finish: FinishReason::Stop,
            usage: None,
            state: AdapterState::none(),
        });
        match validate(&script) {
            Ok(_) => panic!("a second terminal must be rejected"),
            Err(violation) => assert_eq!(violation, StreamViolation::AfterTerminal),
        }
    }

    #[test]
    fn unopened_script_is_rejected_before_anything_else() {
        let script = ScriptBuilder::unopened()
            .raw(AttemptFragment::TextOpened {
                block: BlockOrdinal::new(0),
                state: AdapterState::none(),
            })
            .truncated();
        match validate(&script) {
            Ok(_) => panic!("a script without AttemptOpened must be rejected"),
            Err(violation) => assert_eq!(violation, StreamViolation::NotOpened),
        }
    }

    // ---- named fixtures from the contract --------------------------------

    #[test]
    fn fixture_text_reasoning_usage_stop() {
        // Contract fixture #2.
        let script = ScriptBuilder::new()
            .reasoning(&["weighing options"])
            .text(&["the answer"])
            .usage(deterministic_usage(120, 30, Some(12), Some(150)))
            .finish(FinishReason::Stop, None);
        assert!(validate(&script).is_ok());
    }

    #[test]
    fn fixture_visible_output_no_retry() {
        // Contract fixture #5: a text delta then a rate limit. The class alone
        // is retryable, but visible output has occurred, so the owner must not
        // retry. Both halves of that precondition are asserted here.
        let script = ScriptBuilder::new().text(&["partial"]).fail(
            ProviderFailureClass::RateLimit { retry_after: None },
            None,
            "429 slow down",
        );
        match validate(&script) {
            Ok(validator) => {
                assert!(validator.visible_output(), "text delta is visible output");
                assert!(
                    ProviderFailureClass::RateLimit { retry_after: None }.is_retryable(),
                    "class is retryable in isolation"
                );
                // Therefore the owner's rule — retry only before visible
                // output — is what forbids the retry here.
            }
            Err(violation) => panic!("script must be legal: {violation}"),
        }
    }

    #[test]
    fn fixture_preflight_transport_retry() {
        // Contract fixture #4: first stream fails before any output. Retry is
        // permitted precisely because visible_output is false.
        let script = ScriptBuilder::new().fail(
            ProviderFailureClass::Transport,
            None,
            "connection reset before first byte",
        );
        match validate(&script) {
            Ok(validator) => {
                assert!(!validator.visible_output(), "nothing was emitted");
                assert!(ProviderFailureClass::Transport.is_retryable());
            }
            Err(violation) => panic!("script must be legal: {violation}"),
        }
    }

    #[test]
    fn fixture_virgin_overflow_carries_its_classification() {
        // Contract fixture #6: overflow before visible output may drive one
        // compaction rebuild. Overflow rides the second axis, not the retry
        // class — this asserts the two stay distinct.
        let script = ScriptBuilder::new().fail(
            ProviderFailureClass::InvalidRequest,
            Some(FailureClassification::ContextOverflow),
            "context window exceeded",
        );
        match validate(&script) {
            Ok(validator) => assert!(!validator.visible_output()),
            Err(violation) => panic!("script must be legal: {violation}"),
        }
        match script.fragments().last() {
            Some(AttemptFragment::ProviderFailure {
                class,
                classification,
                diagnostic: None,
                ..
            }) => {
                assert!(!class.is_retryable(), "overflow is not a retry");
                assert_eq!(
                    *classification,
                    Some(FailureClassification::ContextOverflow)
                );
            }
            other => panic!("expected a provider failure, got {other:?}"),
        }
    }

    #[test]
    fn fixture_hosted_tool_eof() {
        // Contract fixture #8: a hosted call with no result at EOF must appear
        // in the sweep so the owner can write one tool.result-missing fact.
        let hosted = call("call_hosted");
        let script = ScriptBuilder::new()
            .tool_call(&hosted, "web", ToolExecution::ProviderHosted, &["{}"])
            .finish(FinishReason::ToolCalls, None);
        match validate(&script) {
            Ok(validator) => {
                let unsettled: Vec<&str> = validator
                    .unsettled_calls()
                    .iter()
                    .map(|id| id.as_str())
                    .collect();
                assert_eq!(unsettled, vec!["call_hosted"]);
            }
            Err(violation) => panic!("script must be legal: {violation}"),
        }
    }

    #[test]
    fn fixture_interrupt_midstream_truncates_without_terminal() {
        // Contract fixture #9: the stream simply stops. No terminal fragment
        // appears, and open blocks stay open — the owner is what settles them.
        let script = ScriptBuilder::new().text(&["half a sen"]).truncated();
        match validate(&script) {
            Ok(validator) => {
                assert!(!validator.terminated(), "no terminal was emitted");
                assert!(validator.visible_output());
            }
            Err(violation) => panic!("script must be legal: {violation}"),
        }
    }
}
