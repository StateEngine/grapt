//! Serialized attempt owner and supervisor — the first `grapt-runtime`
//! vertical.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Bounded handoff and backpressure", §"Attempt lifecycle, tools, and
//! durable order", §"Cancellation ownership". Budgets ruled in the
//! provider-stream review of 2026-08-08 and measured over the frozen clone.
//!
//! # What this crate owns
//!
//! One physical attempt's runtime: a bounded provider handoff, a prioritised
//! tool-terminal path, one cancellation owner over the reader and every owned
//! tool, and the guarantee of exactly one truthful Step terminal written after
//! all tool terminals.
//!
//! # What it deliberately does not own
//!
//! - **Transport.** [`ProviderReader`] is injected. `grapt-provider`'s
//!   no-transport boundary is preserved: nothing here opens a socket or parses
//!   vendor bytes.
//! - **Durable schema.** [`DurableFact`] is a provisional shape sufficient to
//!   prove ordering. The store crate owns the write path; this crate is
//!   expected to adopt it rather than invent one.
//! - **Retry policy.** Eligibility is `grapt_provider::AttemptDisposition`;
//!   the five-attempt cap is `StepPolicy::max_attempts` in attempt admission.
//!
//! # No unbounded channels
//!
//! Every queue here is bounded by a named budget in [`budgets`]. The
//! architecture test in `tests/no_unbounded_channels.rs` fails the build if an
//! unbounded channel is ever introduced.

#[cfg(test)]
use grapt_store::EventStore;
#[cfg(test)]
#[path = "../../../test-support/commit_fault.rs"]
mod commit_fault;

pub mod application;
pub mod attempt_admission;
mod attempt_planning;
pub mod budgets;
pub mod coordinator;
pub mod durable_owner;
mod failure_diagnostic;
pub mod mailbox;
pub mod mailbox_advisory;
pub mod nested_reconcile;
pub mod owner;
mod physical_attempt_owner;
pub mod provider_source;
pub mod recovery;
pub mod residency;
pub mod session_ports;
mod settlement;
pub mod step;
pub mod supervisor;
pub mod tool_protocol;

pub use application::{
    Clock, EventIds, GovernedLocationRuntime, GovernedLocationServiceMap, HistoryCursor,
    LocationError, LocationRuntime, LocationServiceMap, RuntimeApplication, RuntimeDrain,
    StepAllowance, StepChain,
};
pub use attempt_admission::{
    AttemptAdmissionRefusal, AttemptPreparation, ReservedAttemptAdmission,
};
pub use budgets::{
    ATTEMPT_TOOL_BUDGET, ATTEMPT_TOOL_CALL_LIMIT, PROVIDER_CHANNEL_CAPACITY,
    TOOL_ARTIFACT_BUFFER_BYTES, TOOL_ARTIFACT_MAX_BYTES, TOOL_CHANNEL_CAPACITY,
    TOOL_INPUT_MAX_BYTES, TOOL_PROGRESS_CAPACITY, TOOL_PROGRESS_MAX_BYTES, TOOL_TERMINAL_CAPACITY,
};
pub use coordinator::{
    Boundary, CoordinatorError, InterruptOutcome, Placement, Promotion, RunOutcome,
    SessionApplication, SessionCoordinator, WakeOutcome,
};
pub use durable_owner::{
    CommittedFactSink, DurableAttemptOwner, DurableOwnerError, NoLiveSubscribers, ResumedMessage,
    BLOCK_CAPTURE_BUFFER_BYTES, MAX_BLOCK_CAPTURE_BYTES, MAX_TOOL_INPUT_CAPTURE_BYTES,
};
pub use mailbox::{
    drain_mailbox_page, drain_mailbox_page_with_advisories, MailboxDrainError, MailboxDrainPage,
    MailboxMessageHandler,
};
pub use mailbox_advisory::{
    BoundedMailboxAdvisorySink, MailboxAdvisory, MailboxAdvisoryFault, MailboxAdvisoryOffer,
    MailboxAdvisoryPhase, MailboxAdvisorySink, MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES,
    MAX_HANDLED_MAILBOX_ADVISORY_BYTES, MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES,
    MAX_RETAINED_MAILBOX_ADVISORIES, MAX_RETAINED_MAILBOX_ADVISORY_BYTES,
};
pub use nested_reconcile::{reconcile_orphaned_nested, MAX_ORPHANED_NESTED};
pub use owner::{
    AttemptOwner, DurableFact, Observation, OwnerError, RecordingOwner, StepFailure, ToolOutcome,
};
pub use provider_source::{
    AttemptCustodyLedger, AttemptPlan, PlanRefusal, PlannedAttempt, ProviderAttemptSource,
    ProviderCompactionCall, ProviderErrorObserver, ProviderReceiptSettlement, ProviderStreamReader,
    SummaryRefusal, MAX_SUMMARY_CALLS,
};
pub use recovery::{
    recover_process_loss_attempts, ProcessLossRecoveryError, ProcessLossRecoveryReport,
    RecoveryWarning, MAX_RECOVERY_BYTES, MAX_RECOVERY_ROWS, MAX_RECOVERY_SESSIONS,
};
pub use residency::{Cancellation, DrainResidency, Residency};
pub use session_ports::{PromptAdmission, SessionPortError, SessionPorts};
pub use step::{
    step_interrupt, AttemptDecision, AttemptReceipt, AttemptSource, CompactionContext,
    CompactionError, CompactionFailure, ContextPreparationStage, HistoryGeneration, HistorySource,
    NoCompaction, PolicyError, StepDriver, StepInterruptHandle, StepPolicy, StepReport, StepResult,
    StepTerminalHandoff, MAX_TOTAL_ATTEMPTS, RULED_DEFAULT_POLICY_VERSION, RULED_MAX_RETRY_DELAY,
};
pub use supervisor::{
    AttemptCancelCause, AttemptReport, AttemptSupervisor, BoxFuture, Diagnostics, InterruptHandle,
    NestedCallError, NestedTools, ProviderReader, StepOutcome, ToolRunner,
};
pub use tool_protocol::{
    BoundedArtifactWriter, BoundedProgressSink, BoundedToolInput, CapturedToolArtifact,
    GovernedToolExecutor, GovernedToolRunner, ProgressDiagnostics, ProgressOffer, ToolCancellation,
    ToolExecutionTerminal, ToolInputTooLarge, ToolProgressDrain, ToolProgressTooLarge,
};
