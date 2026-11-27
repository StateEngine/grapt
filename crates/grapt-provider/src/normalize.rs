//! Ordering validator for the normalized fragment alphabet.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Normalized fragment vocabulary" (the validation bullet list) and
//! §"Terms and hard invariants".
//!
//! # The corrected expectation
//!
//! A malformed provider stream **must not crash**. Red-team §B.9 found that
//! malformed fragments in the oracle are `Effect.die` crash-class defects
//! across 30 sites, bypassing `Step.Failed`. The contract's fixture #11
//! (`malformed-and-duplicate-fragment`) states the corrected expectation:
//! violations "are rejected before projection corruption; the test uses owned
//! reducer errors, not process aborts."
//!
//! So every rule here returns [`StreamViolation`], and every violation maps to
//! exactly one typed [`AttemptFailure`] via
//! [`StreamViolation::attempt_failure`]. There is no panic path in this module.
//!
//! # What this module is not
//!
//! It owns no transport, no channel, no durable state, and no retry decision.
//! It answers "is this fragment legal here, and does the attempt now have
//! visible output?" — the two facts the serialized owner needs before it may
//! append anything. The five-attempt cap is `grapt-runtime`'s
//! `StepPolicy`; that crate owns the retry bound, and this one deliberately
//! does not duplicate it.

use std::collections::HashMap;

use crate::disposition::NoVisibleOutput;
use crate::fragment::{AttemptFragment, BlockOrdinal, BoundedDetail, ToolExecution};
use grapt_types::{AttemptTerminalReason, ToolCallId};

/// A rejected fragment, named precisely enough to write a fixture against.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamViolation {
    #[error("fragment arrived before the attempt was opened")]
    NotOpened,

    #[error("attempt was opened twice")]
    AlreadyOpened,

    /// Covers the contract's "`ProviderTerminal` occurs once" rule: a second
    /// terminal fragment is simply a fragment arriving after the terminal, so
    /// there is deliberately no separate `DuplicateTerminal` variant. An error
    /// the validator can never construct would be a lie in the public API.
    #[error("fragment arrived after the stream reached its terminal fragment")]
    AfterTerminal,

    #[error("{kind} block {block} was opened while block {active} was still open")]
    BlockAlreadyOpen {
        kind: BlockKind,
        block: u32,
        active: u32,
    },

    #[error("{kind} block {block} received a delta or close with no matching open")]
    BlockNotOpen { kind: BlockKind, block: u32 },

    #[error("tool call {call} received input before it was opened")]
    ToolInputNotOpened { call: String },

    #[error("tool call {call} was opened twice")]
    ToolInputAlreadyOpened { call: String },

    #[error("tool call {call} changed name from {expected} to {actual}")]
    ToolNameChanged {
        call: String,
        expected: String,
        actual: String,
    },

    #[error("tool call {call} changed execution owner")]
    ToolExecutionChanged { call: String },

    #[error("tool call {call} was called twice")]
    ToolCalledTwice { call: String },

    #[error("tool call {call} received a hosted result but is locally executed")]
    HostedResultForLocalCall { call: String },

    #[error("tool call {call} received a hosted result before it was called")]
    HostedResultBeforeCall { call: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    Reasoning,
}

impl std::fmt::Display for BlockKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => f.write_str("text"),
            Self::Reasoning => f.write_str("reasoning"),
        }
    }
}

/// The corrected terminal outcome for a malformed stream: exactly one typed
/// ATTEMPT terminal, never a process abort.
///
/// # Why this is attempt-level, and lives here
///
/// A stream violation is a fact about ONE physical provider attempt, so the
/// reason it carries is an [`AttemptTerminalReason`]. The Step-level reason is
/// a different closed set — it adds the policy and budget outcomes only the
/// Step driver can observe — and belongs to the runtime that owns the driver.
/// Attempt and Step terminals use distinct types so their levels cannot be
/// confused at the type boundary.
///
/// Fields are private and construction is checked so no caller can build one
/// from a literal or hand it an unbounded diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptFailure {
    reason: AttemptTerminalReason,
    detail: BoundedDetail,
    diagnostic: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    limit_reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
}

impl AttemptFailure {
    /// The only constructor. Both fields are required: an attempt terminal
    /// without a typed reason cannot be constructed.
    pub fn new(reason: AttemptTerminalReason, detail: impl Into<BoundedDetail>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            diagnostic: None,
            limit_reset: None,
        }
    }

    pub fn with_usage_diagnostic(
        mut self,
        diagnostic: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    ) -> Self {
        self.diagnostic = diagnostic;
        self
    }
    pub fn usage_diagnostic(&self) -> Option<grapt_types::failure_diagnostic::UsageDiagnostic> {
        self.diagnostic
    }

    pub fn with_limit_reset(
        mut self,
        reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
    ) -> Self {
        self.limit_reset = reset;
        self
    }

    pub fn limit_reset(&self) -> Option<grapt_types::provider_limit::ProviderLimitReset> {
        self.limit_reset
    }

    pub fn reason(&self) -> &AttemptTerminalReason {
        &self.reason
    }

    /// Non-authoritative diagnosis. Nothing branches on this.
    pub fn detail(&self) -> &BoundedDetail {
        &self.detail
    }
}

impl StreamViolation {
    /// Map a violation to the one durable attempt terminal it produces.
    ///
    /// Every violation collapses to `InvalidProviderOutput`: from the session's
    /// perspective the provider emitted something the contract does not permit,
    /// which is the same class as an undecodable response. The specific
    /// violation survives in the bounded diagnostic.
    ///
    /// The collapse is deliberate — it is
    /// structure-carried, not witnessed by a mutation row, because no single
    /// edit widens it into per-violation classes without inventing vocabulary
    /// the foundational enum does not have.
    pub fn attempt_failure(&self) -> AttemptFailure {
        AttemptFailure::new(
            AttemptTerminalReason::ProviderFailure {
                class: grapt_types::ProviderFailureClass::InvalidProviderOutput,
            },
            self.to_string(),
        )
    }
}

/// What accepting a fragment did to the attempt's observable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// True when this fragment made the attempt visibly productive for the
    /// first time. The owner uses the rising edge to close the retry window.
    pub first_visible_output: bool,
    /// True when this fragment ended the stream.
    pub terminal: bool,
    /// Set when the fragment was legal to receive but deliberately not
    /// applied — a straggler. The owner records it for diagnosis and changes
    /// no durable state.
    pub ignored: Option<IgnoredEvidence>,
}

/// A fragment that arrived too late to matter and was deliberately dropped.
///
/// This is transport evidence, not a domain fact. It never produces a second
/// settlement and never fails the step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredEvidence {
    pub call: String,
    pub kind: StragglerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StragglerKind {
    /// A duplicate success after the call already settled. The oracle raises
    /// an unchecked defect here (`Duplicate tool result: <call>`); narrowing
    /// that blast radius is a disclosed behaviour change with a waiver row.
    Success,
    /// A late failure after the call already settled — benign in the oracle
    /// too, which runs to completion.
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCallState {
    tool: String,
    execution: ToolExecution,
    input_closed: bool,
    called: bool,
    settled: bool,
}

/// Validates one physical attempt's fragment sequence.
///
/// One validator instance per [`grapt_types::PhysicalAttemptId`]. It is never
/// reused across attempts — a retry mints a new attempt and therefore a new
/// validator, which is how the "one stream per attempt" law shows up here.
#[derive(Debug, Default)]
pub struct StreamValidator {
    opened: bool,
    terminated: bool,
    active_text: Option<BlockOrdinal>,
    active_reasoning: Option<BlockOrdinal>,
    calls: HashMap<ToolCallId, ToolCallState>,
    visible_output: bool,
}

impl StreamValidator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this attempt has produced visible output.
    ///
    /// Contract: "a logical Step may retry before observable output". The
    /// owner must consult this before classifying a failure as retryable —
    /// [`crate::fragment::ProviderFailureClass::is_retryable`] is necessary
    /// but not sufficient.
    pub fn visible_output(&self) -> bool {
        self.visible_output
    }

    /// Unforgeable proof that this attempt produced no visible output, or
    /// `None` once it has.
    ///
    /// This is the only source of [`NoVisibleOutput`], which
    /// [`crate::disposition::AttemptDisposition::Retry`] cannot be built
    /// without. That is what turns "retry only before visible output" from a
    /// rule someone must remember into one the compiler enforces.
    pub fn no_visible_output(&self) -> Option<NoVisibleOutput> {
        if self.visible_output {
            None
        } else {
            Some(NoVisibleOutput(()))
        }
    }

    /// Whether the stream has reached a terminal fragment.
    pub fn terminated(&self) -> bool {
        self.terminated
    }

    /// Tool calls that were started but never reached a durable terminal.
    ///
    /// Contract step 6: after provider terminal, failure, or cancellation the
    /// owner must "settle each unsettled local/hosted call with its truthful
    /// reason". This is the sweep list. Locally-executed calls appear here
    /// too — the owner settles them from its own join set, not from the
    /// stream.
    pub fn unsettled_calls(&self) -> Vec<&ToolCallId> {
        let mut unsettled: Vec<&ToolCallId> = self
            .calls
            .iter()
            .filter(|(_, state)| state.called && !state.settled)
            .map(|(id, _)| id)
            .collect();
        // HashMap iteration order is unspecified; the sweep must be
        // deterministic so fixtures can assert an exact settlement order.
        unsettled.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        unsettled
    }

    /// Validate one fragment and fold it into the attempt state.
    ///
    /// On `Err` the fragment is rejected and state is left unchanged, so a
    /// caller that chooses to continue reading cannot observe a half-applied
    /// fragment.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn accept(&mut self, fragment: &AttemptFragment) -> Result<Accepted, StreamViolation> {
        if self.terminated {
            return Err(StreamViolation::AfterTerminal);
        }
        if matches!(fragment, AttemptFragment::AttemptOpened) {
            if self.opened {
                return Err(StreamViolation::AlreadyOpened);
            }
            self.opened = true;
            return Ok(Accepted {
                first_visible_output: false,
                terminal: false,
                ignored: None,
            });
        }
        if !self.opened {
            return Err(StreamViolation::NotOpened);
        }

        match fragment {
            // Fully handled above, which always returns. Kept as an explicit
            // no-op arm so the match stays exhaustive over the closed alphabet
            // without a catch-all that could swallow a future variant.
            AttemptFragment::AttemptOpened => {}

            AttemptFragment::TextOpened { block, .. } => {
                open_block(&mut self.active_text, BlockKind::Text, *block)?;
            }
            AttemptFragment::TextDelta { block, .. }
            | AttemptFragment::TextClosed { block, .. } => {
                require_open(self.active_text, BlockKind::Text, *block)?;
                if matches!(fragment, AttemptFragment::TextClosed { .. }) {
                    self.active_text = None;
                }
            }

            AttemptFragment::ReasoningOpened { block, .. } => {
                open_block(&mut self.active_reasoning, BlockKind::Reasoning, *block)?;
            }
            AttemptFragment::ReasoningDelta { block, .. }
            | AttemptFragment::ReasoningClosed { block, .. } => {
                require_open(self.active_reasoning, BlockKind::Reasoning, *block)?;
                if matches!(fragment, AttemptFragment::ReasoningClosed { .. }) {
                    self.active_reasoning = None;
                }
            }

            AttemptFragment::ToolInputOpened {
                call,
                tool,
                execution,
                ..
            } => {
                if self.calls.contains_key(call) {
                    return Err(StreamViolation::ToolInputAlreadyOpened {
                        call: call.as_str().to_owned(),
                    });
                }
                self.calls.insert(
                    call.clone(),
                    ToolCallState {
                        tool: tool.clone(),
                        execution: *execution,
                        input_closed: false,
                        called: false,
                        settled: false,
                    },
                );
            }

            AttemptFragment::ToolInputDelta { call, tool, .. }
            | AttemptFragment::ToolInputClosed { call, tool, .. }
            | AttemptFragment::ToolInputInvalid { call, tool, .. } => {
                let state = self.call_mut(call)?;
                check_tool_name(call, &state.tool, tool)?;
                if matches!(
                    fragment,
                    AttemptFragment::ToolInputClosed { .. }
                        | AttemptFragment::ToolInputInvalid { .. }
                ) {
                    state.input_closed = true;
                }
            }

            AttemptFragment::ToolCalled {
                call,
                tool,
                execution,
                ..
            } => {
                // Contract: "ToolCalled occurs once and closes any open input
                // first." A call may also arrive without a preceding input
                // sequence, which adapters do for zero-argument tools.
                if let Some(state) = self.calls.get_mut(call) {
                    if state.called {
                        return Err(StreamViolation::ToolCalledTwice {
                            call: call.as_str().to_owned(),
                        });
                    }
                    if state.tool != *tool {
                        return Err(StreamViolation::ToolNameChanged {
                            call: call.as_str().to_owned(),
                            expected: state.tool.clone(),
                            actual: tool.clone(),
                        });
                    }
                    if state.execution != *execution {
                        return Err(StreamViolation::ToolExecutionChanged {
                            call: call.as_str().to_owned(),
                        });
                    }
                    state.input_closed = true;
                    state.called = true;
                } else {
                    self.calls.insert(
                        call.clone(),
                        ToolCallState {
                            tool: tool.clone(),
                            execution: *execution,
                            input_closed: true,
                            called: true,
                            settled: false,
                        },
                    );
                }
            }

            AttemptFragment::HostedToolResult { call, tool, .. }
            | AttemptFragment::HostedToolFailed { call, tool, .. } => {
                let state = self.call_mut(call)?;
                check_tool_name(call, &state.tool, tool)?;
                if state.execution != ToolExecution::ProviderHosted {
                    return Err(StreamViolation::HostedResultForLocalCall {
                        call: call.as_str().to_owned(),
                    });
                }
                if !state.called {
                    return Err(StreamViolation::HostedResultBeforeCall {
                        call: call.as_str().to_owned(),
                    });
                }
                if state.settled {
                    // A late hosted result after settlement is a STRAGGLER, not
                    // a step-failing violation. Evidence: the
                    // provider-straggler-asymmetry fixture — caseA (late ERROR after a
                    // success) records `toolSuccessEvents: 1, ranToCompletion:
                    // true`, so the run must survive it. Its corrected
                    // expectation extends the same treatment to caseB (a
                    // duplicate SUCCESS, which the oracle turns into an
                    // unchecked defect, "Duplicate tool result: call-dup"):
                    // "Rust records it as ignored transport evidence with a
                    // diagnostic, never a second settlement and never a crash".
                    //
                    // So: no state change (settlement stays exactly-once), no
                    // error, and the straggler is reported for diagnosis.
                    let kind = match fragment {
                        AttemptFragment::HostedToolResult { .. } => StragglerKind::Success,
                        _ => StragglerKind::Failure,
                    };
                    return Ok(Accepted {
                        first_visible_output: false,
                        terminal: false,
                        ignored: Some(IgnoredEvidence {
                            call: call.as_str().to_owned(),
                            kind,
                        }),
                    });
                }
                state.settled = true;
            }

            AttemptFragment::Usage { .. } => {}

            AttemptFragment::ProviderTerminal { .. } | AttemptFragment::ProviderFailure { .. } => {
                self.terminated = true;
            }
        }

        let newly_visible = fragment.is_visible_output() && !self.visible_output;
        if newly_visible {
            self.visible_output = true;
        }
        Ok(Accepted {
            first_visible_output: newly_visible,
            terminal: fragment.is_terminal(),
            ignored: None,
        })
    }

    fn call_mut(&mut self, call: &ToolCallId) -> Result<&mut ToolCallState, StreamViolation> {
        self.calls
            .get_mut(call)
            .ok_or_else(|| StreamViolation::ToolInputNotOpened {
                call: call.as_str().to_owned(),
            })
    }
}

fn open_block(
    slot: &mut Option<BlockOrdinal>,
    kind: BlockKind,
    block: BlockOrdinal,
) -> Result<(), StreamViolation> {
    // Contract: "one active text and reasoning block at a time ...; sequential
    // blocks are allowed and retain an ordinal".
    if let Some(active) = *slot {
        return Err(StreamViolation::BlockAlreadyOpen {
            kind,
            block: block.value(),
            active: active.value(),
        });
    }
    *slot = Some(block);
    Ok(())
}

fn require_open(
    slot: Option<BlockOrdinal>,
    kind: BlockKind,
    block: BlockOrdinal,
) -> Result<(), StreamViolation> {
    // Contract: "a delta/end requires the matching opened block".
    match slot {
        Some(active) if active == block => Ok(()),
        _ => Err(StreamViolation::BlockNotOpen {
            kind,
            block: block.value(),
        }),
    }
}

fn check_tool_name(call: &ToolCallId, expected: &str, actual: &str) -> Result<(), StreamViolation> {
    // Contract: "a tool input's name and execution owner cannot change".
    if expected == actual {
        return Ok(());
    }
    Err(StreamViolation::ToolNameChanged {
        call: call.as_str().to_owned(),
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::{AdapterState, FinishReason, ProviderFailureClass, TokenUsage};

    include!("normalize/tests.rs");
}
