//! The attempt planner port and typed pre-provider refusal semantics.
use crate::step::HistoryGeneration;
use grapt_provider::{request::ProviderRequest, PhysicalAttemptAuthoritySnapshot};
use grapt_types::{PhysicalAttemptId, StepNumber};

/// One attempt's model-facing inputs: the authority snapshot carrying the
/// registry-issued target and the request built from the current history
/// generation. A fresh pair per attempt: the binding consumes the issuance.
#[allow(
    missing_debug_implementations,
    reason = "it carries a model-visible request; a derived formatter would print it"
)]
pub struct PlannedAttempt {
    pub context: Option<grapt_types::RequestContextIdentity>,
    pub mail: Option<crate::mailbox::remote::NativeMailBatch>,
    pub operation_mail: Option<grapt_inbox::operation_mail::OperationMailBatch>,
    pub authority: PhysicalAttemptAuthoritySnapshot,
    /// The closed provider dispatch, not one route's request type.
    ///
    /// PROVIDER-1 widened this from `ChatGptCodexRequest`. A runtime pinned to
    /// one family could not plan an attempt for the three the provider crate
    /// now serves, and the opener already refuses a request whose family is not
    /// the one it was configured for — so the narrowing this field used to
    /// perform was never the thing keeping the pairing honest.
    pub request: ProviderRequest,
}

/// Closed operational cause, independent of the human-readable detail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PlanRefusalCause {
    #[default]
    Terminal,
    CapacityBusy,
}

/// Why a plan could not be produced, retaining transient local contention.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct PlanRefusal {
    pub cause: PlanRefusalCause,
    pub detail: String,
    /// A recoverable history refusal that must enter the ordinary compactor.
    pub compaction: Option<grapt_types::CompactionTrigger>,
}

impl PlanRefusal {
    pub fn new(detail: String, compaction: Option<grapt_types::CompactionTrigger>) -> Self {
        Self {
            cause: PlanRefusalCause::Terminal,
            detail,
            compaction,
        }
    }

    pub fn capacity_busy() -> Self {
        Self {
            cause: PlanRefusalCause::CapacityBusy,
            detail: "waiting for capacity".into(),
            compaction: None,
        }
    }
}

impl From<String> for PlanRefusal {
    fn from(detail: String) -> Self {
        Self::new(detail, None)
    }
}

/// A transient reader wait or a genuine failure of context admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ContextAdmissionRefusal {
    #[error("waiting for capacity")]
    CapacityBusy,
    #[error("context admission failed: {0:?}")]
    Context(grapt_types::ContextFailure),
}

impl From<grapt_types::ContextFailure> for ContextAdmissionRefusal {
    fn from(failure: grapt_types::ContextFailure) -> Self {
        Self::Context(failure)
    }
}

impl From<String> for ContextAdmissionRefusal {
    fn from(_: String) -> Self {
        Self::Context(grapt_types::ContextFailure::CompactionFailed)
    }
}

/// A context refusal and its optional payload-free continuity diagnosis.
/// Recovery guidance never changes the canonical context failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPreparationFailure {
    Context(grapt_types::ContextFailure),
    Continuity(grapt_session::execution::ContinuityFailure),
}

impl From<grapt_types::ContextFailure> for ContextPreparationFailure {
    fn from(failure: grapt_types::ContextFailure) -> Self {
        Self::Context(failure)
    }
}

/// Supplies each attempt's authority and request. `generation` is the history
/// generation the attempt must be built from: a rebuilt attempt receives the
/// post-compaction value and must plan over the reloaded history, never the one
/// that overflowed.
pub trait AttemptPlan: Send {
    /// Read-only projection of the Step driver's backoff, never retry policy.
    fn retrying(&mut self, _notice: Option<crate::step::RetryNotice>) {}

    /// Rebind read-only context after a promoted human input joins recovery.
    fn promote_input(&mut self, _input: &grapt_types::MessageId) -> Result<(), String> {
        Ok(())
    }

    /// Retain partial output and cue the next ordinary Step, if supported.
    /// The runtime's chain policy supplies the bound; this never opens I/O.
    fn output_continuation(&mut self, _enabled: bool) -> bool {
        false
    }

    /// Bind a disposable accounting projection to the exact reserved attempt.
    fn admitted(&mut self, _attempt: &PhysicalAttemptId) {}

    /// Assess the complete request before minting or reserving its attempt.
    /// A threshold decision grants no provider or campaign authority.
    fn context_admission(
        &mut self,
        _planned: &PlannedAttempt,
    ) -> Result<Option<grapt_types::CompactionTrigger>, ContextAdmissionRefusal> {
        Ok(None)
    }

    /// Payload-free completed-work signals for the preparation inactivity bound.
    /// The value identifies the active stage after that work has completed.
    fn context_preparation_progress(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<crate::step::ContextPreparationStage>> {
        None
    }

    /// Refresh external context through the product-owned boundary before the
    /// synchronous request plan. No provider receipt or identity exists yet.
    fn refresh_context(
        &mut self,
        _step: StepNumber,
        _generation: HistoryGeneration,
    ) -> crate::BoxFuture<'_, Result<(), ContextPreparationFailure>> {
        Box::pin(async { Ok(()) })
    }

    /// Close the chain's context resources before the owning drain drops it.
    fn shutdown(&mut self) -> crate::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    /// Runs after preparation completes or is cancelled. The Step driver bounds
    /// cleanup and lets a live owner retain scan progress on ordinary completion.
    fn finish_context(&mut self, _cancelled: bool) -> crate::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn plan(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal>;
}

pub(crate) mod capacity;
