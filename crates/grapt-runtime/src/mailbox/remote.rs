//! One in-process owner of bounded remote-mail convergence. Product drives this
//! alongside its existing events; no task, second database or monitor is born.
pub mod credential;
mod delivery;
pub mod http;
mod io;
mod presentation;
mod progress;
mod snapshot;
pub use presentation::{render_notification, MailNotificationScan, NativeMailBatch};

use grapt_inbox::Inbox;
use grapt_mail_transport::operation::OperationFailure;
use grapt_mail_transport::{
    ArmedMailboxSubscription, ChangeSignal, ExclusiveSourceCursor, MailBinding, OpenRequest,
    ProviderLimits, RemoteSeatMailTransportV1Port, SafeReplayOrigin, ShutdownReceipt,
};
use grapt_store::{RemoteMailboxScope, WriteError};
use grapt_types::{InterruptionSource, MailboxAdmissionSeq, MailboxCursor};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::watch;

/// Closed local failures; source JSON and credentials never enter this owner.
#[derive(Debug, thiserror::Error)]
pub enum RemoteMailboxError {
    #[error("remote mailbox operation failed: {0}")]
    Port(#[source] OperationFailure),
    #[error("remote mailbox store failed")]
    Store(#[from] WriteError),
    #[error("remote mailbox shared store is unavailable")]
    StoreUnavailable,
    #[error("remote mailbox binding does not match the expected scope")]
    ForeignBinding,
    #[error("remote mailbox message or receipt does not match its binding")]
    ForeignReceipt,
    #[error("remote mailbox operation was cancelled")]
    Cancelled,
    #[error("remote mailbox operation exceeded its deadline")]
    Deadline,
    #[error("remote mailbox profile cannot fit one bounded page in a slice")]
    SliceTooSmall,
    #[error("remote mailbox is not live")]
    NotLive,
    #[error("remote mailbox replay origin does not cover the invalid cursor")]
    RecoveryUnavailable,
    #[error("remote mailbox source page did not advance")]
    CursorDidNotAdvance,
}

/// An exhausted slice is pending, even when it emitted no notifications.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RemoteMailboxSlice {
    pub pages: u32,
    pub rows: u32,
    /// Conservative charged upper bounds for source pages, actual encoded
    /// bytes for local recovery. This is a budget, not a wire measurement.
    pub charged_bytes: usize,
    pub rescan_notifications: bool,
    /// The oldest actionable row whose delivery this slice witnessed: rows
    /// below a reader's high-water mark that became presentable.
    pub delivered_from: Option<MailboxAdmissionSeq>,
    pub live: bool,
}

#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct RemoteMailboxRuntime<P> {
    port: P,
    inbox: Arc<Mutex<Inbox>>,
    expected: RemoteMailboxScope,
    request: OpenRequest,
    requested_limits: ProviderLimits,
    binding: Option<MailBinding>,
    subscription: Option<ArmedMailboxSubscription>,
    notification_after: MailboxCursor,
    notification_before: Option<MailboxAdmissionSeq>,
    /// Presentation reads only rows above this mark; see [`below`].
    notification_floor: MailboxCursor,
    /// The newest row of the presentation pass in progress.
    notification_top: Option<MailboxAdmissionSeq>,
    delivery: delivery::DeliveryProgress,
    interleaved_delivery: bool,
    recovery_after: MailboxCursor,
    recovery_done: bool,
    checkpoint_before: Option<MailboxAdmissionSeq>,
    checkpoint_candidate: Option<ExclusiveSourceCursor>,
    checkpoint_done: bool,
    source_after: Option<ExclusiveSourceCursor>,
    snapshot_done: bool,
    replay_origin: Option<SafeReplayOrigin>,
    progress_clock: Option<Arc<dyn grapt_types::ClockAuthority + Send + Sync>>,
    progress_after: Option<grapt_types::EventSeq>,
    progress_prepared: bool,
    integrity_gaps: u64,
    decode_failures: u64,
    /// Source positions of the first undecodable rows, for the diagnostic that
    /// explains the count. Bounded and never content: a cursor names a position
    /// in the source, not the mail.
    decode_failure_positions: Vec<ExclusiveSourceCursor>,
}

/// A mark that lets a reader see `seq` again: rows above a reader's mark are
/// re-read. An older row becomes presentable only through a witnessed or
/// recovered delivery or a wake re-report, and each of those lowers the marks.
pub fn below(seq: MailboxAdmissionSeq) -> MailboxCursor {
    MailboxAdmissionSeq::new(seq.value().saturating_sub(1))
        .map_or(MailboxCursor::START, MailboxCursor::after)
}

/// How many undecodable-row positions the diagnostic keeps.
const DECODE_FAILURE_POSITIONS: usize = 4;

impl<P: RemoteSeatMailTransportV1Port> RemoteMailboxRuntime<P> {
    pub fn new(
        port: P,
        inbox: Arc<Mutex<Inbox>>,
        expected: RemoteMailboxScope,
        request: OpenRequest,
        requested_limits: ProviderLimits,
    ) -> Result<Self, RemoteMailboxError> {
        if request.expected_recipient() != &expected.recipient
            || request.expected_local_project() != expected.projects.target()
        {
            return Err(RemoteMailboxError::ForeignBinding);
        }
        Ok(Self {
            port,
            inbox,
            expected,
            request,
            requested_limits,
            binding: None,
            subscription: None,
            notification_after: MailboxCursor::START,
            notification_before: None,
            notification_floor: MailboxCursor::START,
            notification_top: None,
            delivery: delivery::DeliveryProgress::default(),
            interleaved_delivery: false,
            recovery_after: MailboxCursor::START,
            recovery_done: false,
            checkpoint_before: None,
            checkpoint_candidate: None,
            checkpoint_done: false,
            source_after: None,
            snapshot_done: false,
            replay_origin: None,
            progress_clock: None,
            progress_after: None,
            progress_prepared: false,
            integrity_gaps: 0,
            decode_failures: 0,
            decode_failure_positions: Vec::new(),
        })
    }

    /// Explicit v2 configuration supplies the same injected attribution clock
    /// as the composition root. V1 construction and behavior stay unchanged.
    pub fn with_progress_clock(
        mut self,
        clock: Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
    ) -> Result<Self, RemoteMailboxError> {
        if self.request.requested_protocol().as_str()
            != grapt_mail_transport::REMOTE_SEAT_MAIL_TRANSPORT_V2
        {
            return Err(RemoteMailboxError::ForeignBinding);
        }
        self.progress_clock = Some(clock);
        Ok(self)
    }
    /// A retained service owner can observe source changes between debt slices.
    /// Private callers keep their existing ordered convergence contract.
    pub fn with_interleaved_delivery(mut self) -> Result<Self, RemoteMailboxError> {
        if !self.is_v2() {
            return Err(RemoteMailboxError::ForeignBinding);
        }
        self.interleaved_delivery = true;
        Ok(self)
    }
    fn is_interleaved(&self) -> bool {
        self.interleaved_delivery
    }
    fn is_v2(&self) -> bool {
        self.request.requested_protocol().as_str()
            == grapt_mail_transport::REMOTE_SEAT_MAIL_TRANSPORT_V2
    }

    /// Absent is a validated empty checkpoint only after the whole prefix
    /// completes. A partial page candidate never leaves the runtime owner.
    pub fn validated_checkpoint(
        &self,
    ) -> Result<Option<&ExclusiveSourceCursor>, RemoteMailboxError> {
        if !self.checkpoint_done {
            return Err(RemoteMailboxError::NotLive);
        }
        Ok(self.checkpoint_candidate.as_ref())
    }

    /// Recorded integrity observations, independent of delivered mail count.
    pub fn recorded_integrity_gaps(&self) -> u64 {
        self.integrity_gaps
    }

    /// Local unknown-kind failures never count as source refusal byte ranges.
    pub fn recorded_decode_failures(&self) -> u64 {
        self.decode_failures
    }

    /// The first source positions behind [`Self::recorded_decode_failures`], so
    /// a diagnostic can name where the source reported them. Bounded, never
    /// more than the count, and never the rows themselves.
    pub fn decode_failure_positions(&self) -> &[ExclusiveSourceCursor] {
        &self.decode_failure_positions
    }

    /// Keep the first few positions for the diagnostic, ignoring the rest.
    fn remember_decode_failure_positions(
        &mut self,
        positions: impl IntoIterator<Item = ExclusiveSourceCursor>,
    ) {
        for position in positions {
            if self.decode_failure_positions.len() >= DECODE_FAILURE_POSITIONS {
                return;
            }
            self.decode_failure_positions.push(position);
        }
    }

    pub fn scope(&self) -> &RemoteMailboxScope {
        &self.expected
    }
    pub fn is_live(&self) -> bool {
        self.snapshot_done && self.binding.is_some()
    }

    fn store(&self) -> Result<MutexGuard<'_, Inbox>, RemoteMailboxError> {
        self.inbox
            .lock()
            .map_err(|_| RemoteMailboxError::StoreUnavailable)
    }
    fn limits(&self) -> ProviderLimits {
        self.binding
            .as_ref()
            .map_or(self.requested_limits, |binding| {
                *binding.receipt().limits().provider()
            })
    }
    /// Discard every coordinate, for a new binding or a closed transport.
    fn reset_scan(&mut self) {
        self.subscription = None;
        self.notification_after = MailboxCursor::START;
        self.notification_before = None;
        self.notification_floor = MailboxCursor::START;
        self.notification_top = None;
        self.delivery.forget_marks();
        self.recovery_after = MailboxCursor::START;
        self.recovery_done = false;
        self.checkpoint_before = None;
        self.checkpoint_candidate = None;
        self.checkpoint_done = false;
        self.source_after = None;
        self.snapshot_done = false;
        self.progress_after = None;
        self.progress_prepared = false;
        self.integrity_gaps = 0;
        self.decode_failures = 0;
        self.decode_failure_positions.clear();
    }

    /// Re-observe the source after an accepted advisory, keeping the durable
    /// prefix this binding already verified.
    ///
    /// The fact stream is append-only within a binding and generation: the
    /// store's authorizer refuses `UPDATE`/`DELETE` on `event`, and a page write
    /// must extend the durable checkpoint
    /// (`record_remote_mail_page_within`). Nothing the advisory reports can
    /// rewrite a fact this walk already read, so the walk coordinates stand.
    ///
    /// Only the observation side restarts: the subscription re-arms, and
    /// delivery is re-read because a successor row changes what is actionable.
    /// Dropping the walk here instead re-read the whole recorded stream on every
    /// advisory, which is the steady read that pinned a core.
    fn reobserve(&mut self) {
        self.subscription = None;
        self.source_after = None;
        self.snapshot_done = false;
        self.notification_after = MailboxCursor::START;
        self.notification_before = None;
        self.delivery.observe();
    }

    /// Request a fresh atomic source snapshot at a supported safe boundary.
    /// A partial scan keeps its progress. No synthetic advisory is introduced.
    pub fn request_snapshot(&mut self) {
        if self.is_live() {
            self.reobserve();
        }
    }

    /// The existing closed transient classifier also governs an ended owned
    /// advisory wait. Authentication/takeover refusals never request reconnect.
    pub fn change_failure_is_retryable(&self, failure: &OperationFailure) -> bool {
        io::retryable(&RemoteMailboxError::Port(failure.clone()))
    }

    /// Owns no detached poll. The returned future owns its coordinates and can
    /// be selected with product events while the runtime handles other work.
    /// Drop it before reconnect/close, then call `observe_change` on completion.
    pub fn next_change(
        &mut self,
    ) -> Result<
        impl std::future::Future<Output = Result<ChangeSignal, OperationFailure>> + Send + use<P>,
        RemoteMailboxError,
    > {
        if !self.is_live() {
            return Err(RemoteMailboxError::NotLive);
        }
        let subscription = self
            .subscription
            .as_mut()
            .ok_or(RemoteMailboxError::NotLive)?;
        Ok(self.port.next_change(subscription))
    }

    /// One bounded read on the held binding and snapshot, telling a quiet
    /// subscription from a dead one: a live door answers it at once. The page
    /// is discarded and nothing changes. Call it with no change poll pending.
    /// A subscription that is not live (a rescan is replacing it) has nothing
    /// to probe and passes.
    pub async fn probe(&mut self, bound: std::time::Duration) -> Result<(), RemoteMailboxError> {
        let (Some(binding), Some(subscription), true) = (
            self.binding.as_ref(),
            self.subscription.as_ref(),
            self.is_live(),
        ) else {
            return Ok(());
        };
        let snapshot = subscription.snapshot();
        let page = grapt_mail_transport::PageLimits::new(
            1,
            binding.receipt().limits().aggregate_body_bytes_per_page(),
        );
        let read =
            self.port
                .fetch_snapshot_page(binding, snapshot, self.source_after.as_ref(), page);
        tokio::time::timeout(bound, read)
            .await
            .map_err(|_| RemoteMailboxError::Deadline)?
            .map_err(RemoteMailboxError::Port)?;
        Ok(())
    }

    /// A signal asks for a rescan; it cannot classify work or cause promotion.
    /// An old generation cannot tear down a newer snapshot. A disconnect is
    /// returned explicitly; callers use bounded reconnect, never an idle claim.
    ///
    /// The signal names a change at the source, not a rewrite of what this
    /// binding already recorded, so the verified prefix is kept and only the
    /// head is re-read.
    pub fn observe_change(&mut self, signal: ChangeSignal) -> bool {
        if self
            .subscription
            .as_ref()
            .is_none_or(|subscription| subscription.generation() != signal.generation())
        {
            return false;
        }
        self.reobserve();
        true
    }

    /// One fair slice. V2 records the complete emitted prefix before advancing;
    /// its original delivery debt settles independently. V1 keeps ordered ACKs.
    pub async fn advance_slice(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
    ) -> Result<RemoteMailboxSlice, RemoteMailboxError> {
        io::check_cancel(cancel)?;
        if self.is_v2() && self.progress_clock.is_none() {
            return Err(RemoteMailboxError::ForeignBinding);
        }
        self.ensure_open(cancel).await?;
        self.prepare_progress()?;
        let limits = self.limits();
        let mut result = RemoteMailboxSlice::default();
        while result.pages < limits.max_pages_per_runtime_slice().get()
            && result.rows < limits.max_messages_per_runtime_slice().get()
        {
            io::check_cancel(cancel)?;
            if self.is_live() {
                break;
            }
            let remaining_bytes = limits
                .max_decoded_bytes_per_runtime_slice()
                .get()
                .saturating_sub(result.charged_bytes);
            if remaining_bytes == 0 {
                break;
            }
            let remaining_rows = limits.max_messages_per_runtime_slice().get() - result.rows;
            if !self.recovery_done {
                if self.is_v2() {
                    self.recover_progress(cancel, remaining_rows, remaining_bytes, &mut result)
                        .await?;
                } else {
                    self.recover_page(cancel, remaining_rows, remaining_bytes, &mut result)
                        .await?;
                }
            } else if !self.checkpoint_done {
                self.checkpoint_page(remaining_rows, remaining_bytes, &mut result)?;
            } else {
                if remaining_bytes < limits.max_decoded_page_bytes_including_metadata().get() {
                    if result.pages == 0 {
                        return Err(RemoteMailboxError::SliceTooSmall);
                    }
                    break;
                }
                self.snapshot_page(cancel, remaining_rows, &mut result)
                    .await?;
            }
            // Fairness for cancellation and product work even with a ready fake
            // or a fully cached page. No clock, backoff loop or task is created.
            tokio::task::yield_now().await;
        }
        if self.is_interleaved() && self.is_live() && result.pages == 0 {
            self.settle_delivery_slice(cancel, &mut result).await?;
        }
        result.live = self.is_live();
        Ok(result)
    }

    /// Caller drops the owned poll first. Takeover/authorization failures are
    /// not passed here automatically. No newer owner is challenged on takeover.
    pub async fn reconnect(
        &mut self,
        cancel: &watch::Receiver<Option<InterruptionSource>>,
    ) -> Result<RemoteMailboxSlice, RemoteMailboxError> {
        let limits = self.limits();
        self.close().await?;
        let mut last = None;
        for attempt in 0..limits.max_reconnect_attempts_per_runtime_slice().get() {
            if attempt > 0 {
                let delay = limits.max_single_reconnect_delay_ticks().get().min(
                    limits.max_total_reconnect_delay_ticks_per_slice().get()
                        / (limits.max_reconnect_attempts_per_runtime_slice().get() - 1).max(1),
                );
                io::pause(cancel, delay).await?;
            }
            match self.advance_slice(cancel).await {
                Ok(slice) => return Ok(slice),
                Err(error) if io::retryable(&error) => {
                    last = Some(error);
                    self.close().await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last.ok_or(RemoteMailboxError::NotLive)?)
    }

    pub async fn close(&mut self) -> Result<Option<ShutdownReceipt>, RemoteMailboxError> {
        let limits = self.limits();
        self.reset_scan();
        self.replay_origin = None;
        let Some(binding) = self.binding.take() else {
            return Ok(None);
        };
        let budget = grapt_mail_transport::ShutdownBudget::new(
            limits.max_shutdown_duration_ticks().get(),
            limits.max_joined_transport_tasks_at_shutdown().get(),
        );
        // Shutdown still attempts its bounded close after user cancellation.
        // A cancelled admission is not permission to abandon the owned binding.
        tokio::time::timeout(
            std::time::Duration::from_millis(u64::from(limits.max_shutdown_duration_ticks().get())),
            self.port.close(binding, budget),
        )
        .await
        .map_err(|_| RemoteMailboxError::Deadline)?
        .map(Some)
        .map_err(RemoteMailboxError::Port)
    }
}

#[cfg(test)]
mod tests;
