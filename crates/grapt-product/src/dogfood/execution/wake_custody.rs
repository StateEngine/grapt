//! Serialize product principal lifetime while the runtime retains all decisions
//! about promotion, Step boundaries and cancellation of active effects.
use super::super::prompt_driver::Started;
use grapt_types::{ExecutionId, SessionId};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{watch, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

const PER_SESSION: usize = 4;

/// How long a stop waits for admitted owners and their settlement to end.
const DRAIN_BOUND: Duration = Duration::from_secs(15);

fn drain_timeout() -> String {
    format!(
        "Session stop could not confirm execution-owner completion within {} seconds",
        DRAIN_BOUND.as_secs()
    )
}

#[derive(Debug, Default)]
pub(in crate::dogfood) struct WakeSlots(Mutex<HashMap<SlotKey, Weak<Slot>>>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SlotKey {
    Session(SessionId),
    Workspace(PathBuf),
}

pub(in crate::dogfood) fn workspace_key(root: &Path) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    {
        Ok(PathBuf::from(
            root.to_str()
                .ok_or("workspace is not UTF-8")?
                .to_lowercase(),
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(root)
    }
}

#[derive(Debug)]
struct Slot {
    custody: Arc<tokio::sync::Mutex<()>>,
    capacity: Arc<Semaphore>,
    /// Signalled when the last admitted owner ends. A running permit cannot
    /// witness that: a parked owner holds none.
    owners_settled: tokio::sync::Notify,
    state: Mutex<SlotState>,
}

#[derive(Debug, Default)]
struct SlotState {
    stopping: bool,
    stop: Option<StopFlight>,
    waiters: Vec<Weak<watch::Sender<bool>>>,
    /// Admitted owners that have not ended: parked, running or settling. Owner
    /// lifecycle accounting, deliberately separate from `capacity`. A parked
    /// owner spends no running permit, so the semaphore alone cannot tell a
    /// stop whether every admitted owner has ended.
    owners: usize,
    /// A rewind or reconcile holds the Session idle: no admission, running
    /// permit, or catalog entry may exist until it ends.
    idle: bool,
}

/// One wake owner's lifecycle, from admission until the last handle retaining
/// it ends. Shared with `completion()`, so retained child and native
/// settlement keeps the owner visible to a stop after the reservation is gone.
#[derive(Debug)]
struct OwnerLife {
    slot: Arc<Slot>,
}

impl Drop for OwnerLife {
    fn drop(&mut self) {
        let mut state = self.slot.state.lock().unwrap_or_else(|p| p.into_inner());
        state.owners = state.owners.saturating_sub(1);
        if state.owners == 0 {
            // This runs under the state lock, so a drain that read the count
            // before this point cannot also miss the notification.
            self.slot.owners_settled.notify_waiters();
        }
    }
}

impl Slot {
    fn live_owners(&self) -> usize {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).owners
    }
}

/// How many owners may wait on one Session. A parked owner costs no running
/// permit, but its cancellation catalog is still a bounded resource. The
/// global prompt capacity bounds this too: an admitted prompt holds a prompt
/// permit from admission until its work ends, so one Session cannot queue more
/// work than the whole service may run.
const MAX_WAITING: usize = crate::dogfood::MAX_ACTIVE_EXECUTIONS;

#[derive(Debug)]
struct StopFlight {
    receipt: StopReceipt,
    execution: Option<ExecutionId>,
    processes: bool,
    resume_pending: bool,
}

pub(in crate::dogfood) struct WakeReservation {
    session: SessionId,
    slot: Arc<Slot>,
    /// Taken only when this owner actually enters custody, never while it
    /// waits for its predecessor: a parked owner is not running, so it must
    /// not spend the allowance that bounds running work. The cell is shared
    /// with `completion()`, so retained child and native settlement keeps the
    /// permit until the last holder ends.
    capacity: Arc<Mutex<Option<Arc<OwnedSemaphorePermit>>>>,
    _cancel: Arc<watch::Sender<bool>>,
    cancelled: watch::Receiver<bool>,
    _workspace: Option<(PathBuf, Arc<Slot>, OwnedSemaphorePermit)>,
    interrupt_wait: Option<Duration>,
    /// Counted from admission until the last holder ends; see [`OwnerLife`].
    _life: Arc<OwnerLife>,
}

pub(in crate::dogfood) struct IdleReservation {
    slot: Arc<Slot>,
    capacity: Option<OwnedSemaphorePermit>,
    #[cfg(test)]
    after_capacity_release: Option<Box<dyn FnOnce() + Send>>,
}

impl Drop for IdleReservation {
    fn drop(&mut self) {
        // Clear the flag and release the permits under one state-lock hold, so
        // no reservation observes either half of the handoff. Releasing the
        // permits first left a gap: a second idle reservation took them and set
        // `idle`, and this drop then cleared that reservation's flag, reopening
        // admission while it still expected exclusive custody. Every full-permit
        // acquisition and every `idle` read happens under this lock, so holding
        // it across both changes makes the handoff atomic.
        let mut state = self.slot.state.lock().unwrap_or_else(|p| p.into_inner());
        state.idle = false;
        drop(self.capacity.take());
        // The probe observes the handoff from inside it. It must find the state
        // lock held: an atomic handoff is exactly what a successor must not
        // be able to interrupt.
        #[cfg(test)]
        if let Some(probe) = self.after_capacity_release.take() {
            probe();
        }
        drop(state);
    }
}

/// Keeps admission closed until every admitted owner and its cleanup have ended.
pub(in crate::dogfood) struct StopReservation {
    slot: Arc<Slot>,
    completed: watch::Sender<Option<Result<(), String>>>,
    released: bool,
    outcome: Option<Result<(), String>>,
}

pub(in crate::dogfood) enum StopAdmission<T> {
    Owner(StopReservation, T),
    Joined(StopReceipt),
}

pub(in crate::dogfood) enum StopRelease {
    ProcessesRequested,
    Released(Option<WakeReservation>),
}

#[derive(Debug, Clone)]
pub(in crate::dogfood) struct StopReceipt {
    completed: watch::Receiver<Option<Result<(), String>>>,
}

impl StopReceipt {
    pub(in crate::dogfood) async fn wait(mut self) -> Result<(), String> {
        loop {
            if let Some(result) = self.completed.borrow().clone() {
                return result;
            }
            self.completed.changed().await.map_err(|_| {
                "Session stop owner ended without a completion receipt; retry stop".to_owned()
            })?;
        }
    }
}

pub(in crate::dogfood) struct WakeCompletion {
    _slot: Arc<Slot>,
    /// Shares the reservation's permit cell, so the permit lives until the
    /// last holder ends, including retained child and native settlement.
    _capacity: Arc<Mutex<Option<Arc<OwnedSemaphorePermit>>>>,
    /// Shares the reservation's lifecycle, so a stop sees this owner until the
    /// last retained handle ends, not only until the permit is released.
    _life: Arc<OwnerLife>,
}

impl StopReservation {
    pub(in crate::dogfood) fn receipt(&self) -> StopReceipt {
        StopReceipt {
            completed: self.completed.subscribe(),
        }
    }

    pub(in crate::dogfood) fn processes_requested(&self) -> bool {
        self.slot
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .stop
            .as_ref()
            .is_some_and(|flight| flight.processes)
    }

    /// Close receipt eligibility at the same boundary as reopening admission.
    /// A join's cleanup and resume intents belong to this driver until release.
    pub(in crate::dogfood) fn release_admission(
        &mut self,
        processes_stopped: bool,
        resume: Option<&SessionId>,
    ) -> Result<StopRelease, String> {
        let mut state = self.slot.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.stop.as_ref().is_some_and(|flight| flight.processes) && !processes_stopped {
            return Ok(StopRelease::ProcessesRequested);
        }
        let resume = resume.filter(|_| {
            state
                .stop
                .as_ref()
                .is_some_and(|flight| flight.resume_pending)
        });
        // A later stop must find and cancel this wake even if scheduling it
        // pauses after admission reopens.
        let wake = resume
            .map(|session| WakeSlots::reservation(session, Arc::clone(&self.slot), &mut state))
            .transpose()?;
        state.stop = None;
        state.stopping = false;
        self.released = true;
        Ok(StopRelease::Released(wake))
    }

    pub(in crate::dogfood) fn finish(mut self, result: Result<(), String>) {
        self.outcome = Some(result);
    }

    pub(in crate::dogfood) async fn drained(&self) -> Result<OwnedSemaphorePermit, String> {
        // Wait for owner lifecycle, not running permits. A parked or cancelled
        // owner holds no running permit, so the semaphore can be fully free
        // while such an owner (or its retained completion handle) is alive.
        // Reporting drained then would reopen admission and receipt the stop
        // before that cleanup settles.
        let deadline = tokio::time::Instant::now() + DRAIN_BOUND;
        let settled = async {
            loop {
                // Register the wake-up before reading the count: `notify_waiters`
                // only wakes waiters that are already registered, so an owner
                // dropping between the read and the await would otherwise be lost.
                let notified = self.slot.owners_settled.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.slot.live_owners() == 0 {
                    break;
                }
                notified.await;
            }
        };
        tokio::time::timeout_at(deadline, settled)
            .await
            .map_err(|_| drain_timeout())?;
        // Admission reopens holding every running permit, so no owner can start
        // between this drain and the release of admission.
        tokio::time::timeout_at(
            deadline,
            Arc::clone(&self.slot.capacity).acquire_many_owned(PER_SESSION as u32),
        )
        .await
        .map_err(|_| drain_timeout())?
        .map_err(|_| "Session execution-owner completion is unavailable".into())
    }
}

impl Drop for StopReservation {
    fn drop(&mut self) {
        if !self.released {
            let mut state = self.slot.state.lock().unwrap_or_else(|p| p.into_inner());
            state.stopping = false;
            state.stop = None;
        }
        self.completed
            .send_replace(Some(self.outcome.take().unwrap_or_else(|| {
                Err("Session stop owner ended without a completion receipt; retry stop".into())
            })));
    }
}

pub(in crate::dogfood) struct WorkspaceIdleReservation {
    root: PathBuf,
    _slot: Arc<Slot>,
    _capacity: OwnedSemaphorePermit,
}

impl WorkspaceIdleReservation {
    pub(in crate::dogfood) fn matches(&self, root: &Path) -> bool {
        workspace_key(root).is_ok_and(|root| root == self.root)
    }
}

pub(in crate::dogfood) struct WakeGuard {
    _custody: OwnedMutexGuard<()>,
    _reservation: WakeReservation,
}

impl WakeSlots {
    fn slot(&self, key: SlotKey) -> Result<Arc<Slot>, String> {
        let mut slots = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slots.retain(|_, slot| slot.strong_count() > 0);
        Ok(match slots.get(&key).and_then(Weak::upgrade) {
            Some(slot) => slot,
            None => {
                if slots.len() >= 2 * crate::dogfood::MAX_ACTIVE_EXECUTIONS {
                    return Err("the Session wake capacity is unavailable".into());
                }
                let slot = Arc::new(Slot {
                    custody: Arc::new(tokio::sync::Mutex::new(())),
                    capacity: Arc::new(Semaphore::new(match &key {
                        SlotKey::Session(_) => PER_SESSION,
                        SlotKey::Workspace(_) => crate::dogfood::MAX_ACTIVE_EXECUTIONS,
                    })),
                    owners_settled: tokio::sync::Notify::new(),
                    state: Mutex::new(SlotState::default()),
                });
                slots.insert(key, Arc::downgrade(&slot));
                slot
            }
        })
    }

    /// No active, waiting or newly admitted wake can coexist with this
    /// reservation. Holding the slot itself keeps the Session's identity
    /// stable in the weak catalog. Parking costs no running permit, so the
    /// catalog is what excludes a rewind from a burst of waiting owners.
    pub(in crate::dogfood) fn reserve_idle(
        &self,
        session: &SessionId,
    ) -> Result<IdleReservation, String> {
        let slot = self.slot(SlotKey::Session(session.clone()))?;
        let mut state = slot.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.stopping {
            return Err("Session stop is in progress; wait for its completion".into());
        }
        state.waiters.retain(|waiter| waiter.strong_count() > 0);
        if !state.waiters.is_empty() {
            return Err("the Session has an admitted wake; wait for it before rewinding".into());
        }
        let capacity = Arc::clone(&slot.capacity)
            .try_acquire_many_owned(PER_SESSION as u32)
            .map_err(|_| "the Session has an admitted wake; wait for it before rewinding")?;
        state.idle = true;
        drop(state);
        Ok(IdleReservation {
            slot,
            capacity: Some(capacity),
            #[cfg(test)]
            after_capacity_release: None,
        })
    }

    pub(in crate::dogfood) fn reserve_stop<T>(
        &self,
        session: &SessionId,
        processes: bool,
        resume_pending: bool,
        expected: Option<&ExecutionId>,
        interrupt: impl FnOnce() -> Result<(Option<ExecutionId>, T), String>,
    ) -> Result<StopAdmission<T>, String> {
        let slot = self.slot(SlotKey::Session(session.clone()))?;
        let mut state = slot.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(flight) = &mut state.stop {
            if expected.is_some_and(|expected| Some(expected) != flight.execution.as_ref()) {
                return Err("the active execution identity changed".into());
            }
            flight.processes |= processes;
            // False omits an intent; it does not veto another caller's request.
            flight.resume_pending |= resume_pending;
            return Ok(StopAdmission::Joined(flight.receipt.clone()));
        }
        let (execution, control) = interrupt()?;
        let (completed, response) = watch::channel(None);
        state.stopping = true;
        state.stop = Some(StopFlight {
            receipt: StopReceipt {
                completed: response,
            },
            execution,
            processes,
            resume_pending,
        });
        drop(state);
        Ok(StopAdmission::Owner(
            StopReservation {
                slot,
                completed,
                released: false,
                outcome: None,
            },
            control,
        ))
    }

    pub(in crate::dogfood) fn reserve(
        &self,
        session: &SessionId,
    ) -> Result<WakeReservation, String> {
        let slot = self.slot(SlotKey::Session(session.clone()))?;
        let mut state = slot.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.stopping {
            return Err("Session stop is in progress; retry the prompt after it completes".into());
        }
        Self::reservation(session, Arc::clone(&slot), &mut state)
    }

    fn reservation(
        session: &SessionId,
        slot: Arc<Slot>,
        state: &mut SlotState,
    ) -> Result<WakeReservation, String> {
        if state.idle {
            return Err(
                "the Session is busy with a rewind or reconcile; retry the prompt after it completes"
                    .into(),
            );
        }
        let (cancel, cancelled) = watch::channel(false);
        let cancel = Arc::new(cancel);
        {
            state.waiters.retain(|waiter| waiter.strong_count() > 0);
            if state.waiters.len() >= MAX_WAITING {
                return Err("Session waiter custody is unavailable".into());
            }
            state.waiters.push(Arc::downgrade(&cancel));
            // Counted before the reservation exists, so a stop that observes
            // this owner can wait for it to end. Nothing can drop it before
            // `OwnerLife` is constructed below.
            state.owners += 1;
        }
        Ok(WakeReservation {
            session: session.clone(),
            slot: Arc::clone(&slot),
            capacity: Arc::new(Mutex::new(None)),
            _cancel: cancel,
            cancelled,
            _workspace: None,
            interrupt_wait: None,
            _life: Arc::new(OwnerLife { slot }),
        })
    }

    pub(in crate::dogfood) fn reserve_for_workspace(
        &self,
        session: &SessionId,
        root: &Path,
    ) -> Result<WakeReservation, String> {
        self.bind_reservation(self.reserve(session)?, root)
    }

    pub(in crate::dogfood) fn bind_reservation(
        &self,
        mut wake: WakeReservation,
        root: &Path,
    ) -> Result<WakeReservation, String> {
        // Preserve admission of an unreadable directory: normal preflight
        // reports it as not-run-yet before promotion. Every live wake must
        // bind the canonical root again at that preflight boundary.
        if let Ok(root) = workspace_key(root) {
            self.bind_workspace(&mut wake, root)?;
        }
        Ok(wake)
    }

    fn bind_workspace(&self, wake: &mut WakeReservation, root: PathBuf) -> Result<(), String> {
        if wake
            ._workspace
            .as_ref()
            .is_some_and(|(held, _, _)| held == &root)
        {
            return Ok(());
        }
        let (slot, permit) = self.workspace_capacity(&root, false)?;
        // Acquire the new identity before releasing the prior identity.
        wake._workspace = Some((root, slot, permit));
        Ok(())
    }

    pub(in crate::dogfood) fn reserve_workspace_idle(
        &self,
        root: &Path,
    ) -> Result<WorkspaceIdleReservation, String> {
        let root = workspace_key(root)?;
        let (slot, capacity) = self.workspace_capacity(&root, true)?;
        Ok(WorkspaceIdleReservation {
            root,
            _slot: slot,
            _capacity: capacity,
        })
    }

    /// The catalog lock covers overlap inspection AND permit acquisition, so
    /// a newly admitted nested workspace cannot race an exclusive parent root.
    fn workspace_capacity(
        &self,
        root: &Path,
        idle: bool,
    ) -> Result<(Arc<Slot>, OwnedSemaphorePermit), String> {
        let max = crate::dogfood::MAX_ACTIVE_EXECUTIONS;
        let mut slots = self
            .0
            .lock()
            .map_err(|_| "workspace wake owner is unavailable")?;
        slots.retain(|_, slot| slot.strong_count() > 0);
        for (key, slot) in slots.iter() {
            let SlotKey::Workspace(other) = key else {
                continue;
            };
            if !root.starts_with(other) && !other.starts_with(root) {
                continue;
            }
            if let Some(slot) = slot.upgrade() {
                let available = slot.capacity.available_permits();
                if (idle && available < max) || (!idle && available == 0) {
                    return Err("this workspace or a nested workspace has admitted work or file undo in progress".into());
                }
            }
        }
        let key = SlotKey::Workspace(root.to_path_buf());
        let slot = match slots.get(&key).and_then(Weak::upgrade) {
            Some(slot) => slot,
            None => {
                if slots.len() >= 2 * max {
                    return Err("workspace wake capacity is unavailable".into());
                }
                let slot = Arc::new(Slot {
                    custody: Arc::new(tokio::sync::Mutex::new(())),
                    capacity: Arc::new(Semaphore::new(max)),
                    owners_settled: tokio::sync::Notify::new(),
                    state: Mutex::new(SlotState::default()),
                });
                slots.insert(key, Arc::downgrade(&slot));
                slot
            }
        };
        let count = if idle { max } else { 1 };
        let permit = Arc::clone(&slot.capacity)
            .try_acquire_many_owned(count as u32)
            .map_err(|_| "workspace wake capacity is unavailable")?;
        Ok((slot, permit))
    }

    pub(in crate::dogfood) fn cancel_waiters(&self, session: &SessionId) {
        let slots = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(slot) = slots
            .get(&SlotKey::Session(session.clone()))
            .and_then(Weak::upgrade)
        {
            let state = slot
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for waiter in state.waiters.iter().filter_map(Weak::upgrade) {
                waiter.send_replace(true);
            }
        }
    }
}

impl WakeGuard {
    pub(super) fn cancellation(&self) -> watch::Receiver<bool> {
        self._reservation.cancellation()
    }
    pub(in crate::dogfood) fn require_session(&self, session: &SessionId) -> Result<(), String> {
        if self._reservation.session != *session {
            return Err("wake custody belongs to a different Session".into());
        }
        Ok(())
    }

    /// Bind the final preflight directory while this guard still owns the
    /// entire wake. A changed alias cannot bypass an idle restore owner.
    pub(super) fn bind_workspace(
        &mut self,
        slots: &WakeSlots,
        root: &Path,
    ) -> Result<PathBuf, String> {
        let root = workspace_key(root)?;
        slots.bind_workspace(&mut self._reservation, root.clone())?;
        Ok(root)
    }
}

impl WakeReservation {
    /// Retain admission for nested helpers without acquiring execution's mutex.
    /// The helper itself is running work, so it takes the running allowance for
    /// its own lifetime and keeps a rewind out.
    pub(in crate::dogfood) async fn run_activity<T>(
        mut self,
        future: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let capacity = self.take_capacity()?;
        let result = {
            tokio::pin!(future);
            if *self.cancelled.borrow() {
                Err("Session activity was cancelled".to_owned())
            } else {
                tokio::select! {
                    biased;
                    _ = self.cancelled.changed() => Err("Session activity was cancelled".to_owned()),
                    result = &mut future => result,
                }
            }
        };
        // Drop the helper future before releasing its admission permit.
        drop(capacity);
        drop(self);
        result
    }

    /// Take the Session's running allowance for custody this owner has won.
    /// A refused acquisition leaves the wake queued instead of running.
    fn take_capacity(&self) -> Result<Arc<OwnedSemaphorePermit>, String> {
        let mut held = self
            .capacity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(capacity) = held.as_ref() {
            return Ok(Arc::clone(capacity));
        }
        let capacity = Arc::new(
            Arc::clone(&self.slot.capacity)
                .try_acquire_owned()
                .map_err(|_| "this Session already has four running wake owners".to_owned())?,
        );
        *held = Some(Arc::clone(&capacity));
        Ok(capacity)
    }

    /// The outer service-owned task retains this receipt through child/native settlement.
    pub(in crate::dogfood) fn completion(&self) -> WakeCompletion {
        WakeCompletion {
            _slot: Arc::clone(&self.slot),
            _capacity: Arc::clone(&self.capacity),
            _life: Arc::clone(&self._life),
        }
    }
    pub(super) fn cancelled_result(restarting: bool) -> Result<Option<WakeGuard>, String> {
        if restarting {
            Err("Restart cancelled before execution custody was acquired; the message remains queued.".into())
        } else {
            Ok(None)
        }
    }

    /// The owned wake survives an HTTP disconnect, but cannot acknowledge a
    /// queued-message restart until the interrupted owner has released custody.
    pub(in crate::dogfood) fn after_interrupt(mut self) -> Self {
        self.interrupt_wait = Some(Duration::from_secs(10));
        self
    }

    pub(in crate::dogfood) fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancelled.clone()
    }

    pub(in crate::dogfood) async fn enter(
        mut self,
        started: &Started,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<Option<WakeGuard>, String> {
        if *self.cancelled.borrow() || *shutdown.borrow() {
            return Self::cancelled_result(self.interrupt_wait.is_some());
        }
        // Tokio's FIFO mutex preserves order among waiting custody owners.
        let custody = match Arc::clone(&self.slot.custody).try_lock_owned() {
            Ok(custody) => custody,
            Err(_) => {
                // The response acknowledges the committed INPUT, not an
                // ExecutionId or a promise to make another provider call.
                if self.interrupt_wait.is_none() {
                    started.settle(Ok(()));
                }
                let acquire = async {
                    let lock = Arc::clone(&self.slot.custody).lock_owned();
                    match self.interrupt_wait {
                        Some(bound) => tokio::time::timeout(bound, lock).await.map_err(|_| {
                            "Interrupted execution has not released custody; the message remains queued.".to_owned()
                        }),
                        None => Ok(lock.await),
                    }
                };
                tokio::select! {
                    biased;
                    _ = self.cancelled.changed() => return Self::cancelled_result(self.interrupt_wait.is_some()),
                    _ = shutdown.changed() => return Self::cancelled_result(self.interrupt_wait.is_some()),
                    custody = acquire => custody?,
                }
            }
        };
        // Cancellation may have raced the successful lock acquisition.
        if *self.cancelled.borrow() || *shutdown.borrow() {
            return Self::cancelled_result(self.interrupt_wait.is_some());
        }
        // Custody is won, so this owner is the Session's one running wake and
        // takes the running allowance only now. An owner parked behind a
        // running turn never spends it, so a burst of rapid submissions is
        // admitted and runs in order instead of being refused at four.
        self.take_capacity()?;
        Ok(Some(WakeGuard {
            _custody: custody,
            _reservation: self,
        }))
    }
}

/// Followup acquires the existing Session custody before its atomic run claim.
/// Passing that same guard onward avoids a second acquisition of its own mutex.
pub(in crate::dogfood) enum WakeCustody {
    Reserved(WakeReservation),
    Acquired(WakeGuard),
}

impl WakeCustody {
    pub(super) fn restarting(&self) -> bool {
        match self {
            Self::Reserved(wake) => wake.interrupt_wait.is_some(),
            Self::Acquired(wake) => wake._reservation.interrupt_wait.is_some(),
        }
    }
    pub(super) fn cancellation(&self) -> watch::Receiver<bool> {
        match self {
            Self::Reserved(wake) => wake.cancellation(),
            Self::Acquired(wake) => wake.cancellation(),
        }
    }
    pub(super) async fn enter(
        self,
        session: &SessionId,
        started: &Started,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Option<WakeGuard>, String> {
        let guard = match self {
            Self::Reserved(reservation) => reservation.enter(started, shutdown.clone()).await?,
            Self::Acquired(guard) => Some(guard),
        };
        let Some(guard) = guard else {
            return Ok(None);
        };
        guard.require_session(session)?;
        if *guard._reservation.cancelled.borrow() || *shutdown.borrow() {
            return WakeReservation::cancelled_result(guard._reservation.interrupt_wait.is_some());
        }
        Ok(Some(guard))
    }
}

#[cfg(test)]
#[path = "wake_custody/interrupt_tests.rs"]
mod interrupt_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquired_followup_custody_cannot_run_another_session() -> Result<(), String> {
        let slots = WakeSlots::default();
        let session: SessionId = "ses_followup_owner"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let other: SessionId = "ses_followup_foreign"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let (started, _response) = Started::channel();
        let (_shutdown, stopped) = watch::channel(false);
        let guard = slots
            .reserve(&session)?
            .enter(&started, stopped.clone())
            .await?
            .ok_or("fixture wake was cancelled")?;
        assert!(guard.require_session(&other).is_err());
        assert!(WakeCustody::Acquired(guard)
            .enter(&other, &started, stopped)
            .await
            .is_err());
        assert!(
            slots.reserve_idle(&session).is_ok(),
            "refused custody retained its permit"
        );
        Ok(())
    }

    #[tokio::test]
    async fn rewind_reservation_excludes_active_and_admitted_wakes() -> Result<(), String> {
        let slots = WakeSlots::default();
        let session: SessionId = "ses_rewind_idle"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let other: SessionId = "ses_rewind_other"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let idle = slots.reserve_idle(&session)?;
        assert!(slots.reserve(&session).is_err());
        assert!(slots.reserve_idle(&session).is_err());
        drop(idle);
        let owner = slots.reserve(&session)?;
        assert!(slots.reserve_idle(&session).is_err());
        let waiter = slots.reserve(&session)?;
        let (started, _response) = Started::channel();
        let (_shutdown, stopped) = watch::channel(false);
        let owner = owner
            .enter(&started, stopped.clone())
            .await?
            .ok_or("owner was cancelled")?;
        assert!(slots.reserve_idle(&session).is_err());
        slots.cancel_waiters(&session);
        assert!(waiter.enter(&started, stopped).await?.is_none());
        drop(owner);
        let _idle = slots.reserve_idle(&session)?;
        let _other = slots.reserve(&other)?;
        Ok(())
    }

    #[tokio::test]
    async fn a_session_bounds_running_work_while_a_burst_waits_and_cancelled_waiters_never_restart(
    ) -> Result<(), String> {
        let slots = WakeSlots::default();
        let session: SessionId = "ses_wake"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let other: SessionId = "ses_other"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        let (started, _response) = Started::channel();
        let (shutdown, stopped) = watch::channel(false);
        let owner = slots
            .reserve(&session)?
            .enter(&started, stopped.clone())
            .await?
            .ok_or("owner was cancelled")?;
        let waiter = slots.reserve(&session)?;
        // Parked owners hold no running permit, so a burst is admitted rather
        // than refused at four; running work stays bounded at four.
        let held = (0..8)
            .map(|_| slots.reserve(&session))
            .collect::<Result<Vec<_>, _>>()?;
        let running = (0..PER_SESSION - 1)
            .map(|_| slots.reserve(&session))
            .collect::<Result<Vec<_>, _>>()?;
        for reservation in &running {
            reservation.take_capacity()?;
        }
        let overflow = slots.reserve(&session)?;
        assert!(
            overflow.take_capacity().is_err(),
            "running work exceeded the Session's running allowance"
        );
        drop(running);
        let _other = slots.reserve(&other)?;
        slots.cancel_waiters(&session);
        assert!(
            waiter.enter(&started, stopped.clone()).await?.is_none(),
            "interrupted waiter started a successor"
        );
        drop(held);
        // A new explicit input after interruption has fresh cancellation
        // custody, but still cannot overlap the old owner's principal guard.
        let replacement = slots.reserve(&session)?;
        let (waiting, accepted) = Started::channel();
        let (entered, release) = tokio::join!(replacement.enter(&waiting, stopped), async {
            accepted
                .await
                .map_err(|_| "waiter did not acknowledge admission")??;
            shutdown.send_replace(true);
            Ok::<(), String>(())
        });
        release?;
        assert!(
            entered?.is_none(),
            "shutdown waited for custody instead of cancelling immediately"
        );
        drop(owner);
        Ok(())
    }
}
