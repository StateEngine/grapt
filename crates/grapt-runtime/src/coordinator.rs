//! Process-local Session execution coordinator.
//!
//! Law: `SECODE_RUST_FOUNDATIONAL_TYPES_AND_LIFECYCLES_2026-08-07.md`
//! §"Session lifecycle", and the four laws the seed fixture proves
//! (`fixtures/rust-reconstruction/README.md`).
//!
//! # No clustering fiction
//!
//! This is **one process's** view. It is keyed by [`SessionId`] and coordinates
//! only what this process owns. It holds no lease and writes no durable drain
//! identity — a Session being "locally unowned" is a fact about this process,
//! never a fact about the world.
//!
//! # The state machine, and why it is shaped this way
//!
//! Every transition that matters happens under ONE acquisition of the slot
//! lock, because each of these went wrong when it did not:
//!
//! - **Claim or join is one critical section.** A joiner subscribes to the
//!   completion channel *while still holding the lock*, so the owner cannot
//!   finish between the joiner's decision to wait and its registration. A
//!   `Notify` cloned under the lock and awaited after releasing it is a
//!   lost-wakeup hang.
//! - **"Nothing eligible" and "release ownership" are the same critical
//!   section.** Deciding there is no work, releasing the lock, and only then
//!   clearing ownership loses a wake that lands in the gap: the coordinator
//!   would clear its `wake_pending` flag and report success while newly
//!   admitted work sat unexecuted.
//! - **Cancellation is stored unconditionally.** `watch::Sender::send` fails
//!   *and discards the value* when no receiver exists, so an interrupt arriving
//!   before the run subscribed was silently dropped while `interrupt()`
//!   reported success. Updates use `send_replace`, and the slot retains a
//!   receiver so a later subscription still observes the current value.
//! - **Ownership is released even if the owner is dropped.** An aborted wake
//!   future used to leave `active = true` forever, wedging that Session for the
//!   life of the process. A drop guard releases it and publishes a terminal
//!   result, so joiners learn the truth instead of waiting on a corpse.
//! - **An abandoned drain is answered at once but not replaced at once.** That
//!   drop guard runs synchronously, and a drain's frames are not all it owns: a
//!   `tokio` task aborted on the way out is destroyed later, by the runtime, at
//!   a moment no `Drop` can await. Freeing the Session there admitted a second
//!   drain beside the first's live provider reader. So abandonment splits in
//!   two — joiners get the terminal result immediately, because they are
//!   waiting on a *result*; the right to CLAIM waits for
//!   [`residency`](crate::residency) to confirm the old drain's tasks are gone.
//!   The contract is explicit that this is not optional: "Rust does not pretend
//!   `Drop` completed this work."
//!
//! # Errors are never silently absent
//!
//! The application seam is `Result`-bearing throughout. An inbox read or a Step
//! failure must not become `None` — which the coordinator would read as "no
//! work" — nor a panic, nor a false success. Joiners receive the owner's
//! terminal result rather than an unconditional `Joined`.
//!
//! # The drain owns its Location, and its boundaries are Steps
//!
//! A drain is not a loop over promotions. It is a loop over **logical Step
//! boundaries**, and it owns one context ([`SessionApplication::Drain`]) for
//! its whole life:
//!
//! - Placement is discovered once, at [`SessionApplication::begin_drain`], and
//!   the Location runtime it names is resolved there and owned by that context.
//!   Resolving per promotion let one drain reach two different runtimes.
//! - Every steer eligible at a boundary promotes — a **batch** — before the
//!   next Step, so a prompt typed mid-turn reaches the model at the next safe
//!   boundary instead of waiting for the whole chain to finish.
//! - A queued input is offered only at an otherwise-idle boundary: nothing
//!   steered here, and no continuation owed. After one is promoted,
//!   continuation is reevaluated before another is offered.
//! - Exactly one Step runs per iteration. A Step that ends still calling tools
//!   returns [`RunOutcome::Continue`], which is what makes the next boundary
//!   real rather than a comment inside an opaque chain.
//!
//! # What it deliberately does not do
//!
//! No durable drain identity, no transcript boundary, and no post-crash or
//! post-interrupt provider auto-retry: the coordinator never re-runs a
//! promotion on its own.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_types::{DeliveryMode, InterruptionSource, LocationId, PendingInputId, SessionId};
use tokio::sync::{watch, Semaphore};

use crate::residency::{Cancellation, DrainResidency};
use crate::supervisor::BoxFuture;

/// Where a Session's work runs. Discovered from the store when a drain starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub location: LocationId,
}

/// Which promotion boundary an eligibility pass is asking about.
///
/// The order is the law: steers are offered first, and a queued input is only
/// offered at an otherwise-idle boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Steer,
    Idle,
}

/// One durable inbox row the application chose to promote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub input: PendingInputId,
    pub delivery: DeliveryMode,
}

/// What ONE logical Step left the drain in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The Step ended still calling tools: the terminals it committed are new
    /// model-visible history and the chain owes a continuation. The coordinator
    /// promotes pending steers before that continuation and offers no queued
    /// work while it is owed.
    Continue,
    /// The chain reached its terminal. The Session is idle unless more durable
    /// work is eligible.
    Idle,
    /// The run was interrupted. No retry and no further Step follows.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoordinatorError {
    /// A public request named a Session this process cannot see. Typed
    /// rejection, deliberately distinct from a known-but-idle no-op.
    #[error("unknown session {session}")]
    UnknownSession { session: String },
    #[error("placement lookup failed for {session}: {reason}")]
    Placement { session: String, reason: String },
    /// The inbox could not be read. Never collapsed into "no work eligible".
    #[error("inbox read failed for {session}: {reason}")]
    Inbox { session: String, reason: String },
    /// Running a promotion failed. Never collapsed into a successful outcome.
    #[error("run failed for {session}: {reason}")]
    Run { session: String, reason: String },
    /// One promoted batch spent its whole Step budget while the model was still
    /// calling tools. Typed, so the product can say so publicly without
    /// publishing arbitrary runtime text.
    #[error("the {policy} Step budget was exhausted for {session} after spending {spent} of {granted} Steps while the model was still calling tools")]
    StepBudgetExhausted {
        session: SessionId,
        policy: grapt_types::PolicyVersion,
        spent: u32,
        granted: u32,
    },
    /// Provider admission waited to its bound or reached its bounded queue limit.
    #[error(
        "provider slot admission for {session} is temporarily saturated; retry or resume the child"
    )]
    ProviderCapacity { session: SessionId },
    /// The provider requires account action before more work can be admitted.
    #[error("the provider usage limit was reached for {session}")]
    ProviderLimit {
        session: SessionId,
        reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
    },
    /// A Step ended because the provider failed, with the adapter's bounded,
    /// already-redacted detail. That detail is the status, the vendor's code
    /// and an excerpt when the provider answered with an error status, and the
    /// adapter's own reason when it failed on the wire; either way it is
    /// product-authored text the row may show, never a raw provider body.
    #[error("the provider failed {session}: {summary}")]
    ProviderFailed { session: SessionId, summary: String },
    /// A Step ended with a typed context or compaction failure.
    #[error("run failed for {session}: {reason}")]
    Context {
        session: SessionId,
        failure: grapt_types::ContextFailure,
        /// Optional recovery diagnosis; it grants no context or retry authority.
        continuity: Option<grapt_session::execution::ContinuityFailure>,
        reason: String,
    },
    /// The task owning the drain was dropped before finishing. Joiners are told
    /// this rather than waiting forever or receiving a false success.
    #[error("the drain owner for {session} was dropped before completing")]
    OwnerAbandoned { session: String },
    /// The coordinator owns this semaphore for its whole lifetime and never
    /// closes it. Keeping the impossible shutdown observable nevertheless
    /// avoids turning a failed capacity acquisition into an unbounded wait or
    /// a panic.
    #[error("the bounded local-drain capacity authority was closed")]
    CapacityClosed,
}

/// The application seam. Real implementations reach the store and the Location
/// service map; tests inject a deterministic one.
///
/// Note what is absent: after [`begin_drain`](SessionApplication::begin_drain)
/// no method takes a `SessionId`, a `Placement`, or anything else a second
/// Location resolution could be built from. One drain, one resolution, carried
/// in the context.
pub trait SessionApplication: Send + Sync + 'static {
    /// Everything one drain resolves once and then owns: its placement, the
    /// Location runtime serving that placement, and the Step chain in flight.
    ///
    /// `Send` because the drain crosses `.await` points inside the owning wake
    /// task. Deliberately not `Clone`: a second handle to a drain would be a
    /// second chain over one Session's transcript.
    type Drain: Send;

    /// Whether this process can see the Session at all. Separates an unknown
    /// Session (typed rejection) from a known idle one (no-op).
    ///
    /// `Result`-bearing because production answers this from the store: a read
    /// failure must surface as a store error, never collapse into
    /// `UnknownSession`, which would tell a caller their Session does not exist
    /// because a disk hiccuped.
    fn session_exists(&self, session: &SessionId) -> Result<bool, CoordinatorError>;

    /// Start a drain: discover placement from the store, resolve the Location
    /// runtime that serves it, and return the context owning both.
    ///
    /// Called exactly once per drain. It is the ONLY placement seam, which is
    /// what makes "one drain runs on one Location" structural rather than a
    /// convention every later call has to keep.
    fn begin_drain(&self, session: &SessionId) -> Result<Self::Drain, CoordinatorError>;

    /// Whether the drain owes a Step: a chain that has not reached its
    /// terminal, including one that has just taken a promotion and not yet run.
    ///
    /// While this is true the Session is not idle, whatever the inbox holds, and
    /// no queued input is offered.
    fn continuation_owed(&self, drain: &Self::Drain) -> bool;

    /// One eligibility pass at the given boundary. `Ok(None)` means nothing is
    /// eligible there; an `Err` is a real failure and never means "no work".
    ///
    /// A promotion returned here has ALREADY joined the drain: it either opened
    /// the chain or was admitted into the one in flight, and it reset the step
    /// allowance (once for a batch of steers). The coordinator decides
    /// boundaries; the application owns what a promotion does to the chain.
    fn next_eligible(
        &self,
        drain: &mut Self::Drain,
        boundary: Boundary,
    ) -> Result<Option<Promotion>, CoordinatorError>;

    /// Run exactly ONE logical Step of the drain's chain.
    ///
    /// Returning after each Step is what gives the coordinator a real boundary
    /// to promote at. [`Cancellation`] carries interruption for this Session's
    /// active ownership chain, and the residency token every task this Step
    /// spawns must hold so the coordinator can tell when they are gone.
    fn advance<'a>(
        &'a self,
        drain: &'a mut Self::Drain,
        cancellation: Cancellation,
    ) -> BoxFuture<'a, Result<RunOutcome, CoordinatorError>>;
}

/// Result of a wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOutcome {
    Drained {
        passes: u32,
        promotions: u32,
        /// Logical Steps taken, one per `advance`. A tool-calling chain reports
        /// more Steps than promotions; a steer batch reports fewer.
        steps: u32,
        /// Always exactly one per drain, structurally: `begin_drain` is the only
        /// placement seam and the coordinator calls it once.
        placement_lookups: u32,
    },
    /// A drain was already active; this caller joined it rather than starting a
    /// second one. Only returned when that drain SUCCEEDED — a failed drain
    /// returns its error to joiners too.
    Joined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptOutcome {
    Interrupted,
    /// Known but no active local drain. A no-op, not an error.
    NoOpIdle,
}

/// Published to joiners. `Finished` carries the owner's terminal truth.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DrainState {
    Active,
    Finished(Result<(), CoordinatorError>),
}

/// What a wake decided under the slot lock.
///
/// Named rather than expressed as nested `Option`s because the third case is
/// the whole point: "not now, and not as a joiner either — wait for the last
/// drain's work to be gone."
enum Claim {
    Owner,
    Joiner(watch::Receiver<DrainState>),
    AwaitCleanup(Arc<DrainResidency>),
}

struct Slot {
    active: bool,
    /// True only after this owner holds one of the coordinator's bounded
    /// process-wide drain permits. A claimed Session may wait for capacity,
    /// but `/active` and interrupt must describe work that is actually
    /// draining, not a waiter queued behind the bound.
    capacity_acquired: bool,
    /// A wake arrived during the active drain: one more eligibility pass is
    /// owed. Coalesced, so many wakes still mean one extra pass.
    wake_pending: bool,
    cancel: watch::Sender<Option<InterruptionSource>>,
    /// Retained so `cancel` always has a receiver: a `watch` send discards the
    /// value when the receiver count is zero.
    cancel_keepalive: watch::Receiver<Option<InterruptionSource>>,
    /// Joiners subscribe to this UNDER THE LOCK, so a completion cannot slip
    /// between their decision to wait and their registration.
    state: watch::Sender<DrainState>,
    /// The current drain's residency: the tokens its spawned tasks hold.
    residency: Arc<DrainResidency>,
    /// Set when a drain was ABANDONED — its owning future was dropped without
    /// finishing, typically because the wake's task was aborted.
    ///
    /// The owner is gone, but `abort` is a request and its tasks may not be.
    /// Until this closes the Session is NOT claimable: handing it to a
    /// replacement drain here is exactly how one Session ends up with two live
    /// provider readers. Joiners are told the drain's terminal truth
    /// immediately; only the right to start a new one waits.
    cancelling: Option<Arc<DrainResidency>>,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("active", &self.active)
            .field("wake_pending", &self.wake_pending)
            .finish()
    }
}

/// Session-ID keyed, process-global execution coordinator.
#[derive(Debug)]
pub struct SessionCoordinator<A> {
    app: Arc<A>,
    // std::sync::Mutex on purpose: the guard is never held across an await, and
    // a sync lock makes that structurally obvious rather than a habit.
    slots: Mutex<HashMap<SessionId, Slot>>,
    capacity: Semaphore,
}

/// The production process-wide bound on concurrent Session drains.
pub const MAX_ACTIVE_SESSION_DRAINS: usize = 64;

/// Releases drain ownership even if the owning future is dropped, and publishes
/// a terminal result so joiners are never left waiting on a corpse.
struct OwnershipGuard<'a, A: SessionApplication> {
    coordinator: &'a SessionCoordinator<A>,
    session: SessionId,
    released: bool,
}

impl<A: SessionApplication> Drop for OwnershipGuard<'_, A> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        // ABANDONED, not released. Everything this drain owned in a frame has
        // just unwound, but anything it SPAWNED has only been asked to stop —
        // and `Drop` cannot await the difference. Marking the Session free here
        // would let a replacement drain open a second provider reader beside
        // the first. `abandon` publishes the terminal truth to joiners at once
        // and defers only the right to claim.
        self.coordinator.abandon(&self.session);
    }
}

impl<A: SessionApplication> SessionCoordinator<A> {
    pub fn new(app: Arc<A>) -> Self {
        let capacity = match NonZeroUsize::new(MAX_ACTIVE_SESSION_DRAINS) {
            Some(capacity) => capacity,
            None => unreachable!("the production Session drain bound is non-zero"),
        };
        Self::with_capacity(app, capacity)
    }

    /// Construct a coordinator with an explicit non-zero process-wide drain
    /// bound. Production and the exact 64/65 witness both use
    /// [`MAX_ACTIVE_SESSION_DRAINS`]; smaller capacities remain available for
    /// focused ownership tests.
    pub fn with_capacity(app: Arc<A>, capacity: NonZeroUsize) -> Self {
        Self {
            app,
            slots: Mutex::new(HashMap::new()),
            capacity: Semaphore::new(capacity.get()),
        }
    }

    /// Wake a Session: a resume, or a prompt that has already been durably
    /// admitted. Advisory — it drains eligible durable rows and nothing else.
    pub async fn wake(&self, session: SessionId) -> Result<WakeOutcome, CoordinatorError> {
        self.wake_with(session, || None).await
    }

    /// The same, with the drain this caller comes to own starting from
    /// `claimed()`: an interrupt that reached the caller while it prepared,
    /// before any drain existed to receive it. `claimed` runs ONCE, under
    /// the slot lock, at the instant this caller becomes the owner, so a
    /// caller can hand its own cancellation over without a gap: an interrupt
    /// before that instant is the caller's to report, and one after it finds
    /// this drain's slot active. The drain then ends as an ordinary
    /// interrupt, its promoted input settled like any other. A drain this
    /// caller only joins is not interrupted by it, and `claimed` is not run.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub async fn wake_with(
        &self,
        session: SessionId,
        claimed: impl FnOnce() -> Option<InterruptionSource>,
    ) -> Result<WakeOutcome, CoordinatorError> {
        let mut claimed = Some(claimed);
        if !self.app.session_exists(&session)? {
            return Err(CoordinatorError::UnknownSession {
                session: session.as_str().to_owned(),
            });
        }

        // Claim the drain, join the one in flight, or wait out an abandoned
        // one's cleanup. Each decision is taken under the lock — subscribing
        // included, so an owner cannot finish in the gap between a joiner
        // deciding to wait and registering.
        //
        // A loop, because the cleanup wait must happen with the lock RELEASED
        // and the world may have moved on by the time it finishes: another
        // caller may have claimed the Session while this one waited, and it
        // must then join rather than claim.
        let joiner = loop {
            let claim = {
                let mut slots = self.lock_slots();
                let slot = slots.entry(session.clone()).or_insert_with(|| {
                    let (cancel, cancel_keepalive) = watch::channel(None);
                    let (state, _) = watch::channel(DrainState::Active);
                    Slot {
                        active: false,
                        capacity_acquired: false,
                        wake_pending: false,
                        cancel,
                        cancel_keepalive,
                        state,
                        residency: DrainResidency::new(),
                        cancelling: None,
                    }
                });
                if slot.active {
                    slot.wake_pending = true;
                    Claim::Joiner(slot.state.subscribe())
                } else if let Some(cancelling) =
                    slot.cancelling.as_ref().filter(|it| !it.is_closed())
                {
                    // An abandoned drain's tasks are still being destroyed.
                    Claim::AwaitCleanup(Arc::clone(cancelling))
                } else {
                    slot.cancelling = None;
                    slot.active = true;
                    slot.capacity_acquired = false;
                    slot.wake_pending = false;
                    // A fresh cancellation channel per drain: interrupting a
                    // previous drain must not cancel this one. The receiver is
                    // retained so sends are never discarded.
                    let interrupted = claimed.take().and_then(|claimed| claimed());
                    let (cancel, cancel_keepalive) = watch::channel(interrupted);
                    slot.cancel = cancel;
                    slot.cancel_keepalive = cancel_keepalive;
                    // A FRESH completion channel per drain. Reusing one lets a
                    // later claim publish Active over the previous drain's
                    // terminal value before its joiner has polled, so that joiner
                    // would miss owner-1's truth and observe drain-2 instead.
                    // Old joiners keep the old receiver, which retains the value
                    // they are entitled to.
                    let (state, _) = watch::channel(DrainState::Active);
                    slot.state = state;
                    // And a fresh residency: this drain's spawned work is its
                    // own, and must never be measured against the last one's.
                    slot.residency = DrainResidency::new();
                    Claim::Owner
                }
            };
            match claim {
                Claim::Owner => break None,
                Claim::Joiner(state) => break Some(state),
                Claim::AwaitCleanup(cancelling) => cancelling.closed().await,
            }
        };

        if let Some(mut state) = joiner {
            loop {
                // borrow_and_update marks the current value seen, so changed()
                // cannot return the same state twice.
                let current = state.borrow_and_update().clone();
                match current {
                    DrainState::Finished(result) => {
                        return result.map(|()| WakeOutcome::Joined);
                    }
                    DrainState::Active => {}
                }
                if state.changed().await.is_err() {
                    // Sender gone without publishing: abandoned, rather than
                    // hanging or claiming success.
                    return Err(CoordinatorError::OwnerAbandoned {
                        session: session.as_str().to_owned(),
                    });
                }
            }
        }

        let mut guard = OwnershipGuard {
            coordinator: self,
            session: session.clone(),
            released: false,
        };
        // Capacity is acquired only by the one caller that won Session
        // ownership. Same-Session joiners therefore consume no second permit.
        // A 65th distinct Session remains durably admitted and waits here; when
        // one of the first 64 drains releases, this exact caller continues and
        // drains it. Dropping the caller while it waits triggers the ownership
        // guard and cannot leave a phantom active slot.
        let permit = self
            .capacity
            .acquire()
            .await
            .map_err(|_| CoordinatorError::CapacityClosed)?;
        {
            let mut slots = self.lock_slots();
            if let Some(slot) = slots.get_mut(&session) {
                slot.capacity_acquired = true;
            }
        }
        let result = self.drain(&session).await;
        guard.released = true;
        let outcome = match result {
            Ok((outcome, already_released)) => {
                if !already_released {
                    // Interrupt path: the drain broke out still owning the slot.
                    self.release(&session, Ok(()));
                }
                Ok(outcome)
            }
            Err(error) => {
                self.release(&session, Err(error.clone()));
                Err(error)
            }
        };
        // Release the permit only after the slot transition above. Reversing
        // these two operations gives the next waiter a brief interval in
        // which `/active` can truthfully observe 65 acquired drains.
        drop(permit);
        outcome
    }

    /// Interrupt a Session's active local ownership chain.
    ///
    /// Known but not locally draining is a no-op. Unknown is a typed rejection.
    pub fn interrupt(&self, session: &SessionId) -> Result<InterruptOutcome, CoordinatorError> {
        self.interrupt_by(session, InterruptionSource::User)
    }

    /// The lifecycle owner supplies the source before waking an active drain.
    pub fn interrupt_by(
        &self,
        session: &SessionId,
        source: InterruptionSource,
    ) -> Result<InterruptOutcome, CoordinatorError> {
        if !self.app.session_exists(session)? {
            return Err(CoordinatorError::UnknownSession {
                session: session.as_str().to_owned(),
            });
        }
        let slots = self.lock_slots();
        match slots.get(session) {
            // A claimed drain still waiting for capacity is interrupted too:
            // its cancellation is read when it starts, so it ends at once
            // rather than running the turn the user stopped.
            Some(slot) if slot.active => {
                // The retained receiver keeps an interrupt observable even
                // before the run subscribes; the first cause stays authoritative.
                // The caller names the source before this wake can settle.
                // User interruption and managed lifecycle shutdown share the
                // same cancellation channel without changing their provenance.
                slot.cancel.send_if_modified(|cause| {
                    if cause.is_some() {
                        return false;
                    }
                    *cause = Some(source);
                    true
                });
                Ok(InterruptOutcome::Interrupted)
            }
            _ => Ok(InterruptOutcome::NoOpIdle),
        }
    }

    /// Whether a wake has arrived during the active drain and is still owed an
    /// eligibility pass. Diagnostics, and the state a test must wait on to
    /// observe the final-cut race deterministically rather than by timing.
    pub fn has_pending_wake(&self, session: &SessionId) -> bool {
        self.lock_slots()
            .get(session)
            .map(|slot| slot.wake_pending)
            .unwrap_or(false)
    }

    /// Whether this process currently owns an active drain for the Session.
    pub fn is_locally_draining(&self, session: &SessionId) -> bool {
        self.lock_slots()
            .get(session)
            .map(|slot| slot.active && slot.capacity_acquired)
            .unwrap_or(false)
    }

    /// A bounded, coordinator-owned projection of the Sessions this process
    /// is actively draining. This is the sole source for product `/active`;
    /// callers receive identities, never a handle with which to mutate slots.
    pub fn active_sessions(&self) -> Vec<SessionId> {
        let mut sessions: Vec<_> = self
            .lock_slots()
            .iter()
            .filter(|(_, slot)| slot.active && slot.capacity_acquired)
            .map(|(session, _)| session.clone())
            .collect();
        sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        sessions
    }

    /// Returns the outcome plus whether ownership was already released as part
    /// of the quiescent transition.
    async fn drain(&self, session: &SessionId) -> Result<(WakeOutcome, bool), CoordinatorError> {
        // Placement is discovered HERE — once, when the drain starts — and the
        // Location runtime serving it is resolved into the context every Step
        // below reaches through.
        let mut drain = self.app.begin_drain(session)?;
        let placement_lookups = 1u32;
        let mut passes = 0u32;
        let mut promotions = 0u32;
        let mut steps = 0u32;
        let mut released = false;

        loop {
            // THE STEER BATCH. Every steer eligible at this boundary promotes
            // before the next Step, because a batch of steers is one input
            // event to the model rather than a queue of turns. Each promotion is
            // still its own durable transaction: the batch is a boundary
            // decision, not a transaction boundary. An Err here is a real inbox
            // failure and must never read as "done".
            let mut steered = 0u32;
            loop {
                passes += 1;
                match self.app.next_eligible(&mut drain, Boundary::Steer)? {
                    Some(_) => {
                        promotions += 1;
                        steered += 1;
                    }
                    None => break,
                }
            }

            // THE IDLE BOUNDARY. A queued input is offered only when the Session
            // would otherwise become idle: nothing steered into this boundary
            // and no continuation owed. Exactly one, after which continuation is
            // reevaluated at the top of the loop before another is offered.
            if steered == 0 && !self.app.continuation_owed(&drain) {
                passes += 1;
                if self
                    .app
                    .next_eligible(&mut drain, Boundary::Idle)?
                    .is_some()
                {
                    promotions += 1;
                    // A human promotion can re-enable held native work. Recheck
                    // the existing Steer boundary before planning the Step;
                    // its owed continuation prevents another Queue promotion.
                    continue;
                }
            }

            // ONE STEP. Returning to the top after each one is what makes the
            // next boundary real: a steer admitted while this Step ran promotes
            // before the continuation it belongs in.
            if self.app.continuation_owed(&drain) {
                steps += 1;
                let cancellation = self.cancellation(session);
                match self.app.advance(&mut drain, cancellation).await? {
                    // An interrupt ends the drain. No retry, no further Step and
                    // no further promotion: the user stopped this chain.
                    RunOutcome::Interrupted => break,
                    RunOutcome::Continue | RunOutcome::Idle => continue,
                }
            }

            // Nothing eligible. The quiescent decision, clearing ownership and
            // publishing the terminal result are ONE lock transition. Deciding
            // here and releasing later — even under a second lock immediately
            // after — leaves a gap in which a wake sees active=true, registers
            // as a joiner, and is then cleared by the release: its work is lost
            // and the joiner is told the drain succeeded.
            {
                let mut slots = self.lock_slots();
                match slots.get_mut(session) {
                    Some(slot) if slot.wake_pending => {
                        slot.wake_pending = false;
                        // Loop again while still owning the drain.
                        continue;
                    }
                    Some(slot) => {
                        slot.active = false;
                        slot.capacity_acquired = false;
                        slot.wake_pending = false;
                        slot.state.send_replace(DrainState::Finished(Ok(())));
                        if slot.residency.is_closed() {
                            slots.remove(session);
                        }
                        released = true;
                        break;
                    }
                    None => break,
                }
            }
        }

        Ok((
            WakeOutcome::Drained {
                passes,
                promotions,
                steps,
                placement_lookups,
            },
            released,
        ))
    }

    /// Release ownership and publish the terminal result to joiners, atomically.
    fn release(&self, session: &SessionId, result: Result<(), CoordinatorError>) {
        let mut slots = self.lock_slots();
        if let Some(slot) = slots.get_mut(session) {
            if !slot.active {
                return;
            }
            slot.active = false;
            slot.capacity_acquired = false;
            slot.wake_pending = false;
            slot.state.send_replace(DrainState::Finished(result));
            if slot.residency.is_closed() {
                slots.remove(session);
            }
        }
    }

    /// Mark an abandoned drain: publish its terminal truth, but keep the
    /// Session unclaimable until its spawned work is destroyed.
    ///
    /// The two halves are deliberately separated. A joiner is owed an answer
    /// now — it is waiting on a result, not on a resource. A *claimant* is
    /// asking to run a second drain over the same Session, and that must wait
    /// for the first one's reader to be gone, not merely told to go.
    fn abandon(&self, session: &SessionId) {
        let mut slots = self.lock_slots();
        if let Some(slot) = slots.get_mut(session) {
            if !slot.active {
                return;
            }
            slot.active = false;
            slot.capacity_acquired = false;
            slot.wake_pending = false;
            slot.cancelling = Some(Arc::clone(&slot.residency));
            slot.state.send_replace(DrainState::Finished(Err(
                CoordinatorError::OwnerAbandoned {
                    session: session.as_str().to_owned(),
                },
            )));
            if slot.residency.is_closed() {
                slots.remove(session);
            }
        }
    }

    fn cancellation(&self, session: &SessionId) -> Cancellation {
        let slots = self.lock_slots();
        match slots.get(session) {
            Some(slot) => Cancellation::new(slot.cancel.subscribe(), slot.residency.token()),
            None => {
                let (_, rx) = watch::channel(None);
                Cancellation::new(rx, DrainResidency::new().token())
            }
        }
    }

    /// A poisoned slot map would mean a panic inside the coordinator itself.
    /// Recovering the guard keeps one Session's panic from wedging every other
    /// Session in the process.
    fn lock_slots(&self) -> MutexGuard<'_, HashMap<SessionId, Slot>> {
        let mut slots = match self.slots.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Finished joiners keep their channel's terminal value after its
        // sender goes away. Abandoned work retains its slot until its last
        // residency token closes; reap it on the next coordinator operation.
        slots.retain(|_, slot| slot.active || !slot.residency.is_closed());
        slots
    }
}

#[cfg(test)]
#[path = "coordinator_retirement_tests.rs"]
mod retirement_tests;
