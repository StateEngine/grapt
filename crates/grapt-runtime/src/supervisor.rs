//! The attempt supervisor and its serialized owner loop.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Bounded handoff and backpressure", §"Attempt lifecycle, tools, and
//! durable order", §"Cancellation ownership". Topology and budgets from
//! `PROVIDER_STREAM_TOPOLOGY_AND_BUDGETS_D2_2026-08-08.md`, ruled in the
//! provider-stream review.
//!
//! # Shape
//!
//! ```text
//!   provider reader ──(bounded 64, fragments)──┐
//!     │ one task, started only after the        │   biased select:
//!     │ attempt-start fact commits              ├──▶ owner loop ──▶ AttemptOwner
//!     ▼ cancellable directly                    │    (single task)  commit → notify
//!   tool tasks ────(bounded, PRIORITISED)───────┘                   (synchronous)
//!    ≤16 permits
//! ```
//!
//! # Why the tool channel is polled first
//!
//! Topology finding: "tool terminal facts must NOT be blocked behind a
//! full provider channel — the owner must drain tool-terminal messages even
//! while the provider channel is saturated, or interrupt-settlement
//! deadlocks."
//!
//! # Why cancellation reaches the reader directly
//!
//! Oracle red baseline (provider-interrupt-midstream, corrected expectation):
//! interrupting a **hung provider stream with no tool running** does not
//! settle promptly, because the oracle's proven settlement path runs through
//! running local tools. Here one cancellation signal is watched by the reader
//! task itself, so a hung read is abandoned whether or not any tool exists —
//! and it is watched through settlement too, so an interrupt arriving after a
//! clean provider terminal still produces a truthful aborted outcome.
//!
//! # What this module does not own
//!
//! Transport (the reader is built by an injected one-shot factory), the
//! durable schema (see [`crate::owner::DurableFact`]), and the retry decision
//! (that is `grapt_provider::AttemptDisposition` plus the `StepPolicy` cap in
//! attempt admission).

mod attempt_loop;
mod nested;
mod task_drivers;
pub use nested::{NestedCallError, NestedTools};

use attempt_loop::run_attempt;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use grapt_provider::{
    AttemptFailure, AttemptFragment, FailureClassification, FinishReason, NormalizedFinish,
    ProviderFailureClass, StreamValidator, ToolExecution,
};
use grapt_types::{
    AttemptTerminalReason, InterruptionSource, PhysicalAttemptId,
    ProviderFailureClass as TerminalClass, ToolCallId, ToolFailureReason,
};
use tokio::sync::{mpsc, watch, Semaphore};
use tokio::task::JoinSet;

use crate::budgets::{
    ATTEMPT_TOOL_BUDGET, ATTEMPT_TOOL_CALL_LIMIT, PROVIDER_CHANNEL_CAPACITY, TOOL_CHANNEL_CAPACITY,
};
use crate::owner::{AttemptOwner, DurableFact, Observation, OwnerError, ToolOutcome};
use crate::residency::Residency;
use crate::tool_protocol::BoundedToolInput;

/// A boxed future, so a trait method can be async without the trait losing
/// object safety. Avoids pulling in `futures` for one type alias.
///
/// The lifetime is explicit rather than elided to `'static`: a seam whose
/// future owns everything it needs uses `BoxFuture<'static, _>`, while a seam
/// that drives borrowed state — the drain context a Step chain lives in —
/// returns a future borrowing it. Erasing that distinction would force the
/// borrowed case to clone or re-resolve the very state its caller owns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The provider side of one attempt. Injected, never constructed here — this
/// crate owns no transport.
///
/// # Do not emit `AttemptOpened`
///
/// The supervisor opens the attempt: it mints the identity, commits the
/// attempt-start fact, and only then builds this reader. A reader yields
/// **content** fragments only. Emitting `AttemptOpened` means two things claim
/// to have opened one stream, and the attempt fails with `AlreadyOpened`.
pub trait ProviderReader: Send + 'static {
    /// Next normalized fragment, or `None` at end of stream.
    fn next(&mut self) -> impl Future<Output = Option<AttemptFragment>> + Send;
}

/// Runs one local tool call.
pub trait ToolRunner: Send + Sync + 'static {
    /// Reap process custody retained when a tool future was cancelled. False
    /// means unresolved effects; the product must quarantine their continuation.
    fn settle_attempt(&self) -> BoxFuture<'static, bool> {
        Box::pin(async { true })
    }
    /// Trusted composition metadata, never a declaration supplied by a guest.
    fn is_composition(&self, _tool: &str) -> bool {
        false
    }

    fn run_with_nested(
        &self,
        attempt: Option<PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
        _nested: Option<NestedTools>,
    ) -> BoxFuture<'static, ToolOutcome> {
        self.run_with_attempt(attempt, call, tool, input)
    }

    /// Same governed effect route, with machine output for its parent audience.
    fn run_nested(
        &self,
        attempt: Option<PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        self.run_with_attempt(attempt, call, tool, input)
    }
    /// Process-local custody of outstanding human decisions, issued by the
    /// tool permission owner. Provider fragments cannot pause the watchdog.
    fn decision_waits(&self) -> Option<watch::Receiver<u32>> {
        None
    }

    fn run(
        &self,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome>;

    /// The same run, told which ATTEMPT admitted the call.
    ///
    /// DEFAULTED, AND THAT IS THE POINT. The attempt is owned by the recording
    /// side — the durable owner holds "the attempt currently streaming" and
    /// looks it up when a terminal arrives — so it never travelled the dispatch
    /// path at all. A runner that has no use for it (every test double here)
    /// keeps working unchanged, while the production runner overrides this to
    /// carry the identity down to the MCP boundary.
    ///
    /// Adding a parameter to `run` instead would have edited ten test doubles to
    /// thread a value none of them observe, which is churn that obscures the one
    /// implementation the change is actually about.
    fn run_with_attempt(
        &self,
        _attempt: Option<PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        self.run(call, tool, input)
    }
}

/// Typed messages a tool task may send the owner.
///
/// Terminals only. Tool *progress* is named in the contract as transient and
/// bounded, but no progress message crosses this channel today — when one is
/// added, [`TOOL_CHANNEL_CAPACITY`] must be revisited so a permitted tool can
/// still always enqueue its terminal without blocking.
#[derive(Debug)]
enum ToolMessage {
    Terminal {
        call: ToolCallId,
        outcome: ToolOutcome,
    },
}

/// Measured facts about one attempt's run. Diagnostics, not durable domain
/// facts — the contract is explicit that these "are not durable domain facts
/// unless an accepted observability contract says otherwise".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Deepest the provider channel ever got.
    pub provider_high_water: usize,
    /// Times the reader found the channel **full** and had to wait. This, not
    /// `producer_wait`, is the proof that backpressure actually engaged.
    pub provider_contention_events: usize,
    /// Exact number of adjacent fragments absorbed into emitted batches.
    pub provider_fragments_coalesced: usize,
    /// Time spent blocked on a full channel. Measured only across a genuine
    /// contended wait, never across an uncontended fast-path reserve.
    pub producer_wait: Duration,
    /// Tools that were started.
    pub tools_started: usize,
    /// Greatest number of tools running at once. Must never exceed the budget.
    pub tools_peak_concurrent: usize,
    /// Times a tool had to wait for a permit.
    pub tool_permit_waits: usize,
    /// Fragments still queued when the attempt was **cancelled**, and therefore
    /// explicitly classified rather than silently dropped.
    pub discarded_on_cancel: usize,
    /// Fragments that arrived after an accepted provider terminal. Any value
    /// above zero is an invalid provider stream and fails the step.
    pub post_terminal_fragments: usize,
    /// Whether this attempt produced visible output. The Step driver needs it
    /// to enforce "never retry after visible output"; a retry there would
    /// re-issue work the user has already seen.
    pub visible_output: bool,
    pub cancelled: bool,
}

/// How the attempt ended.
///
/// `Failed` carries an ATTEMPT-level reason. The Step driver converts it into
/// a Step-level reason when it decides to stop; the two vocabularies are not
/// interchangeable and the compiler now says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Ended {
        finish: FinishReason,
    },
    Failed {
        failure: AttemptFailure,
    },
    /// The owner refused a fact. Distinct from a provider failure: nothing is
    /// wrong with the stream, the store would not accept it, and the attempt
    /// stopped rather than proceeding on unwritten state.
    OwnerRejected {
        error: OwnerError,
    },
}

/// Why the provider itself failed, in the vocabulary the Step driver needs to
/// decide what happens next. Absent when the attempt did not end in a provider
/// failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailureDetail {
    pub class: ProviderFailureClass,
    pub classification: Option<FailureClassification>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttemptReport {
    pub attempt: PhysicalAttemptId,
    pub outcome: StepOutcome,
    pub diagnostics: Diagnostics,
    /// Set only when the stream ended in a `ProviderFailure`.
    pub provider_failure: Option<ProviderFailureDetail>,
}

/// Why an attempt was cancelled.
///
/// The supervisor cannot infer this, and inferring it is exactly the defect
/// R2A removes: a Step cancellation carries a real source (a user pressed
/// stop), while the no-progress watchdog is not an interruption at all — it is
/// a provider failure. Before R2A both arrived as a bare `true` on a
/// `watch<bool>` and both settled as the string `aborted`, so a stalled stream
/// was indistinguishable from a user interrupt in the durable record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptCancelCause {
    /// The Step's cancellation reached this attempt, carrying its source.
    Interrupted { by: InterruptionSource },
    /// The runtime's no-progress watchdog fired. NEVER an interruption.
    NoProgress,
}

/// Handle used to interrupt a running attempt.
#[derive(Debug, Clone)]
pub struct InterruptHandle {
    cancel: watch::Sender<Option<AttemptCancelCause>>,
}

impl InterruptHandle {
    /// Request cancellation, naming the cause. Reaches the provider reader
    /// directly, so a hung read is abandoned whether or not a tool is running,
    /// and is observed through settlement so a late interrupt still settles
    /// truthfully.
    ///
    /// The cause is supplied by the caller that KNOWS it — the Step driver,
    /// which distinguishes its own cancel channel from its watchdog deadline —
    /// never guessed here.
    pub fn interrupt(&self, cause: AttemptCancelCause) {
        let _ = self.cancel.send(Some(cause));
    }
}

#[derive(Debug, Default)]
struct SharedDiagnostics {
    provider_high_water: AtomicUsize,
    provider_contention_events: AtomicUsize,
    provider_fragments_coalesced: AtomicUsize,
    producer_wait_nanos: AtomicU64,
    tools_peak_concurrent: AtomicUsize,
    tools_running: AtomicUsize,
    tool_permit_waits: AtomicUsize,
}

/// Runs one physical attempt to its single terminal fact.
#[derive(Debug)]
pub struct AttemptSupervisor;

impl AttemptSupervisor {
    /// Create the interrupt handle and the attempt future together.
    ///
    /// `make_reader` is a **one-shot factory**: the supervisor owns when the
    /// provider stream is opened, and opens it exactly once, only after the
    /// attempt-start fact has been durably committed. Handing in an
    /// already-constructed reader would mean the stream was opened before the
    /// attempt existed, which is what contract rule 1 forbids.
    ///
    /// The attempt is not spawned here: the caller drives it, so the owner
    /// stays on the caller's task and no store handle ever crosses a task
    /// boundary.
    /// `residency` is minted by the coordinator for this drain. Every task this
    /// attempt spawns holds a clone, so the coordinator can tell when they are
    /// destroyed rather than merely aborted — see [`crate::residency`].
    pub(crate) fn prepare<F, R, T, O>(
        attempt: PhysicalAttemptId,
        make_reader: F,
        tools: Arc<T>,
        owner: O,
        residency: Residency,
        step: watch::Receiver<Option<InterruptionSource>>,
    ) -> (InterruptHandle, impl Future<Output = (O, AttemptReport)>)
    where
        F: FnOnce() -> R + Send + 'static,
        R: ProviderReader,
        T: ToolRunner,
        O: AttemptOwner,
    {
        let (cancel_tx, cancel_rx) = watch::channel(None);
        let handle = InterruptHandle {
            cancel: cancel_tx.clone(),
        };
        let future = run_attempt(
            attempt,
            make_reader,
            tools,
            owner,
            cancel_tx,
            cancel_rx,
            residency,
            step,
        );
        (handle, future)
    }
}

/// A spawned task whose life is this frame's life.
///
/// `tokio::spawn` detaches: dropping a bare [`JoinHandle`] leaves the task
/// running. That is wrong for the provider reader. An owning wake can be
/// aborted, which drops [`run_attempt`]'s future mid-await, and a detached
/// reader would then go on streaming into an attempt nobody owns — while the
/// coordinator, whose ownership guard has already released the Session, admits
/// a replacement drain. Two readers on one Session is precisely what the
/// contract's §"Cancellation ownership" forbids.
///
/// Aborting in `Drop` makes the reader's lifetime the attempt's, on every exit
/// path including the one that never returns.
///
/// [`JoinHandle`]: tokio::task::JoinHandle
struct OwnedTask(tokio::task::JoinHandle<()>);

impl OwnedTask {
    /// Stop the task and wait for it to be gone.
    ///
    /// The ordinary exit. `Drop` covers the path where this is never reached;
    /// this one additionally *joins*, so a caller that returns after it knows
    /// the reader is finished rather than merely told to stop.
    async fn settle(mut self) {
        self.0.abort();
        // `JoinHandle` is `Unpin`, so it can be polled through the borrow and
        // the `Drop` below still runs (aborting a finished task is a no-op).
        let _ = (&mut self.0).await;
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Commit synchronously, then notify. Never the other way round, and never
/// past a refusal.
fn commit<O: AttemptOwner>(owner: &mut O, fact: DurableFact) -> Result<(), OwnerError> {
    owner.commit(fact.clone())?;
    owner.notify(&fact);
    Ok(())
}

#[cfg(test)]
mod tests;
