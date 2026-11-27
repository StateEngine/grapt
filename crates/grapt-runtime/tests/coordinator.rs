//! R3-c acceptance witnesses — process-local Session execution coordinator.
//!
//! # Real cut barriers, not sleeps
//!
//! Every rendezvous is a `Semaphore`, which cannot lose a signal the way a
//! `Notify` can if the waiter has not registered yet. A hook adds a permit when
//! it is REACHED and then blocks until the test adds a permit to RELEASE it, so
//! the test observes an exact cut in the coordinator's state machine rather
//! than sleeping and hoping. An earlier version of this file used 50ms sleeps
//! and poll loops while claiming to be deterministic; it was not, and the races
//! it was supposed to pin were exactly where the bugs were.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use grapt_runtime::{
    Boundary, BoxFuture, Cancellation, CoordinatorError, InterruptOutcome, Placement, Promotion,
    RunOutcome, SessionApplication, SessionCoordinator, WakeOutcome,
};
use grapt_types::{DeliveryMode, LocationId, MessageId, PendingInputId, SessionId};
use tokio::sync::Semaphore;

const DEADLINE: Duration = Duration::from_secs(5);

fn session(id: &str) -> SessionId {
    match format!("ses_{id}").parse::<SessionId>() {
        Ok(session) => session,
        Err(error) => panic!("session id must parse: {error}"),
    }
}

fn location(id: &str) -> LocationId {
    match LocationId::new(id) {
        Some(location) => location,
        None => panic!("location id must be non-empty"),
    }
}

/// A pending input's identity IS its message identity.
fn input(id: &str) -> PendingInputId {
    match format!("msg_{id}").parse::<MessageId>() {
        Ok(message) => PendingInputId::new(message),
        Err(error) => panic!("message id must parse: {error}"),
    }
}

/// A one-directional rendezvous that cannot lose a signal.
#[derive(Debug, Clone)]
struct Gate {
    reached: Arc<Semaphore>,
    release: Arc<Semaphore>,
}

impl Gate {
    fn new() -> Self {
        Self {
            reached: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
        }
    }

    /// Called from inside the coordinator's call path.
    async fn arrive(&self) {
        self.reached.add_permits(1);
        match self.release.acquire().await {
            Ok(permit) => permit.forget(),
            Err(_) => panic!("gate release closed"),
        }
    }

    /// Called from the test: wait until the hook is reached.
    async fn wait(&self) {
        match tokio::time::timeout(DEADLINE, self.reached.acquire()).await {
            Ok(Ok(permit)) => permit.forget(),
            Ok(Err(_)) => panic!("gate closed"),
            Err(_) => panic!("gate was never reached within the deadline"),
        }
    }

    /// Called from the test: let the hook proceed.
    fn open(&self) {
        self.release.add_permits(1);
    }
}

#[derive(Default)]
struct Inbox {
    steers: Vec<Promotion>,
    queued: Vec<Promotion>,
}

/// The per-drain context: what the coordinator carries from `begin_drain` to
/// the drain's last Step.
///
/// It holds the placement resolved ONCE, and the chain state that makes
/// `continuation_owed` a real question. `steps_left` scripts how many Steps a
/// promotion's chain takes before it reaches its terminal, so a witness can put
/// a boundary in the middle of a chain the way a tool-calling model does.
struct TestDrain {
    session: SessionId,
    placement: Placement,
    /// Steps still owed by the chain in flight. Zero means idle.
    steps_left: u32,
    /// Every promotion that joined this drain, in order.
    joined: Vec<String>,
}

/// Where a gate is installed in the call path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hook {
    /// The very first eligibility question of a drain — after ownership is
    /// claimed, before any run.
    FirstEligible,
    /// Entry to `run`.
    Run,
    /// An eligibility pass that is about to report nothing eligible: the final
    /// cut, where the drain is deciding to release ownership.
    FinalCut,
}

struct TestApp {
    known: Vec<SessionId>,
    inboxes: Mutex<HashMap<SessionId, Inbox>>,
    passes: Mutex<Vec<(String, Boundary)>>,
    placements: Mutex<HashMap<String, u32>>,
    running: Arc<Mutex<Vec<String>>>,
    peak_concurrent: Arc<AtomicUsize>,
    ran: Mutex<Vec<String>>,
    gate: Option<(Hook, Gate)>,
    eligible_calls: AtomicUsize,
    /// Fail placement for these Sessions.
    fail_placement: Vec<SessionId>,
    /// Fail the inbox read after this many eligibility calls.
    fail_inbox_after: Option<usize>,
    /// Fail the run.
    fail_run: bool,
    /// Fail the existence check, as a store read would.
    fail_exists: bool,
    /// How many Steps a promotion's chain owes before it reaches its terminal.
    /// One means every promotion is answered by a single Step.
    steps_per_promotion: u32,
    /// One entry per `advance`, so a witness can count Steps as distinct from
    /// promotions.
    stepped: Mutex<Vec<String>>,
}

impl TestApp {
    fn new(known: Vec<SessionId>) -> Self {
        Self {
            known,
            inboxes: Mutex::new(HashMap::new()),
            passes: Mutex::new(Vec::new()),
            placements: Mutex::new(HashMap::new()),
            running: Arc::new(Mutex::new(Vec::new())),
            peak_concurrent: Arc::new(AtomicUsize::new(0)),
            ran: Mutex::new(Vec::new()),
            gate: None,
            eligible_calls: AtomicUsize::new(0),
            fail_placement: Vec::new(),
            fail_inbox_after: None,
            fail_run: false,
            fail_exists: false,
            steps_per_promotion: 1,
            stepped: Mutex::new(Vec::new()),
        }
    }

    /// Script a chain that takes several Steps, the way a tool-calling model
    /// does: the drain must come back to a boundary between them.
    fn with_steps_per_promotion(mut self, steps: u32) -> Self {
        self.steps_per_promotion = steps;
        self
    }

    fn stepped(&self) -> usize {
        match self.stepped.lock() {
            Ok(stepped) => stepped.len(),
            Err(_) => panic!("stepped lock poisoned"),
        }
    }

    fn failing_exists(mut self) -> Self {
        self.fail_exists = true;
        self
    }

    fn with_inbox(self, session: &SessionId, inbox: Inbox) -> Self {
        match self.inboxes.lock() {
            Ok(mut map) => {
                map.insert(session.clone(), inbox);
            }
            Err(_) => panic!("inbox lock poisoned"),
        }
        self
    }

    fn with_gate(mut self, hook: Hook, gate: Gate) -> Self {
        self.gate = Some((hook, gate));
        self
    }

    fn failing_placement(mut self, session: &SessionId) -> Self {
        self.fail_placement.push(session.clone());
        self
    }

    fn failing_inbox_after(mut self, calls: usize) -> Self {
        self.fail_inbox_after = Some(calls);
        self
    }

    fn failing_run(mut self) -> Self {
        self.fail_run = true;
        self
    }

    fn add_steer(&self, session: &SessionId, id: &str) {
        match self.inboxes.lock() {
            Ok(mut map) => map
                .entry(session.clone())
                .or_default()
                .steers
                .push(steer(id)),
            Err(_) => panic!("inbox lock poisoned"),
        }
    }

    fn boundaries_for(&self, session: &SessionId) -> Vec<Boundary> {
        match self.passes.lock() {
            Ok(passes) => passes
                .iter()
                .filter(|(id, _)| id == session.as_str())
                .map(|(_, boundary)| *boundary)
                .collect(),
            Err(_) => panic!("passes lock poisoned"),
        }
    }

    fn placement_lookups(&self, session: &SessionId) -> u32 {
        match self.placements.lock() {
            Ok(map) => map.get(session.as_str()).copied().unwrap_or(0),
            Err(_) => panic!("placements lock poisoned"),
        }
    }

    fn ran(&self) -> Vec<String> {
        match self.ran.lock() {
            Ok(ran) => ran.clone(),
            Err(_) => panic!("ran lock poisoned"),
        }
    }

    fn gate_for(&self, hook: Hook) -> Option<Gate> {
        match &self.gate {
            Some((installed, gate)) if *installed == hook => Some(gate.clone()),
            _ => None,
        }
    }
}

impl SessionApplication for TestApp {
    type Drain = TestDrain;

    fn session_exists(&self, session: &SessionId) -> Result<bool, CoordinatorError> {
        if self.fail_exists {
            return Err(CoordinatorError::Inbox {
                session: session.as_str().to_owned(),
                reason: "session store unreadable".to_owned(),
            });
        }
        Ok(self.known.iter().any(|known| known == session))
    }

    fn begin_drain(&self, session: &SessionId) -> Result<Self::Drain, CoordinatorError> {
        match self.placements.lock() {
            Ok(mut map) => {
                *map.entry(session.as_str().to_owned()).or_insert(0) += 1;
            }
            Err(_) => panic!("placements lock poisoned"),
        }
        if self.fail_placement.iter().any(|failing| failing == session) {
            return Err(CoordinatorError::Placement {
                session: session.as_str().to_owned(),
                reason: "location service map refused".to_owned(),
            });
        }
        Ok(TestDrain {
            session: session.clone(),
            placement: Placement {
                location: location("loc_main"),
            },
            steps_left: 0,
            joined: Vec::new(),
        })
    }

    fn continuation_owed(&self, drain: &Self::Drain) -> bool {
        drain.steps_left > 0
    }

    fn next_eligible(
        &self,
        drain: &mut Self::Drain,
        boundary: Boundary,
    ) -> Result<Option<Promotion>, CoordinatorError> {
        let session = drain.session.clone();
        let call = self.eligible_calls.fetch_add(1, Ordering::SeqCst) + 1;
        match self.passes.lock() {
            Ok(mut passes) => passes.push((session.as_str().to_owned(), boundary)),
            Err(_) => panic!("passes lock poisoned"),
        }
        if let Some(after) = self.fail_inbox_after {
            if call > after {
                return Err(CoordinatorError::Inbox {
                    session: session.as_str().to_owned(),
                    reason: "pending table unreadable".to_owned(),
                });
            }
        }

        let promotion = {
            let mut inboxes = match self.inboxes.lock() {
                Ok(inboxes) => inboxes,
                Err(_) => panic!("inbox lock poisoned"),
            };
            match inboxes.get_mut(&session) {
                None => None,
                Some(inbox) => match boundary {
                    Boundary::Steer => {
                        if inbox.steers.is_empty() {
                            None
                        } else {
                            Some(inbox.steers.remove(0))
                        }
                    }
                    Boundary::Idle => {
                        if inbox.queued.is_empty() {
                            None
                        } else {
                            Some(inbox.queued.remove(0))
                        }
                    }
                },
            }
        };

        // Synchronous hooks block the calling thread on purpose: this method is
        // synchronous in the seam, so a gate here is a real cut.
        if call == 1 {
            if let Some(gate) = self.gate_for(Hook::FirstEligible) {
                gate.reached.add_permits(1);
                loop {
                    if gate.release.try_acquire().is_ok() {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        }
        if promotion.is_none() && boundary == Boundary::Idle {
            if let Some(gate) = self.gate_for(Hook::FinalCut) {
                gate.reached.add_permits(1);
                loop {
                    if gate.release.try_acquire().is_ok() {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        }

        if let Some(promotion) = promotion.as_ref() {
            // The promotion JOINS the drain, exactly as production does: it
            // guarantees the chain owes at least a full script of Steps. A
            // steer joining a chain already owing more does not extend it.
            drain.steps_left = drain.steps_left.max(self.steps_per_promotion);
            drain
                .joined
                .push(promotion.input.message().as_str().to_owned());
            match self.ran.lock() {
                Ok(mut ran) => ran.push(promotion.input.message().as_str().to_owned()),
                Err(_) => panic!("ran lock poisoned"),
            }
        }
        Ok(promotion)
    }

    fn advance<'a>(
        &'a self,
        drain: &'a mut Self::Drain,
        cancellation: Cancellation,
    ) -> BoxFuture<'a, Result<RunOutcome, CoordinatorError>> {
        let mut cancel = cancellation.signal();
        let session = drain.session.clone();
        let peak = match self.running.lock() {
            Ok(mut running) => {
                running.push(session.as_str().to_owned());
                running.len()
            }
            Err(_) => panic!("running lock poisoned"),
        };
        self.peak_concurrent.fetch_max(peak, Ordering::SeqCst);
        match self.stepped.lock() {
            Ok(mut stepped) => stepped.push(session.as_str().to_owned()),
            Err(_) => panic!("stepped lock poisoned"),
        }

        struct Leave {
            running: Arc<Mutex<Vec<String>>>,
            id: String,
        }
        impl Drop for Leave {
            fn drop(&mut self) {
                if let Ok(mut running) = self.running.lock() {
                    if let Some(index) = running.iter().position(|entry| *entry == self.id) {
                        running.remove(index);
                    }
                }
            }
        }
        let leave = Leave {
            running: Arc::clone(&self.running),
            id: session.as_str().to_owned(),
        };
        let gate = self.gate_for(Hook::Run);
        let fail = self.fail_run;
        let id = session.as_str().to_owned();

        Box::pin(async move {
            let _leave = leave;
            if let Some(gate) = gate {
                gate.arrive().await;
            }
            if fail {
                // A failed Step ends the chain: nothing owed, nothing retried.
                drain.steps_left = 0;
                return Err(CoordinatorError::Run {
                    session: id,
                    reason: "step driver failed".to_owned(),
                });
            }
            if cancel.borrow_and_update().is_some() {
                drain.steps_left = 0;
                return Ok(RunOutcome::Interrupted);
            }
            drain.steps_left = drain.steps_left.saturating_sub(1);
            if drain.steps_left > 0 {
                Ok(RunOutcome::Continue)
            } else {
                Ok(RunOutcome::Idle)
            }
        })
    }
}

fn steer(id: &str) -> Promotion {
    Promotion {
        input: input(id),
        delivery: DeliveryMode::Steer,
    }
}

fn queued(id: &str) -> Promotion {
    Promotion {
        input: input(id),
        delivery: DeliveryMode::Queue,
    }
}

async fn within<F: std::future::Future>(what: &str, future: F) -> F::Output {
    match tokio::time::timeout(DEADLINE, future).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not complete within the deadline"),
    }
}

include!("coordinator/ownership_claim_and_join.rs");
include!("coordinator/concurrent_drain_order_placement.rs");
include!("coordinator/interrupt_steer_idle.rs");
include!("coordinator/store_failure_and_quiescence.rs");
include!("coordinator/claim_handoff.rs");

// ---------------------------------------------------------------------------
// Whole-candidate correction: one bounded active/capacity authority
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixty_four_active_sessions_defer_the_sixty_fifth_then_drain_it_once() {
    use std::num::NonZeroUsize;

    const CAPACITY: usize = 64;
    const REQUESTS: usize = 65;

    let sessions: Vec<_> = (0..REQUESTS)
        .map(|index| session(&format!("capacity_{index:02}")))
        .collect();
    let gate = Gate::new();
    let mut app = TestApp::new(sessions.clone());
    for (index, held) in sessions.iter().enumerate() {
        app = app.with_inbox(
            held,
            Inbox {
                steers: vec![steer(&format!("capacity_{index:02}"))],
                queued: Vec::new(),
            },
        );
    }
    let app = Arc::new(app.with_gate(Hook::Run, gate.clone()));
    let capacity = NonZeroUsize::new(CAPACITY)
        .unwrap_or_else(|| panic!("the deterministic capacity is non-zero"));
    let coordinator = Arc::new(SessionCoordinator::with_capacity(
        Arc::clone(&app),
        capacity,
    ));

    let mut wakes = Vec::new();
    for held in &sessions {
        let coordinator = Arc::clone(&coordinator);
        let held = held.clone();
        wakes.push(tokio::spawn(async move { coordinator.wake(held).await }));
    }

    for _ in 0..CAPACITY {
        gate.wait().await;
    }
    let active = coordinator.active_sessions();
    assert_eq!(active.len(), CAPACITY);
    assert_eq!(
        app.peak_concurrent.load(Ordering::SeqCst),
        CAPACITY,
        "the coordinator exceeded the exact process-wide bound"
    );

    gate.open();
    gate.wait().await;
    assert_eq!(coordinator.active_sessions().len(), CAPACITY);

    for _ in 0..CAPACITY {
        gate.open();
    }
    for (index, wake) in wakes.into_iter().enumerate() {
        let outcome = within(&format!("capacity wake {index}"), wake).await;
        assert!(matches!(outcome, Ok(Ok(WakeOutcome::Drained { .. }))));
    }

    let mut ran = app.ran();
    ran.sort();
    ran.dedup();
    assert_eq!(ran.len(), REQUESTS, "every request drained exactly once");
    assert!(coordinator.active_sessions().is_empty());
}
