//! The Step driver: retry, compaction and timeout **policy** over the
//! accepted attempt supervisor.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Retry and compaction classification" and §"Terms and hard invariants".
//!
//! # Step vs PhysicalAttempt
//!
//! A **Step** is one logical model-call position in a Session drain. A
//! **PhysicalAttempt** is one actual provider stream. A retry is not a new
//! Step — it is a new attempt at the same Step, with a new identity. This
//! module owns that distinction and nothing else: it makes no network call,
//! writes no durable schema, and holds no transport.
//!
//! # The laws it enforces
//!
//! 1. One provider stream per PhysicalAttempt, opened only after that
//!    attempt's start fact is durable. [`AttemptSource::prepare`] returns a
//!    sealed [`AttemptPreparation`]; ordinary public composition can construct
//!    only its typed refusal, never a live reader or deferred factory.
//! 2. Retry only a retryable failure that occurred **before visible output**,
//!    within an explicit TOTAL attempt bound covering retries and rebuilds
//!    alike, honouring a provider `retry-after` when it exceeds the backoff.
//! 3. Never retry after visible output, and never after an interrupt.
//!    Cancellation is checked before minting an attempt and during backoff, so
//!    an interrupt never buys another stream nor waits out a pointless delay.
//! 4. A virgin context overflow **compacts, then reloads**, producing a new
//!    [`HistoryGeneration`] the rebuilt attempt observes. A second overflow is
//!    terminal, and a failed compaction or reload opens no further stream.
//! 5. A stream that commits nothing within the window is a typed no-progress
//!    timeout. Each successful commit resets the deadline, so the window
//!    measures silence since the last durable fact until the provider terminal.
//!    Owned tools then settle under their own execution bounds and cancellation.
//! 6. The admitted user input is never re-promoted. Retrying and rebuilding
//!    reuse the already-admitted input; only the reload refreshes
//!    model-visible content.
//!
//! # Attempt receipts
//!
//! [`AttemptReceipt`]s are operational facts for diagnosis, explicitly NOT
//! Session transcript facts.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use grapt_provider::{FinishReason, ProviderFailureClass};
use grapt_types::{
    ContextFailure, InterruptionSource, PhysicalAttemptId, PolicyVersion,
    ProviderFailureClass as TerminalClass, StepNumber, StepTerminalReason,
};
use tokio::sync::{watch, Notify};

use crate::attempt_admission::{
    AttemptAdmissionRefusal, AttemptPreparation, AttemptProtocol, PreparedAttempt,
};
use crate::owner::{AttemptOwner, DurableFact, Observation, OwnerError, StepFailure};
use crate::residency::{Cancellation, DrainResidency};
use crate::supervisor::{AttemptCancelCause, Diagnostics, ProviderReader, StepOutcome, ToolRunner};

mod admission;
mod backoff;
pub(crate) mod capacity;
mod classification;
mod compaction;
mod driver;
mod preparation;
mod status;
pub use status::{ContextPreparationStage, RetryNotice};

use classification::{carried_up, classify};
use compaction::compact_and_reload;

/// Rejected policy values. A policy permitting zero attempts or an unbounded
/// stall is not a lenient policy, it is a broken one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("max_attempts must be at least 1")]
    NoAttempts,
    #[error("max_attempts must not exceed the ruled total cap of {MAX_TOTAL_ATTEMPTS}")]
    TooManyAttempts { requested: u32 },
    #[error("max_overflow_rebuilds must not exceed 1; the contract permits one rebuild")]
    TooManyRebuilds { requested: u32 },
    #[error("no_progress_timeout must be non-zero")]
    ZeroTimeout,
    #[error("max_retry_delay must be non-zero")]
    ZeroMaxRetryDelay,
    #[error("a policy must carry a non-empty version identity")]
    UnnamedPolicy,
}

/// The ruled ceiling on physical attempts per Step: the oracle's one initial
/// call plus four retries (`retry.ts:47`). A policy may ask for fewer, never
/// more — an unbounded attempt count is the defect the cap exists to prevent.
pub const MAX_TOTAL_ATTEMPTS: u32 = 5;

/// Ruled upper bound on local backoff for non-rate-limit retries.
pub const RULED_MAX_RETRY_DELAY: Duration = Duration::from_secs(16);

/// Upper bound on honoring an explicit provider `Retry-After` delay on HTTP 429.
pub const RATE_LIMIT_MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Maximum extra rate-limit waits honored when the provider supplies `Retry-After`
/// before falling back to the ordinary attempt budget.
pub const MAX_RATE_LIMIT_WAITS: u32 = 12;

/// The version identity of this crate's own ruled default policy constants.
///
/// A `StepTerminalReason::PolicyLimit` names the policy whose bound fired, so
/// this string is provenance, not decoration: it changes whenever the ruled
/// constants below change, and a reader can tell one generation of defaults
/// from the next. Configured policies supply their own version instead.
pub const RULED_DEFAULT_POLICY_VERSION: &str = "secode-runtime/step-policy/ruled-default/2";

/// Closed retry/compaction/timeout policy.
///
/// Fields are private and construction is checked, so an invalid policy cannot
/// be written by mistake. Short durations stay constructible for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepPolicy {
    version: PolicyVersion,
    max_attempts: u32,
    base_backoff: Duration,
    max_retry_delay: Duration,
    max_overflow_rebuilds: u32,
    no_progress_timeout: Duration,
}

// There is deliberately NO `Default` impl.
//
// `StepTerminalReason::PolicyLimit` names the exact policy whose bound fired,
// so a policy must have an identity before it can stop a Step. A `Default`
// would have to invent one — and an invented constant identity is worse than
// no identity, because it looks like provenance. Every construction site now
// supplies a real version; the crate's own ruled defaults are named by
// [`StepPolicy::ruled_default_version`] and bump when the constants change.

impl StepPolicy {
    /// Checked construction. `max_attempts` is the TOTAL physical attempts a
    /// Step may make, counting the initial one, every retry and every rebuild.
    /// This crate's ruled default policy, carrying a NAMED version identity.
    ///
    /// The constants are the oracle's (one initial call plus four retries, a
    /// two-second base backoff, one overflow rebuild, a sixty-second
    /// no-progress budget). The version is owned by this crate and changes when
    /// those constants change, so a `PolicyLimit` carrying it points at an
    /// exact policy rather than at an invented constant identity.
    ///
    /// Fallible because [`PolicyVersion`] rejects the empty string and this
    /// crate denies `unwrap`/`expect`: the unreachable branch is REPRESENTED,
    /// not asserted away.
    pub fn ruled_default() -> Result<Self, PolicyError> {
        let version =
            PolicyVersion::new(RULED_DEFAULT_POLICY_VERSION).ok_or(PolicyError::UnnamedPolicy)?;
        Self::new_with_max_retry_delay(
            version,
            5,
            Duration::from_secs(2),
            RULED_MAX_RETRY_DELAY,
            1,
            Duration::from_secs(60),
        )
    }

    /// The version of the policy that produced this decision.
    pub fn version(&self) -> &PolicyVersion {
        &self.version
    }

    pub fn new(
        version: PolicyVersion,
        max_attempts: u32,
        base_backoff: Duration,
        max_overflow_rebuilds: u32,
        no_progress_timeout: Duration,
    ) -> Result<Self, PolicyError> {
        Self::new_with_max_retry_delay(
            version,
            max_attempts,
            base_backoff,
            RULED_MAX_RETRY_DELAY,
            max_overflow_rebuilds,
            no_progress_timeout,
        )
    }

    /// Checked construction with an explicit hostile-delay ceiling.
    pub fn new_with_max_retry_delay(
        version: PolicyVersion,
        max_attempts: u32,
        base_backoff: Duration,
        max_retry_delay: Duration,
        max_overflow_rebuilds: u32,
        no_progress_timeout: Duration,
    ) -> Result<Self, PolicyError> {
        if max_attempts == 0 {
            return Err(PolicyError::NoAttempts);
        }
        if max_attempts > MAX_TOTAL_ATTEMPTS {
            return Err(PolicyError::TooManyAttempts {
                requested: max_attempts,
            });
        }
        if max_overflow_rebuilds > 1 {
            return Err(PolicyError::TooManyRebuilds {
                requested: max_overflow_rebuilds,
            });
        }
        if no_progress_timeout.is_zero() {
            return Err(PolicyError::ZeroTimeout);
        }
        if max_retry_delay.is_zero() {
            return Err(PolicyError::ZeroMaxRetryDelay);
        }
        Ok(Self {
            version,
            max_attempts,
            base_backoff,
            max_retry_delay,
            max_overflow_rebuilds,
            no_progress_timeout,
        })
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }
    pub fn base_backoff(&self) -> Duration {
        self.base_backoff
    }
    pub fn max_retry_delay(&self) -> Duration {
        self.max_retry_delay
    }
    pub fn max_overflow_rebuilds(&self) -> u32 {
        self.max_overflow_rebuilds
    }
    pub fn no_progress_timeout(&self) -> Duration {
        self.no_progress_timeout
    }

    /// Backoff before the given retry number (1 = first retry).
    pub fn backoff_for(&self, retry_number: u32) -> Duration {
        let shift = retry_number.saturating_sub(1).min(16);
        self.base_backoff.saturating_mul(1u32 << shift)
    }

    /// The delay actually taken: the larger of computed backoff and any
    /// provider-declared minimum, capped by the explicit policy ceiling.
    ///
    /// Explicit `Retry-After` values on `RateLimit` are honored up to
    /// [`RATE_LIMIT_MAX_RETRY_DELAY`] when the policy uses at least the ruled
    /// default ceiling, so a 30s or 60s provider retry window is waited out
    /// instead of truncated to 16s.
    pub fn delay_for(&self, retry_number: u32, class: &ProviderFailureClass) -> Duration {
        let backoff = self.backoff_for(retry_number);
        let ceiling = self.retry_ceiling_for(class);
        let desired = match class.retry_after() {
            Some(after) if after > backoff => after,
            _ => backoff,
        };
        desired.min(ceiling)
    }

    pub(crate) fn retry_ceiling_for(&self, class: &ProviderFailureClass) -> Duration {
        match class {
            ProviderFailureClass::RateLimit {
                retry_after: Some(_),
            } if self.max_retry_delay >= RULED_MAX_RETRY_DELAY => {
                self.max_retry_delay.max(RATE_LIMIT_MAX_RETRY_DELAY)
            }
            _ => self.max_retry_delay,
        }
    }
}

/// Which generation of projected history an attempt was built from.
///
/// A compaction produces a new generation, and the rebuilt attempt must observe
/// it. That is what makes "rebuild" mean more than "try again".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HistoryGeneration(u64);

impl HistoryGeneration {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    pub fn value(self) -> u64 {
        self.0
    }
    /// Advance to the next generation, or `None` at the ceiling. Checked, not
    /// saturating: a generation that silently stopped advancing would make a
    /// rebuild look like it observed new history when it did not.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("history compaction failed: {reason}")]
pub struct CompactionError {
    pub reason: String,
}

/// Failure of the separate summary operation, preserving no-progress as a
/// distinct context outcome without requiring legacy sources to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionFailure {
    Failed(CompactionError),
    NoProgress {
        reason: String,
    },
    /// Every call since the last compaction repeated what it summarized.
    Repeated {
        reason: String,
    },
    /// The compaction committed, but the context it left is still at or above
    /// the proactive ceiling.
    DidNotFit {
        reason: String,
    },
}

impl CompactionFailure {
    fn into_terminal(self) -> (ContextFailure, String) {
        match self {
            Self::Failed(error) => (ContextFailure::CompactionFailed, error.reason),
            Self::NoProgress { reason } => (ContextFailure::CompactionNoProgress, reason),
            Self::Repeated { reason } => (ContextFailure::CompactionRepeated, reason),
            Self::DidNotFit { reason } => (ContextFailure::CompactionDidNotFit, reason),
        }
    }
}

/// The owning Step's coordinates and task custody for a separate summary call.
#[derive(Debug, Clone)]
pub struct CompactionContext {
    pub step: StepNumber,
    pub preceding_ordinal: u32,
    pub generation: HistoryGeneration,
    pub cancellation: Cancellation,
    pub trigger: grapt_types::CompactionTrigger,
    /// The Step policy's total physical attempts: a summary call is one, so
    /// none may take a receipt ordinal beyond it.
    pub max_attempts: u32,
}

/// Compacts and reloads the projected history an attempt is built from.
///
/// The driver calls [`HistorySource::compact`] and then
/// [`HistorySource::reload`], in that order. Compaction is a real operation —
/// the contract calls it "a separate operation and provider call" — and the
/// reload is what makes its result visible to the next attempt.
pub trait HistorySource {
    /// Rebind read-only context after a promoted human input joins recovery.
    fn promote_input(&mut self, _input: &grapt_types::MessageId) -> Result<(), String> {
        Ok(())
    }

    /// Compact the context. This is its own provider call in production, so it
    /// is async: performing it synchronously would block the runtime that every
    /// other attempt task shares.
    fn compact(&mut self) -> impl std::future::Future<Output = Result<(), CompactionError>> + Send;

    /// Run compaction under the owning cancellation signal. Production overrides
    /// this to interrupt and join its supervised provider operation before returning.
    /// Legacy sources retain the single-future cancellation behavior.
    fn compact_with(
        &mut self,
        context: CompactionContext,
    ) -> impl std::future::Future<Output = Result<Result<(), CompactionFailure>, InterruptionSource>>
           + Send {
        let compacting = self.compact();
        async move {
            let mut compacting = std::pin::pin!(compacting);
            let mut cancel = context.cancellation.signal();
            loop {
                if let Some(by) = *cancel.borrow() {
                    return Err(by);
                }
                tokio::select! {
                    result = &mut compacting => return Ok(result.map_err(CompactionFailure::Failed)),
                    changed = cancel.changed() => if changed.is_err() {
                        return Ok(compacting.await.map_err(CompactionFailure::Failed));
                    },
                }
            }
        }
    }

    /// Reload projected history, yielding the generation the rebuilt attempt
    /// must be built from.
    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError>;

    /// The generation currently in force.
    fn generation(&self) -> HistoryGeneration;
}

/// A history source for Steps that cannot compact. Compacting is an error
/// rather than a silent no-op, so an overflow cannot appear to be handled.
#[derive(Debug, Default)]
pub struct NoCompaction;

impl HistorySource for NoCompaction {
    async fn compact(&mut self) -> Result<(), CompactionError> {
        Err(CompactionError {
            reason: "this Step has no compactor".to_owned(),
        })
    }
    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
        Err(CompactionError {
            reason: "this Step has no compactor".to_owned(),
        })
    }
    fn generation(&self) -> HistoryGeneration {
        HistoryGeneration::new(0)
    }
}

/// The one durable Step terminal's coordinates, handed to the attempt source
/// before its first prepare of that Step.
///
/// # Why the source must know the terminal before any attempt exists
///
/// The durable attempt receipt persists, AT RESERVE, the exact terminal event
/// its settlement will read back — that is the store's own contract, so a
/// crashed process can prove or reproduce the terminal without having seen it.
/// The terminal's identity is minted once per logical Step by the settlement
/// context; a source that minted its own would reserve receipts naming an
/// event no settlement will ever write, and `settle_attempt_ordinary_terminal`
/// would refuse every one of them. So the application hands the SAME
/// coordinates to both consumers, and this is the carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepTerminalHandoff {
    step: StepNumber,
    event: grapt_types::EventId,
    created_ms: i64,
}

impl StepTerminalHandoff {
    pub fn new(step: StepNumber, event: grapt_types::EventId, created_ms: i64) -> Self {
        Self {
            step,
            event,
            created_ms,
        }
    }

    pub fn step(&self) -> StepNumber {
        self.step
    }

    pub fn event(&self) -> &grapt_types::EventId {
        &self.event
    }

    pub fn created_ms(&self) -> i64 {
        self.created_ms
    }
}

/// A deferred, one-shot provider stream opener.
///
/// The driver never holds a live reader: it holds this, and the supervisor
/// invokes it after the attempt-start fact commits.
/// Mints attempt identities and prepares — but does not open — their streams.
pub trait AttemptSource {
    type Reader: ProviderReader;

    /// Read-only notification around the driver's own backoff; no I/O policy.
    fn retrying(&mut self, _notice: Option<RetryNotice>) {}

    /// Stable session/source coordinate used to salt pre-admission retry jitter
    /// without depending on allocation layout or ambient randomness.
    fn jitter_coordinate(&self) -> Option<&str> {
        None
    }

    /// Arm or clear an output-ceiling continuation cue. Sources opt in only
    /// when the next Step can retain the partial answer in its real history.
    /// This changes request context, never admits a provider call itself.
    /// Rebind read-only context after a promoted human input joins recovery.
    fn promote_input(&mut self, _input: &grapt_types::MessageId) -> Result<(), String> {
        Ok(())
    }

    /// Retain partial output and cue the next ordinary Step, if supported.
    /// The runtime's chain policy supplies the bound; this never opens I/O.
    fn output_continuation(&mut self, _enabled: bool) -> bool {
        false
    }

    /// Mint the next attempt identity and return a factory that opens its
    /// stream later. `generation` is the history generation the attempt must be
    /// built from; a rebuilt attempt receives the post-compaction value.
    ///
    /// This must NOT perform the request. Opening here would issue provider I/O
    /// before the attempt exists durably.
    fn prepare(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader>;

    /// Wait for the provider transport to have room for the next attempt and
    /// hold it for [`Self::prepare`]. The driver bounds and cancels the wait;
    /// a source with no shared capacity has nothing to wait for.
    fn reserve_capacity(&mut self) -> crate::BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// A bounded latest-value signal for real progress on the shared adapter.
    fn capacity_progress(&self) -> Option<watch::Receiver<()>> {
        None
    }

    /// Latest active stage, published only after completed context work. Each
    /// publication resets inactivity; arbitrary wakeups must not publish progress.
    fn context_preparation_progress(&self) -> Option<watch::Receiver<ContextPreparationStage>> {
        None
    }

    /// Bounded asynchronous context preparation before minting or reserving a
    /// provider attempt. The driver supervises cancellation and its deadline.
    fn prepare_context(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> crate::BoxFuture<'_, AttemptPreparation<Self::Reader>> {
        let prepared = self.prepare(step, ordinal, generation);
        Box::pin(async move { prepared })
    }

    /// Discard a request that never opened when this logical Step terminates.
    fn discard_preparation(&mut self) {}

    /// Explicitly close context resources when the owning chain terminates.
    fn shutdown(&mut self) -> crate::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    /// Join interrupted preparation cleanup after its future has been dropped.
    fn finish_context(&mut self, _cancelled: bool) -> crate::BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    /// Receive the Step terminal coordinates the settlement context minted for
    /// the Step about to run. Defaulted to a no-op: a source that reserves no
    /// durable receipts has nothing to bind. The production provider source
    /// overrides it and FAILS CLOSED in `prepare` when the handoff is missing
    /// or names a different Step — a defaulted call that never happened refuses
    /// rather than reserving against invented coordinates.
    fn begin_step(&mut self, terminal: StepTerminalHandoff) {
        let _ = terminal;
    }

    /// The receipt-settlement hook the settlement owner joins at the terminal
    /// commit, if this source reserves durable attempt receipts. Defaulted to
    /// `None` for sources that do not.
    fn receipt_settlement(&self) -> Option<crate::provider_source::ProviderReceiptSettlement> {
        None
    }
}

/// Why the driver moved on from one attempt. Closed set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptDecision {
    Settled,
    Retry {
        after: Duration,
    },
    Rebuild,
    /// The driver stopped, carrying the STEP-level reason it will make
    /// durable. Not an attempt reason: the two sets differ, and the compiler
    /// now refuses the substitution.
    Stop {
        reason: StepTerminalReason,
    },
}

/// An operational record of one attempt. NOT a Session transcript fact.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptReceipt {
    pub attempt: PhysicalAttemptId,
    pub step: StepNumber,
    /// 1-based position within this Step, counting retries and rebuilds alike.
    pub ordinal: u32,
    /// The history generation this attempt was built from.
    pub generation: HistoryGeneration,
    pub outcome: StepOutcome,
    pub decision: AttemptDecision,
    pub diagnostics: Diagnostics,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepResult {
    Ended {
        finish: FinishReason,
    },
    Failed {
        failure: StepFailure,
    },
    OwnerRejected {
        error: OwnerError,
    },
    AdmissionRefused {
        refusal: AttemptAdmissionRefusal,
        failure: StepFailure,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct StepReport {
    pub step: StepNumber,
    pub result: StepResult,
    pub receipts: Vec<AttemptReceipt>,
    pub history_reloads: u32,
    pub compactions: u32,
}

/// Interrupts a whole Step, whichever attempt is currently in flight.
#[derive(Debug, Clone)]
pub struct StepInterruptHandle {
    cancel: watch::Sender<Option<InterruptionSource>>,
}

impl StepInterruptHandle {
    /// Interrupt the Step. This seam is the standalone equivalent of the
    /// coordinator's session interrupt, so it writes the same source at the
    /// same place: the writer, never a reader.
    pub fn interrupt(&self) {
        let _ = self.cancel.send(Some(InterruptionSource::User));
    }
}

/// Create the interrupt handle and the [`Cancellation`] [`StepDriver::run`]
/// consumes.
///
/// The residency it carries has no coordinator behind it: this seam drives a
/// Step standalone, so nothing observes when the Step's tasks end. That is the
/// right shape here — a caller with no Session to protect has nothing to wait
/// for — and it is why residency is proved where it is relied upon, in
/// [`crate::residency`] and in the R3-d coordinator suite, rather than here.
pub fn step_interrupt() -> (StepInterruptHandle, Cancellation) {
    let (cancel, rx) = watch::channel(None);
    (
        StepInterruptHandle { cancel },
        Cancellation::new(rx, DrainResidency::new().token()),
    )
}

/// Wraps the caller's owner and signals AFTER each successful commit, so the
/// stall watchdog measures silence since the last durable fact rather than
/// since the loop happened to sample.
pub(crate) struct ProgressOwner<O> {
    pub(crate) inner: O,
    pub(crate) progress: Arc<Notify>,
    pub(crate) commits: Arc<AtomicU64>,
    pub(crate) provider_finished: Arc<AtomicBool>,
}

impl<O: AttemptOwner> AttemptOwner for ProgressOwner<O> {
    fn committed_output(&self) -> bool {
        self.inner.committed_output()
    }

    fn capture_hosted_output(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        self.inner.capture_hosted_output(text)
    }

    fn capture_diagnostic_detail(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        self.inner.capture_diagnostic_detail(text)
    }

    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError> {
        let provider_finished = matches!(
            &fact,
            DurableFact::Fragment { fragment }
                if matches!(fragment.as_ref(),
                    grapt_provider::AttemptFragment::ProviderTerminal { .. }
                    | grapt_provider::AttemptFragment::ProviderFailure { .. })
        );
        // Signal only on success: a refused commit is not progress.
        self.inner.commit(fact)?;
        if provider_finished {
            self.provider_finished.store(true, Ordering::Relaxed);
        }
        self.commits.fetch_add(1, Ordering::Relaxed);
        self.progress.notify_one();
        Ok(())
    }

    fn notify(&mut self, fact: &DurableFact) {
        self.inner.notify(fact)
    }

    fn observe(&mut self, observation: Observation) {
        self.inner.observe(observation)
    }
}

/// Drives one logical Step to exactly one result.
#[derive(Debug)]
pub struct StepDriver;

#[cfg(test)]
mod tests;
