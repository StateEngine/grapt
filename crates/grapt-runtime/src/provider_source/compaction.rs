//! One separately identified summary call within the owning Step's custody.

use super::*;
use crate::attempt_admission::AttemptProtocol;
use crate::owner::{AttemptOwner, ToolOutcome};
use crate::step::CompactionContext;
use crate::supervisor::{AttemptCancelCause, AttemptReport, BoxFuture, ToolRunner};

#[derive(Debug, Default)]
pub(super) struct CompactionBinding {
    pub terminal: Option<StepTerminalHandoff>,
    pub generation: Option<HistoryGeneration>,
    /// Calls this compaction has made: provider, SE fallback, and SE repair.
    pub calls: u32,
    /// The Session's latest committed descriptor when the first call began.
    /// A fallback is allowed only while it is unchanged: the first call
    /// committed nothing.
    pub first_head: Option<Option<u64>>,
    pub preceding_ordinal: Option<u32>,
    /// A summary call is admitted and has not returned. A fallback must
    /// follow a call that ENDED, so none is admitted while this holds.
    pub running: bool,
}

/// Holds [`CompactionBinding::running`] for one summary call. It clears only
/// when the call RETURNS: its attempt was joined (ended, interrupted or timed
/// out). A call dropped mid-flight leaves its attempt unjoined and its
/// receipt unsettled, so it never counts as ended: no fallback follows it in
/// this Step.
struct Running {
    binding: Arc<Mutex<CompactionBinding>>,
    ended: bool,
}

impl Running {
    fn ended(mut self) {
        self.ended = true;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.ended {
            lock(&self.binding).running = false;
        }
    }
}

/// The summary calls one compaction may make: the first, and up to three
/// retries with less of the oldest history. Each preceding call must have
/// ended without committing a descriptor. The calls are not charged against
/// the Step's coding attempt budget; this count is their own bound.
pub const MAX_SUMMARY_CALLS: u32 = 4;

/// Why a summary call was not run. Saturation is told apart because it passes:
/// the provider adapter stayed full for the bounded wait, which says nothing
/// against the compaction itself (SC-REL-13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRefusal {
    reason: String,
    saturated: bool,
}

impl SummaryRefusal {
    /// A summary call the adapter had no free attempt slot for.
    pub fn saturated(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            saturated: true,
        }
    }

    /// The adapter had no free attempt slot for the summary call.
    pub fn is_saturated(&self) -> bool {
        self.saturated
    }
}

impl core::fmt::Display for SummaryRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for SummaryRefusal {}

impl From<String> for SummaryRefusal {
    fn from(reason: String) -> Self {
        Self {
            reason,
            saturated: false,
        }
    }
}

impl From<&str> for SummaryRefusal {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}

impl From<SummaryRefusal> for String {
    fn from(refusal: SummaryRefusal) -> Self {
        refusal.reason
    }
}

impl PartialEq<&str> for SummaryRefusal {
    fn eq(&self, other: &&str) -> bool {
        self.reason == *other
    }
}

pub(super) fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    match value.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Issued by an ordinary provider source; shares its durable receipt settlement.
/// The summary has its own physical identity and ordinal, but cannot manufacture
/// another coding Step or settle before the owning Step's terminal commit.
pub struct ProviderCompactionCall {
    pub(super) opener: Arc<AttemptOpener>,
    pub(super) error_observer: Option<Arc<dyn ProviderErrorObserver>>,
    pub(super) capacity_observer: Option<Arc<dyn Fn(Option<usize>) + Send + Sync>>,
    pub(super) child_capacity: Option<std::num::NonZeroUsize>,
    pub(super) inbox: Arc<Mutex<Inbox>>,
    pub(super) ids: Arc<dyn IdAuthority + Send + Sync>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) session: SessionId,
    pub(super) ceiling: AttemptRecordCeiling,
    pub(super) ledger: AttemptCustodyLedger,
    pub(super) binding: Arc<Mutex<CompactionBinding>>,
}

impl core::fmt::Debug for ProviderCompactionCall {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProviderCompactionCall")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

struct SummaryPlan(Option<PlannedAttempt>);

impl AttemptPlan for SummaryPlan {
    fn plan(
        &mut self,
        _: StepNumber,
        _: u32,
        _: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        self.0
            .take()
            .ok_or_else(|| PlanRefusal::new("summary plan was already consumed".to_owned(), None))
    }
}

struct NoSummaryTools;

impl ToolRunner for NoSummaryTools {
    fn run(
        &self,
        _: grapt_types::ToolCallId,
        _: String,
        _: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        Box::pin(async {
            ToolOutcome::Failed {
                reason: grapt_types::ToolFailureReason::AuthorityUnavailable,
                detail: None,
            }
        })
    }
}

impl ProviderCompactionCall {
    /// Enter the same sealed admission/supervisor path as coding attempts.
    /// The operation owner must persist summary identity, content and accounting
    /// separately from ordinary assistant messages. No tool executor is exposed.
    pub async fn run<O: AttemptOwner>(
        &mut self,
        context: CompactionContext,
        planned: PlannedAttempt,
        owner: O,
    ) -> Result<(O, AttemptReport), SummaryRefusal> {
        if context.cancellation.signal().borrow().is_some() {
            return Err("summary call was interrupted before admission".into());
        }
        let terminal = {
            let mut binding = lock(&self.binding);
            let terminal = binding
                .terminal
                .as_ref()
                .filter(|terminal| terminal.step() == context.step)
                .ok_or("summary call has no matching owning Step")?
                .clone();
            if binding.running
                || binding.generation != Some(context.generation)
                || binding.preceding_ordinal != Some(context.preceding_ordinal)
            {
                return Err("summary call is repeated or names stale history".into());
            }
            let head = {
                let inbox = lock(&self.inbox);
                grapt_store::compaction::latest_descriptor(
                    inbox.store().connection(),
                    &self.session,
                )
                .map_err(|error| format!("summary call cannot read committed history: {error}"))?
                .map(|(_, seq, _)| seq.value())
            };
            match binding.calls {
                0 => binding.first_head = Some(head),
                // Fallback or repair: only while preceding calls committed nothing.
                calls if calls < MAX_SUMMARY_CALLS && binding.first_head == Some(head) => {}
                _ => return Err("summary call is repeated or names stale history".into()),
            }
            let ordinal = context
                .preceding_ordinal
                .checked_add(binding.calls + 1)
                .ok_or("summary call ordinal overflowed")?;
            binding.calls += 1;
            binding.running = true;
            (terminal, ordinal)
        };
        let (terminal, ordinal) = terminal;
        let running = Running {
            binding: self.binding.clone(),
            ended: false,
        };
        let result = self
            .run_admitted(context, terminal, ordinal, planned, owner)
            .await;
        running.ended();
        result
    }

    /// The admitted call: its attempt is prepared, run and JOINED here, so a
    /// return from this means the attempt has ended.
    async fn run_admitted<O: AttemptOwner>(
        &mut self,
        context: CompactionContext,
        terminal: StepTerminalHandoff,
        ordinal: u32,
        planned: PlannedAttempt,
        owner: O,
    ) -> Result<(O, AttemptReport), SummaryRefusal> {
        let mut cancel = context.cancellation.signal();
        let mut source = ProviderAttemptSource::new(
            self.opener.clone(),
            self.inbox.clone(),
            self.ids.clone(),
            self.clock.clone(),
            self.session.clone(),
            self.ceiling,
            SummaryPlan(Some(planned)),
        );
        source.error_observer = self.error_observer.clone();
        source.capacity_observer = self.capacity_observer.clone();
        source.child_capacity = self.child_capacity;
        source.terminal = Some(terminal);
        source.ledger = self.ledger.clone();
        let timeout = crate::StepPolicy::ruled_default()
            .map_err(|error| error.to_string())?
            .no_progress_timeout();
        // Wait, bounded, for the adapter's slot: whatever stream holds it, a
        // parent's or another child's, ends, and the summary is not refused
        // for having arrived while it ran (SC-REL-13).
        let interrupted = async {
            if cancel.wait_for(Option::is_some).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        let progress = source.capacity_progress();
        tokio::select! {
            _ = crate::step::capacity::until_stalled(source.reserve_capacity(), progress, timeout) => {}
            () = interrupted => {
                return Err("summary call was interrupted before admission".into());
            }
        }
        let preparation = source.prepare(context.step, ordinal, context.generation);
        let progress = Arc::new(tokio::sync::Notify::new());
        let progress_owner = crate::step::ProgressOwner {
            inner: owner,
            progress: progress.clone(),
            commits: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            provider_finished: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let prepared = AttemptProtocol::prepare(
            preparation,
            Arc::new(NoSummaryTools),
            progress_owner,
            context.cancellation.residency(),
            context.cancellation.signal(),
        )
        .map_err(|(_, refusal)| {
            let reason = format!("summary admission refused: {refusal:?}");
            match refusal {
                AttemptAdmissionRefusal::Saturated { .. } => SummaryRefusal::saturated(reason),
                _ => reason.into(),
            }
        })?;
        let interrupt = prepared.interrupt;
        let mut running = std::pin::pin!(prepared.run);
        let mut deadline = std::pin::pin!(tokio::time::sleep(timeout));
        let mut interrupt_available = true;
        loop {
            let interrupted = *cancel.borrow();
            if let Some(by) = interrupted {
                interrupt.interrupt(AttemptCancelCause::Interrupted { by });
                let (owner, report) = running.await;
                return Ok((owner.inner, report));
            }
            tokio::select! {
                (owner, report) = &mut running => return Ok((owner.inner, report)),
                changed = cancel.changed(), if interrupt_available => if changed.is_err() {
                    interrupt_available = false;
                },
                () = progress.notified() => {
                    deadline.as_mut().reset(tokio::time::Instant::now() + timeout);
                }
                () = &mut deadline => {
                    interrupt.interrupt(AttemptCancelCause::NoProgress);
                    let (owner, report) = running.await;
                    return Ok((owner.inner, report));
                }
            }
        }
    }
}
