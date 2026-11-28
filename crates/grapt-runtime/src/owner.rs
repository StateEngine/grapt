//! The serialized owner boundary.
//!
//! Contract §"Attempt lifecycle, tools, and durable order", step 4: the owner
//! consumes normalized fragments, "append\[s\] durable facts/projection updates
//! in owner order, then offer\[s\] live notification **after commit**".
//!
//! (The brackets are escaped because rustdoc otherwise reads `[s]` as an
//! intra-doc link to a type named `s` — the required `-Dwarnings` doc gate fails
//! on it.)
//!
//! Two rules are encoded here rather than left to discipline:
//!
//! 1. [`AttemptOwner::commit`] is **synchronous**. It takes `&mut self` and
//!    returns a plain `Result` — there is no `async fn` to await, so a
//!    database transaction or an aggregate borrow physically cannot be held
//!    across an `.await` inside it. That is contract rule 5.
//! 2. [`AttemptOwner::notify`] is called only after `commit` returns `Ok`.
//!    Live notification is "a best-effort projection of already committed
//!    facts"; it is never a condition of correctness.
//!
//! This crate deliberately does **not** define the durable schema. The
//! store crate owns the write path. [`DurableFact`] is the minimal shape the
//! supervisor needs to prove ordering, and is expected to be replaced by the
//! real event type once that path is accepted.

use grapt_provider::{AttemptFailure, AttemptFragment, BoundedDetail, FinishReason, StragglerKind};
use grapt_types::{PhysicalAttemptId, StepTerminalReason, ToolCallId, ToolTerminalOutcome};

/// A fact the owner is asked to make durable, in owner order.
///
/// PROVISIONAL SHAPE — not a durable schema. Named facts only, so the
/// supervisor's ordering guarantees can be asserted without inventing a
/// serialization the store write path has not ratified.
///
/// Not `Eq`: it carries an `AttemptFragment`, which is only `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub enum DurableFact {
    /// The attempt opened its provider stream.
    AttemptStarted { attempt: PhysicalAttemptId },
    /// A normalized fragment was accepted and applied.
    Fragment { fragment: Box<AttemptFragment> },
    /// A parent-owned call, captured separately from provider/model content.
    NestedToolCalled {
        parent: ToolCallId,
        call: ToolCallId,
        tool: grapt_types::ToolDefinitionId,
        catalog: grapt_types::ToolSchemaVersion,
        input: crate::tool_protocol::BoundedToolInput,
    },
    /// A tool reached exactly one terminal state.
    ToolTerminal {
        call: ToolCallId,
        outcome: ToolOutcome,
    },
    /// The terminal of ONE physical attempt, always the last fact that attempt
    /// writes and always after its tool terminals.
    AttemptEnded { finish: FinishReason },
    /// The terminal of one physical attempt, failure form.
    ///
    /// Carries an ATTEMPT-level reason. Before R2A this shared one struct with
    /// [`DurableFact::StepFailed`], which made the two levels indistinguishable
    /// to the compiler even though `AttemptTerminalReason` and
    /// `StepTerminalReason` are deliberately different sets.
    AttemptFailed { failure: AttemptFailure },
    /// The one terminal of the whole logical Step. Exactly one of these exists
    /// per Step no matter how many attempts it took — the contract's "A Step
    /// has one and only one durable terminal fact". Written by the Step
    /// driver, never by an attempt.
    StepEnded { finish: FinishReason },
    /// The one Step terminal, failure form. Carries a STEP-level reason,
    /// which includes the policy and budget outcomes no single attempt can
    /// observe.
    StepFailed { failure: StepFailure },
}

/// The one terminal of a logical Step, with its typed reason.
///
/// # Why this is a different type from `AttemptFailure`
///
/// A Step may span several attempts, so its reasons include outcomes an
/// individual attempt cannot observe: a policy bound firing across attempts, a
/// budget exhausting, a compaction failing between attempts. Sharing one struct
/// with the attempt level — which is what the code did before R2A — let an
/// attempt-level reason be written into a Step terminal and vice versa, with no
/// compiler objection. The two are now distinct types, and `compile_fail`
/// probe `step_reason_is_not_an_attempt_reason` proves the substitution does
/// not compile.
///
/// Fields are private and construction is checked: no caller can build one from
/// a literal or attach an unbounded diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailure {
    reason: StepTerminalReason,
    detail: BoundedDetail,
    diagnostic: Option<grapt_types::failure_diagnostic::StepFailureDiagnostic>,
}

impl StepFailure {
    /// The only constructor.
    pub fn new(reason: StepTerminalReason, detail: impl Into<BoundedDetail>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            diagnostic: None,
        }
    }

    pub(crate) fn with_usage_diagnostic(
        mut self,
        diagnostic: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
    ) -> Self {
        self.diagnostic =
            diagnostic.map(grapt_types::failure_diagnostic::StepFailureDiagnostic::Usage);
        self
    }
    pub fn diagnostic(&self) -> Option<grapt_types::failure_diagnostic::StepFailureDiagnostic> {
        self.diagnostic
    }

    pub(crate) fn with_admission_refusal(
        mut self,
        diagnostic: grapt_types::failure_diagnostic::StepFailureDiagnostic,
    ) -> Self {
        self.diagnostic = Some(diagnostic);
        self
    }

    pub(crate) fn set_retry_diagnostic(
        &mut self,
        diagnostic: grapt_types::failure_diagnostic::RetryExhaustion,
    ) {
        self.diagnostic = Some(
            grapt_types::failure_diagnostic::StepFailureDiagnostic::RetryExhausted(diagnostic),
        );
    }

    pub(crate) fn from_attempt(reason: StepTerminalReason, failure: &AttemptFailure) -> Self {
        let mut carried = Self::new(reason, failure.detail().clone())
            .with_usage_diagnostic(failure.usage_diagnostic());
        if matches!(
            carried.reason(),
            StepTerminalReason::ProviderFailure {
                class: grapt_types::ProviderFailureClass::Quota
            }
        ) {
            carried.diagnostic = Some(
                grapt_types::failure_diagnostic::StepFailureDiagnostic::ProviderLimit {
                    reset: failure.limit_reset(),
                },
            );
        }
        carried
    }

    pub fn reason(&self) -> &StepTerminalReason {
        &self.reason
    }

    /// Non-authoritative diagnosis. Nothing branches on this.
    pub fn detail(&self) -> &BoundedDetail {
        &self.detail
    }
}

/// Compatibility name for the shared closed durable terminal vocabulary.
pub type ToolOutcome = ToolTerminalOutcome;

/// Something recorded for diagnosis that is not a durable domain fact.
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// A late hosted result arrived after its call had settled. Never a second
    /// settlement — see `grapt_provider::IgnoredEvidence`.
    IgnoredStraggler {
        call: ToolCallId,
        kind: StragglerKind,
    },
    /// A fragment was still queued when the attempt was cancelled. Contract:
    /// "the supervisor drains or explicitly classifies queued fragments before
    /// completing settlement." This is the explicit classification.
    DiscardedOnCancel { fragment: Box<AttemptFragment> },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("owner refused a fact: {reason}")]
pub struct OwnerError {
    pub reason: String,
}

/// Reconciliation always occurs; only a new append earns a live notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPublication {
    Notify,
    SuppressReplay,
}

/// The synchronous durable owner.
///
/// Implementors run inside the owner loop's single task. They must not spawn,
/// await, or block: everything here is expected to be one SQLite transaction's
/// worth of synchronous work.
pub trait AttemptOwner {
    /// Capture a provider-hosted result through the same artifact owner as
    /// local output. A reference reaches ToolTerminal only after capture.
    fn capture_hosted_output(
        &mut self,
        _text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        Err(OwnerError {
            reason: "attempt owner has no hosted output capture".into(),
        })
    }

    /// Capture a bounded, ordinary-sensitivity diagnostic note through the
    /// artifact owner so runtime-synthesized tool failures carry a readable
    /// named cause rather than a secret-bearing payload.
    fn capture_diagnostic_detail(
        &mut self,
        _text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        Err(OwnerError {
            reason: "attempt owner has no diagnostic detail capture".into(),
        })
    }

    fn timing_context(&self) -> Option<grapt_types::timing::TimingContext> {
        None
    }

    /// Enter the next logical Step when an application reuses this owner.
    /// The default is for owners with no Step-scoped state. Stateful owners
    /// must reset their message/capture state explicitly (DurableAttemptOwner).
    fn begin_step(&mut self, _step: grapt_types::StepNumber) -> Result<(), OwnerError> {
        Ok(())
    }

    /// Whether this attempt committed output a retry's request would replay,
    /// beyond what the stream validator counts as visible. The Step must not
    /// retry past it. The default declares an owner that never commits early.
    fn committed_output(&self) -> bool {
        false
    }

    /// Commit bounded pending content before the outer Step terminal writer.
    /// The default declares that this owner buffers no content; a buffering
    /// implementation must drain its owned bytes here before terminal commit.
    fn prepare_step_terminal(&mut self) -> Result<(), OwnerError> {
        Ok(())
    }

    /// Observe the outer writer's committed terminal without writing it again.
    fn step_terminal_committed(
        &mut self,
        fact: &DurableFact,
        _position: &grapt_session::CommittedPosition,
        publication: TerminalPublication,
    ) {
        // Stateless owners have no local settlement to reconcile. Stateful
        // owners must reconcile even when publication suppresses a replay.
        if publication == TerminalPublication::Notify {
            self.notify(fact);
        }
    }

    /// Publish the outer settlement writer's newly committed tool terminal.
    fn tool_terminal_committed(&mut self, fact: &DurableFact) {
        self.notify(fact);
    }

    /// Apply and commit one fact. Synchronous by signature, not by convention.
    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError>;

    /// Best-effort live notification of an already-committed fact. Called only
    /// after [`AttemptOwner::commit`] returned `Ok`. Must not fail the attempt.
    fn notify(&mut self, fact: &DurableFact);

    /// Record a non-durable observation.
    fn observe(&mut self, observation: Observation);
}

/// An owner that records everything, for witnesses.
///
/// Ordering is exactly what the supervisor produced, so a test can assert the
/// contract's "one Step terminal, after its owned tool terminal facts".
#[derive(Debug, Default)]
pub struct RecordingOwner {
    pub committed: Vec<DurableFact>,
    pub notified: Vec<DurableFact>,
    pub observations: Vec<Observation>,
    /// Model an owner that commits finished reasoning as it closes: once one
    /// closes in the current attempt, report committed output.
    pub commits_reasoning_early: bool,
}

impl RecordingOwner {
    pub fn new() -> Self {
        Self::default()
    }

    /// See [`Self::commits_reasoning_early`].
    pub fn committing_reasoning_early() -> Self {
        Self {
            commits_reasoning_early: true,
            ..Self::default()
        }
    }

    /// Index of the single attempt terminal, if one was committed.
    pub fn attempt_terminal_index(&self) -> Option<usize> {
        self.committed.iter().position(|fact| {
            matches!(
                fact,
                DurableFact::AttemptEnded { .. } | DurableFact::AttemptFailed { .. }
            )
        })
    }

    /// Attempt terminals committed. One per physical attempt.
    pub fn attempt_terminal_count(&self) -> usize {
        self.committed
            .iter()
            .filter(|fact| {
                matches!(
                    fact,
                    DurableFact::AttemptEnded { .. } | DurableFact::AttemptFailed { .. }
                )
            })
            .count()
    }

    /// Step terminals committed. Exactly one per logical Step, however many
    /// attempts it took.
    pub fn step_terminal_count(&self) -> usize {
        self.committed
            .iter()
            .filter(|fact| {
                matches!(
                    fact,
                    DurableFact::StepEnded { .. } | DurableFact::StepFailed { .. }
                )
            })
            .count()
    }

    pub fn tool_terminals(&self) -> Vec<(&ToolCallId, &ToolOutcome)> {
        self.committed
            .iter()
            .filter_map(|fact| match fact {
                DurableFact::ToolTerminal { call, outcome } => Some((call, outcome)),
                _ => None,
            })
            .collect()
    }
}

impl AttemptOwner for RecordingOwner {
    fn committed_output(&self) -> bool {
        self.commits_reasoning_early
            && self
                .committed
                .iter()
                .rev()
                .take_while(|fact| !matches!(fact, DurableFact::AttemptStarted { .. }))
                .any(|fact| {
                    matches!(
                        fact,
                        DurableFact::Fragment { fragment }
                            if matches!(fragment.as_ref(), AttemptFragment::ReasoningClosed { .. })
                    )
                })
    }

    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError> {
        self.committed.push(fact);
        Ok(())
    }

    fn notify(&mut self, fact: &DurableFact) {
        self.notified.push(fact.clone());
    }

    fn observe(&mut self, observation: Observation) {
        self.observations.push(observation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_owner_reports_terminal_position_and_count() {
        let mut owner = RecordingOwner::new();
        let call = match ToolCallId::new("call_1") {
            Some(call) => call,
            None => panic!("valid id"),
        };
        assert!(owner
            .commit(DurableFact::ToolTerminal {
                call,
                outcome: ToolOutcome::Completed { output: None },
            })
            .is_ok());
        assert_eq!(owner.attempt_terminal_count(), 0);
        assert!(owner
            .commit(DurableFact::AttemptEnded {
                finish: FinishReason::Stop
            })
            .is_ok());
        assert_eq!(owner.attempt_terminal_count(), 1);
        assert_eq!(owner.attempt_terminal_index(), Some(1));
        assert_eq!(owner.tool_terminals().len(), 1);
    }

    #[test]
    fn aborted_is_never_success() {
        assert!(!ToolOutcome::Cancelled {
            by: grapt_types::InterruptionSource::User,
        }
        .is_success());
        assert!(!ToolOutcome::ResultMissing.is_success());
        assert!(ToolOutcome::Completed { output: None }.is_success());
    }
}
