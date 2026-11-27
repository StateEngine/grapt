//! The attempt-capacity registry, shared by every route adapter.
//!
//! Shared with the ChatGPT/Codex route.
//! Contract rule 1 — "One PhysicalAttempt performs exactly one provider
//! `stream(request)` call" — is not a property of one vendor, and neither is
//! the bounded-everything law recorded below. A per-route copy
//! would be four registries that could each be relaxed separately, and three of
//! them would have no independent evidence that they had been.

use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use grapt_types::PhysicalAttemptId;

use crate::codex::CodexError;

/// The adapter's bounded operating limits.
///
/// # An injected capacity, not an inherited one
///
/// Where the bound comes from was settled after archaeology not possible from
/// inside this crate: **the existing bytes do not ratify a global
/// concurrent-attempt count.** `ATTEMPT_TOOL_BUDGET = 16` bounds tools within
/// one attempt, `PROVIDER_CHANNEL_CAPACITY = 64` bounds fragments within one
/// attempt, and `MAX_TOTAL_ATTEMPTS = 5` bounds *sequential* retries per Step;
/// the policy explicitly permits different Sessions to run concurrently with
/// no global count. Reusing any of those numbers here would be a borrowed
/// justification for a different law.
///
/// So the capacity is **injected and has no `Default`**, exactly as
/// [`crate::CodexDeadlines`] carries no default deadline. The production
/// number is a deployment decision that belongs to the future
/// runtime/registry wiring and is recorded as a composition residual; this
/// adapter is bounded by construction without inventing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexLimits {
    active_attempts: std::num::NonZeroUsize,
}

impl CodexLimits {
    /// `NonZeroUsize` because a zero-capacity adapter can never open anything;
    /// the type refuses the mistake rather than a runtime check reporting it
    /// later as a saturated adapter.
    pub fn new(active_attempts: std::num::NonZeroUsize) -> Self {
        Self { active_attempts }
    }

    pub fn active_attempts(self) -> usize {
        self.active_attempts.get()
    }
}

/// Futures waiting for a slot at once. A waiter beyond it is refused as a
/// saturated adapter rather than queued without bound.
const MAX_WAITING: usize = 1_024;

/// The set of attempts currently streaming, and the capacity that bounds it.
///
/// Shared with every live [`CodexStream`] so a slot is released when the stream
/// terminates **or is dropped** — and a dropped stream is what cancellation
/// looks like from here, which is why release is a `Drop` obligation rather
/// than a call the caller can forget.
#[derive(Debug, Clone)]
pub(crate) struct ActiveAttempts {
    inner: Arc<std::sync::Mutex<Registry>>,
    capacity: usize,
    pressure: Arc<std::sync::Mutex<Option<Pressure>>>,
    adaptive: bool,
}

/// Streaming attempts, slots reserved for attempts not yet staged, and the
/// tasks waiting for either to free one.
#[derive(Debug, Default)]
struct Registry {
    active: HashSet<PhysicalAttemptId>,
    reserved: usize,
    progress: u64,
    waiting: VecDeque<Waiter>,
}

#[derive(Debug)]
struct Pressure {
    limit: usize,
    retry_at: tokio::time::Instant,
}

#[derive(Debug)]
struct Waiter {
    identity: Arc<()>,
    waker: Waker,
}

impl Registry {
    fn occupied(&self) -> usize {
        self.active.len() + self.reserved
    }

    fn wake_waiters(&self) -> Vec<Waker> {
        self.waiting
            .iter()
            .map(|waiter| waiter.waker.clone())
            .collect()
    }

    fn next(&self, capacity: usize) -> Option<Waker> {
        (self.occupied() < capacity)
            .then(|| self.waiting.front().map(|waiter| waiter.waker.clone()))
            .flatten()
    }
}

impl ActiveAttempts {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(Registry::default())),
            capacity,
            pressure: Arc::new(std::sync::Mutex::new(None)),
            adaptive: true,
        }
    }

    /// Root slots share account cooldown, while child limits remain separate.
    pub(super) fn share_account(&mut self, children: &Self) {
        self.pressure = Arc::clone(&children.pressure);
        self.adaptive = false;
    }

    async fn wait_for_cooldown(&self) -> Result<(), CodexError> {
        loop {
            let deadline = {
                let state = self
                    .pressure
                    .lock()
                    .map_err(|_| CodexError::RegistryPoisoned)?;
                state
                    .as_ref()
                    .map(|state| state.retry_at)
                    .filter(|deadline| *deadline > tokio::time::Instant::now())
            };
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => return Ok(()),
            }
        }
    }

    /// Claim a slot for `attempt`, or say exactly why not. A `reservation`
    /// taken from this registry becomes the slot, so a reserved attempt never
    /// meets a capacity refusal.
    pub(crate) fn claim(
        &self,
        attempt: &PhysicalAttemptId,
        reservation: Option<SlotReservation>,
    ) -> Result<AttemptSlot, CodexError> {
        let reserved = reservation.is_some_and(|reservation| reservation.consume_for(self));
        let mut registry = match self.inner.lock() {
            Ok(registry) => registry,
            Err(_) => return Err(CodexError::RegistryPoisoned),
        };
        if reserved {
            registry.reserved = registry.reserved.saturating_sub(1);
        }
        if registry.active.contains(attempt) {
            let next = registry.next(self.capacity);
            drop(registry);
            if let Some(next) = next {
                next.wake();
            }
            return Err(CodexError::DuplicateActiveAttempt {
                attempt: attempt.as_str().to_owned(),
            });
        }
        if !reserved && (registry.occupied() >= self.capacity || !registry.waiting.is_empty()) {
            return Err(CodexError::ActiveAttemptCapacity {
                capacity: self.capacity,
            });
        }
        registry.active.insert(attempt.clone());
        Ok(AttemptSlot {
            registry: self.clone(),
            attempt: attempt.clone(),
            feedback: None,
        })
    }

    /// Wait for a free slot and hold it for an attempt about to be staged.
    /// Waiting is the caller's to bound: an automatic compaction
    /// must not lose its turn to whichever stream on the adapter happens to
    /// be open, but a slot that never frees is still a saturated adapter.
    #[cfg(test)]
    pub(crate) fn reserve(&self) -> Reserve<'_> {
        self.reserve_observed(None)
    }

    pub(crate) fn reserve_observed<'a>(
        &self,
        observer: Option<Box<dyn FnMut(usize, bool) + Send + 'a>>,
    ) -> Reserve<'a> {
        self.reserve_limited(self.capacity, observer)
    }

    pub(super) fn reserve_limited<'a>(
        &self,
        limit: usize,
        observer: Option<Box<dyn FnMut(usize, bool) + Send + 'a>>,
    ) -> Reserve<'a> {
        Reserve {
            limit: limit.min(self.capacity),
            cooldown: None,
            registry: self.clone(),
            identity: Arc::new(()),
            observed: None,
            observer,
        }
    }

    fn rate_limited(&self, after: Option<std::time::Duration>) {
        {
            let Ok(mut pressure) = self.pressure.lock() else {
                return;
            };
            let current = pressure.as_ref().map_or(self.capacity, |state| state.limit);
            let delay = after.unwrap_or(std::time::Duration::from_secs(1)).clamp(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(30),
            );
            let retry_at = tokio::time::Instant::now() + delay;
            let retry_at = pressure
                .as_ref()
                .map_or(retry_at, |state| retry_at.max(state.retry_at));
            *pressure = Some(Pressure {
                limit: (current / 2).max(1),
                retry_at,
            });
        }
        self.wake_waiters();
    }

    fn successful(&self) {
        {
            let Ok(mut pressure) = self.pressure.lock() else {
                return;
            };
            if let Some(state) = pressure.as_mut() {
                if tokio::time::Instant::now() >= state.retry_at {
                    state.limit = state.limit.saturating_add(1).min(self.capacity);
                    if state.limit == self.capacity {
                        *pressure = None;
                    }
                }
            }
        }
        self.wake_waiters();
    }

    fn wake_waiters(&self) {
        let wake = match self.inner.lock() {
            Ok(registry) => registry.wake_waiters(),
            Err(_) => return,
        };
        for waker in wake {
            waker.wake();
        }
    }

    fn progress(&self) {
        let wake = match self.inner.lock() {
            Ok(mut registry) => {
                registry.progress = registry.progress.wrapping_add(1);
                registry.wake_waiters()
            }
            Err(_) => return,
        };
        for waker in wake {
            waker.wake();
        }
    }

    fn release(&self, attempt: Option<&PhysicalAttemptId>) {
        // A poisoned lock cannot be reported from `Drop`, and refusing to
        // release would leak the slot for the process's lifetime. Releasing is
        // the safe direction: the worst case is a freed slot, never a
        // duplicate stream, because the stream that held it is being destroyed.
        let next = match self.inner.lock() {
            Ok(mut registry) => {
                match attempt {
                    Some(attempt) => {
                        registry.active.remove(attempt);
                    }
                    None => registry.reserved = registry.reserved.saturating_sub(1),
                }
                registry.progress = registry.progress.wrapping_add(1);
                registry.wake_waiters()
            }
            Err(_) => return,
        };
        for waker in next {
            waker.wake();
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(registry) => registry.active.len(),
            Err(_) => panic!("test registry lock"),
        }
    }
}

/// A FIFO reservation. Its identity survives waker changes; dropping a wait
/// removes it immediately so cancellation cannot consume queue capacity or
/// strand the next child.
pub(crate) struct Reserve<'a> {
    limit: usize,
    cooldown: Option<Pin<Box<tokio::time::Sleep>>>,
    registry: ActiveAttempts,
    identity: Arc<()>,
    observed: Option<(usize, u64)>,
    observer: Option<Box<dyn FnMut(usize, bool) + Send + 'a>>,
}

impl Future for Reserve<'_> {
    type Output = Result<SlotReservation, CodexError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let owner = self.registry.clone();
        // Pressure precedes registry locking in both pools; admission observes
        // the latest account deadline without coupling their capacity counts.
        let pressure = match owner.pressure.lock() {
            Ok(pressure) => pressure,
            Err(_) => return Poll::Ready(Err(CodexError::RegistryPoisoned)),
        };
        let mut registry = match owner.inner.lock() {
            Ok(registry) => registry,
            Err(_) => return Poll::Ready(Err(CodexError::RegistryPoisoned)),
        };
        let capacity = if owner.adaptive {
            self.limit.min(
                pressure
                    .as_ref()
                    .map_or(owner.capacity, |state| state.limit),
            )
        } else {
            self.limit
        };
        let retry_at = pressure
            .as_ref()
            .filter(|state| state.retry_at > tokio::time::Instant::now())
            .map(|state| state.retry_at);
        let position = registry
            .waiting
            .iter()
            .position(|waiter| Arc::ptr_eq(&waiter.identity, &self.identity));
        if retry_at.is_none()
            && registry.occupied() < capacity
            && registry
                .waiting
                .front()
                .is_none_or(|waiter| Arc::ptr_eq(&waiter.identity, &self.identity))
        {
            if position == Some(0) {
                registry.waiting.pop_front();
            }
            registry.reserved += 1;
            // More than one slot may have freed before the head was polled.
            let wake = registry.wake_waiters();
            drop(registry);
            drop(pressure);
            for waker in wake {
                waker.wake();
            }
            return Poll::Ready(Ok(SlotReservation {
                registry: Some(self.registry.clone()),
            }));
        }
        if let Some(position) = position {
            if let Some(waiter) = registry.waiting.get_mut(position) {
                waiter.waker.clone_from(context.waker());
            }
        } else {
            if registry.waiting.len() >= MAX_WAITING {
                return Poll::Ready(Err(CodexError::AttemptWaiterCapacity {
                    max_waiters: MAX_WAITING,
                }));
            }
            registry.waiting.push_back(Waiter {
                identity: self.identity.clone(),
                waker: context.waker().clone(),
            });
        }
        let position = position.map_or(registry.waiting.len(), |index| index + 1);
        let progress = registry.progress;
        drop(registry);
        drop(pressure);
        if self.observed != Some((position, progress)) {
            let progressed = self
                .observed
                .is_some_and(|(_, previous)| previous != progress);
            self.observed = Some((position, progress));
            if let Some(observer) = &mut self.observer {
                observer(position, progressed);
            }
        }
        if let Some(deadline) = retry_at {
            let timer = self
                .cooldown
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
            if timer.deadline() != deadline {
                timer.as_mut().reset(deadline);
            }
            if timer.as_mut().poll(context).is_ready() {
                context.waker().wake_by_ref();
            }
        } else {
            self.cooldown = None;
        }
        Poll::Pending
    }
}

impl Drop for Reserve<'_> {
    fn drop(&mut self) {
        let next = match self.registry.inner.lock() {
            Ok(mut registry) => {
                if let Some(position) = registry
                    .waiting
                    .iter()
                    .position(|waiter| Arc::ptr_eq(&waiter.identity, &self.identity))
                {
                    registry.waiting.remove(position);
                }
                registry.wake_waiters()
            }
            Err(_) => return,
        };
        for waker in next {
            waker.wake();
        }
    }
}

/// A slot held for one attempt not yet staged. Staging turns it into the
/// attempt's [`AttemptSlot`]; dropped unused, it frees the slot.
#[derive(Debug)]
pub(crate) struct SlotReservation {
    registry: Option<ActiveAttempts>,
}

impl SlotReservation {
    pub(super) fn belongs_to(&self, owner: &ActiveAttempts) -> bool {
        self.registry
            .as_ref()
            .is_some_and(|registry| Arc::ptr_eq(&registry.inner, &owner.inner))
    }

    /// Hand the held slot to a claim on `owner`: true when it was held there.
    /// One held on another registry is released to it instead.
    fn consume_for(mut self, owner: &ActiveAttempts) -> bool {
        match &self.registry {
            Some(registry) if Arc::ptr_eq(&registry.inner, &owner.inner) => {
                self.registry = None;
                true
            }
            _ => false,
        }
    }
}

impl Drop for SlotReservation {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.take() {
            registry.release(None);
        }
    }
}

/// A claimed slot. Releasing is this guard's `Drop`, so success, error and
/// cancellation all free it without a separate code path to forget.
#[derive(Debug)]
pub(crate) struct AttemptSlot {
    registry: ActiveAttempts,
    attempt: PhysicalAttemptId,
    feedback: Option<ActiveAttempts>,
}

impl AttemptSlot {
    pub(super) fn with_feedback(mut self, children: ActiveAttempts) -> Self {
        self.feedback = Some(children);
        self
    }

    pub(super) async fn wait_for_cooldown(&self) -> Result<(), CodexError> {
        self.registry.wait_for_cooldown().await
    }

    pub(super) fn rate_limited(&self, after: Option<std::time::Duration>) {
        if let Some(registry) = &self.feedback {
            registry.rate_limited(after);
        }
    }

    pub(super) fn successful(&self) {
        if let Some(registry) = &self.feedback {
            registry.successful();
        }
    }

    pub(super) fn progress(&self) {
        self.registry.progress();
    }
}

impl Drop for AttemptSlot {
    fn drop(&mut self) {
        self.registry.release(Some(&self.attempt));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn attempt(id: &str) -> PhysicalAttemptId {
        match PhysicalAttemptId::new(id) {
            Some(attempt) => attempt,
            None => panic!("test attempt id {id}"),
        }
    }

    async fn pending(reserve: &mut Reserve<'_>) -> bool {
        tokio::time::timeout(Duration::from_millis(50), reserve)
            .await
            .is_err()
    }

    #[tokio::test]
    async fn a_reservation_waits_for_a_streaming_slot_to_end() {
        let registry = ActiveAttempts::new(1);
        let streaming = match registry.claim(&attempt("att_streaming"), None) {
            Ok(slot) => slot,
            Err(error) => panic!("first claim: {error}"),
        };
        let mut reserve = registry.reserve();
        assert!(pending(&mut reserve).await, "reserved past a full adapter");
        drop(streaming);
        let held = match tokio::time::timeout(Duration::from_secs(5), reserve).await {
            Ok(Ok(held)) => held,
            other => panic!("the freed slot was not reserved: {other:?}"),
        };
        assert!(matches!(
            registry.claim(&attempt("att_unreserved"), None),
            Err(CodexError::ActiveAttemptCapacity { capacity: 1 })
        ));
        assert!(registry.claim(&attempt("att_reserved"), Some(held)).is_ok());
    }

    #[tokio::test]
    async fn an_unused_reservation_frees_its_slot() {
        let registry = ActiveAttempts::new(1);
        let held = match registry.reserve().await {
            Ok(held) => held,
            Err(error) => panic!("reserve: {error}"),
        };
        let mut waiting = registry.reserve();
        assert!(pending(&mut waiting).await);
        drop(held);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), waiting).await,
            Ok(Ok(_))
        ));
    }

    #[tokio::test]
    async fn a_refused_reserved_claim_frees_its_slot() {
        let registry = ActiveAttempts::new(2);
        let _streaming = match registry.claim(&attempt("att_twice"), None) {
            Ok(slot) => slot,
            Err(error) => panic!("first claim: {error}"),
        };
        let held = match registry.reserve().await {
            Ok(held) => held,
            Err(error) => panic!("reserve: {error}"),
        };
        assert!(matches!(
            registry.claim(&attempt("att_twice"), Some(held)),
            Err(CodexError::DuplicateActiveAttempt { .. })
        ));
        assert!(registry.claim(&attempt("att_other"), None).is_ok());
    }

    #[tokio::test]
    async fn a_reservation_held_on_another_adapter_is_not_this_ones_slot() {
        let (registry, other) = (ActiveAttempts::new(1), ActiveAttempts::new(1));
        let _streaming = match registry.claim(&attempt("att_here"), None) {
            Ok(slot) => slot,
            Err(error) => panic!("first claim: {error}"),
        };
        let foreign = match other.reserve().await {
            Ok(held) => held,
            Err(error) => panic!("reserve: {error}"),
        };
        assert!(matches!(
            registry.claim(&attempt("att_foreign"), Some(foreign)),
            Err(CodexError::ActiveAttemptCapacity { capacity: 1 })
        ));
        // Refused here, the foreign place went back to its own adapter.
        assert!(other.claim(&attempt("att_there"), None).is_ok());
    }

    fn poll_reserve(reserve: &mut Reserve<'_>) -> Poll<Result<SlotReservation, CodexError>> {
        Pin::new(reserve).poll(&mut Context::from_waker(Waker::noop()))
    }

    fn ready(reserve: &mut Reserve<'_>) -> SlotReservation {
        match poll_reserve(reserve) {
            Poll::Ready(Ok(held)) => held,
            other => panic!("expected the next queued reservation: {other:?}"),
        }
    }

    #[test]
    fn three_waiters_keep_their_order_when_a_later_task_is_polled_first() {
        let registry = ActiveAttempts::new(1);
        let streaming = match registry.claim(&attempt("att_parent"), None) {
            Ok(slot) => slot,
            Err(error) => panic!("parent claim: {error}"),
        };
        let (mut first, mut second, mut third) =
            (registry.reserve(), registry.reserve(), registry.reserve());
        for waiting in [&mut first, &mut second, &mut third] {
            assert!(poll_reserve(waiting).is_pending());
        }
        drop(streaming);
        assert!(
            poll_reserve(&mut third).is_pending(),
            "third child overtook the queue"
        );
        assert!(
            poll_reserve(&mut second).is_pending(),
            "second child overtook the queue"
        );
        let first = ready(&mut first);
        assert!(matches!(
            registry.claim(&attempt("att_barging"), None),
            Err(CodexError::ActiveAttemptCapacity { capacity: 1 })
        ));
        drop(first);
        assert!(poll_reserve(&mut third).is_pending());
        drop(ready(&mut second));
        drop(ready(&mut third));
        assert!(registry.claim(&attempt("att_after_children"), None).is_ok());
    }

    #[test]
    fn positions_follow_cancellation_but_only_holder_progress_resets_inactivity() {
        let registry = ActiveAttempts::new(1);
        let held = ready(&mut registry.reserve());
        let mut first = registry.reserve();
        assert!(poll_reserve(&mut first).is_pending());
        let observations = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = observations.clone();
        let mut second = registry.reserve_observed(Some(Box::new(move |position, progress| {
            match observed.lock() {
                Ok(mut observed) => observed.push((position, progress)),
                Err(_) => panic!("test observations"),
            }
        })));
        assert!(poll_reserve(&mut second).is_pending());
        drop(first);
        assert!(poll_reserve(&mut second).is_pending());
        registry.progress();
        assert!(poll_reserve(&mut second).is_pending());
        let actual = match observations.lock() {
            Ok(observed) => observed.clone(),
            Err(_) => panic!("test observations"),
        };
        assert_eq!(actual, [(2, false), (1, false), (1, true)]);
        drop(held);
        drop(ready(&mut second));
    }

    #[test]
    fn a_releasing_session_rejoins_behind_every_older_waiter() {
        let registry = ActiveAttempts::new(1);
        let mut holder = ready(&mut registry.reserve());
        let mut child = registry.reserve();
        let mut grandchild = registry.reserve();
        assert!(poll_reserve(&mut child).is_pending());
        assert!(poll_reserve(&mut grandchild).is_pending());
        for _ in 0..8 {
            drop(holder);
            let mut root = registry.reserve();
            assert!(poll_reserve(&mut root).is_pending());
            let child_slot = ready(&mut child);
            child = registry.reserve();
            assert!(poll_reserve(&mut child).is_pending());
            drop(child_slot);
            assert!(poll_reserve(&mut child).is_pending());
            let grandchild_slot = ready(&mut grandchild);
            grandchild = registry.reserve();
            assert!(poll_reserve(&mut grandchild).is_pending());
            drop(grandchild_slot);
            assert!(poll_reserve(&mut grandchild).is_pending());
            holder = ready(&mut root);
        }
    }

    #[test]
    fn cancelling_waiters_removes_their_places_without_blocking_successors() {
        let registry = ActiveAttempts::new(1);
        let held = ready(&mut registry.reserve());
        let (mut cancelled, mut next) = (registry.reserve(), registry.reserve());
        assert!(poll_reserve(&mut cancelled).is_pending());
        assert!(poll_reserve(&mut next).is_pending());
        drop(cancelled);
        for _ in 0..MAX_WAITING * 2 {
            let mut cancelled = registry.reserve();
            assert!(poll_reserve(&mut cancelled).is_pending());
        }
        drop(held);
        drop(ready(&mut next));
        assert!(registry.claim(&attempt("att_after_cancel"), None).is_ok());
    }

    #[test]
    fn the_waiting_queue_is_bounded_even_when_all_futures_share_a_waker() {
        let registry = ActiveAttempts::new(1);
        let _held = ready(&mut registry.reserve());
        let mut waiting: Vec<_> = (0..MAX_WAITING).map(|_| registry.reserve()).collect();
        for next in &mut waiting {
            assert!(poll_reserve(next).is_pending());
        }
        assert!(matches!(
            poll_reserve(&mut registry.reserve()),
            Poll::Ready(Err(CodexError::AttemptWaiterCapacity {
                max_waiters: MAX_WAITING
            }))
        ));
    }
    #[tokio::test(start_paused = true)]
    async fn rate_limit_storm_cools_down_then_recovers_the_child_pool() {
        let children = ActiveAttempts::new(3);
        for _ in 0..3 {
            children.rate_limited(None);
        }
        let mut first = children.reserve();
        assert!(
            poll_reserve(&mut first).is_pending(),
            "cooldown admitted a child"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let first = ready(&mut first);
        let mut second = children.reserve();
        assert!(
            poll_reserve(&mut second).is_pending(),
            "rate limit did not shrink concurrency"
        );
        children.successful();
        let second = ready(&mut second);
        let mut third = children.reserve();
        assert!(
            poll_reserve(&mut third).is_pending(),
            "one success restored too much capacity"
        );
        children.successful();
        let third = ready(&mut third);
        drop((first, second, third));
    }

    #[tokio::test(start_paused = true)]
    async fn provider_retry_after_keeps_admission_closed_until_its_bound() {
        let children = ActiveAttempts::new(3);
        children.rate_limited(Some(Duration::from_secs(5)));
        let mut first = children.reserve();
        assert!(poll_reserve(&mut first).is_pending());
        tokio::time::advance(Duration::from_secs(4)).await;
        children.successful();
        assert!(
            poll_reserve(&mut first).is_pending(),
            "success erased provider backoff"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        drop(ready(&mut first));
    }

    #[tokio::test(start_paused = true)]
    async fn later_rate_limit_cannot_shorten_an_existing_provider_backoff() {
        let children = ActiveAttempts::new(3);
        children.rate_limited(Some(Duration::from_secs(5)));
        tokio::time::advance(Duration::from_secs(1)).await;
        children.rate_limited(None);
        let mut first = children.reserve();
        assert!(poll_reserve(&mut first).is_pending());
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(
            poll_reserve(&mut first).is_pending(),
            "a shorter rate limit erased the existing Retry-After"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        drop(ready(&mut first));
    }
}
