//! R2-f acceptance witnesses.
//!
//! Six proofs required by the commission. Each names the contract clause or
//! oracle defect it stands for, so a failure says what broke rather than which
//! assertion tripped.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::{
    AttemptAdmission, AttemptAdmissionRefusal, AttemptPreparation, AttemptProtocol,
    ReservedAttemptAdmission, UnopenedAttempt,
};
use crate::{
    AttemptCancelCause, AttemptOwner, AttemptReport, BoxFuture, DrainResidency, DurableFact,
    InterruptHandle, Observation, ProviderReader, RecordingOwner, Residency, StepOutcome,
    ToolOutcome, ToolRunner, ATTEMPT_TOOL_BUDGET, PROVIDER_CHANNEL_CAPACITY,
};
use grapt_provider::{
    AdapterState, AttemptFragment, BlockOrdinal, FinishReason, ProviderFailureClass, ToolExecution,
};
use grapt_types::{
    AttemptTerminalReason, InterruptionSource, PhysicalAttemptId,
    ProviderFailureClass as TerminalClass, ToolCallId,
};
use tokio::sync::Notify;

/// A Step watch that never publishes. These fixtures exercise admission, not
/// interruption. The sender is dropped rather than leaked, which exercises the
/// closed-watch path deliberately: losing the Step sender removes an interrupt
/// source and must leave a healthy attempt running.
fn step_watch() -> tokio::sync::watch::Receiver<Option<InterruptionSource>> {
    let (sender, receiver) = tokio::sync::watch::channel(None);
    drop(sender);
    receiver
}

fn attempt(id: &str) -> PhysicalAttemptId {
    match PhysicalAttemptId::new(id) {
        Some(value) => value,
        None => panic!("attempt id must be non-empty"),
    }
}

/// A standalone residency token. These witnesses drive the supervisor directly,
/// with no coordinator to answer to, so nothing here observes the other end —
/// the token exists because the supervisor's spawned tasks must carry one.
/// Residency itself is witnessed in `grapt_runtime::residency`, and end to end
/// in the R3-d suite.
fn residency() -> Residency {
    DrainResidency::new().token()
}

fn call(id: &str) -> ToolCallId {
    match ToolCallId::new(id) {
        Some(value) => value,
        None => panic!("call id must be non-empty"),
    }
}

fn tool_called(id: &str) -> AttemptFragment {
    AttemptFragment::ToolCalled {
        call: call(id),
        tool: "echo".into(),
        input: "{}".into(),
        execution: ToolExecution::Local,
        state: AdapterState::none(),
    }
}

fn text_block(ordinal: u32, body: &str) -> Vec<AttemptFragment> {
    let block = BlockOrdinal::new(ordinal);
    vec![
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: body.into(),
            state: AdapterState::none(),
        },
        AttemptFragment::TextClosed {
            block,
            state: AdapterState::none(),
        },
    ]
}

fn stop() -> AttemptFragment {
    AttemptFragment::ProviderTerminal {
        finish: FinishReason::Stop,
        usage: None,
        state: AdapterState::none(),
    }
}

/// A reader that yields a fixed script as fast as the channel allows.
struct ScriptedReader {
    fragments: std::vec::IntoIter<AttemptFragment>,
    saturation: Option<Arc<Saturation>>,
}

impl ScriptedReader {
    fn new(fragments: Vec<AttemptFragment>) -> Self {
        Self {
            fragments: fragments.into_iter(),
            saturation: None,
        }
    }

    fn reporting_to(mut self, saturation: Arc<Saturation>) -> Self {
        self.saturation = Some(saturation);
        self
    }
}

impl ProviderReader for ScriptedReader {
    fn next(&mut self) -> impl std::future::Future<Output = Option<AttemptFragment>> + Send {
        let next = self.fragments.next();
        if let Some(saturation) = &self.saturation {
            saturation.read();
        }
        async move { next }
    }

    fn reservation_probe(&self) -> Option<crate::supervisor::reservation_probe::ReservationProbe> {
        let saturation = Arc::clone(self.saturation.as_ref()?);
        Some(crate::supervisor::reservation_probe::ReservationProbe::new(
            move || saturation.reservation_parked(),
        ))
    }
}

/// A reader that never yields anything and never completes — a hung provider.
/// It notifies `entered` once the attempt is waiting on it.
struct HungReader {
    entered: Arc<Notify>,
}

impl ProviderReader for HungReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        self.entered.notify_one();
        // Never resolves. Only direct cancellation can end this.
        std::future::pending::<()>().await;
        None
    }
}

/// Tools that complete immediately.
struct InstantTools;

impl ToolRunner for InstantTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        Box::pin(async { ToolOutcome::Completed { output: None } })
    }
}

/// Tools that block until released, and count how many are concurrently inside.
struct BarrierTools {
    release: Arc<Notify>,
    inside: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl ToolRunner for BarrierTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        let release = Arc::clone(&self.release);
        let inside = Arc::clone(&self.inside);
        let peak = Arc::clone(&self.peak);
        Box::pin(async move {
            let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            release.notified().await;
            inside.fetch_sub(1, Ordering::SeqCst);
            ToolOutcome::Completed { output: None }
        })
    }
}

/// An owner that stalls on every commit, to create real backpressure.
struct SlowOwner {
    inner: RecordingOwner,
    stall: Duration,
}

impl AttemptOwner for SlowOwner {
    fn commit(&mut self, fact: DurableFact) -> Result<(), crate::OwnerError> {
        // Synchronous by signature — a blocking sleep is the honest way to
        // model a slow transaction here, and proves the owner cannot await.
        std::thread::sleep(self.stall);
        self.inner.commit(fact)
    }
    fn notify(&mut self, fact: &DurableFact) {
        self.inner.notify(fact)
    }
    fn observe(&mut self, observation: Observation) {
        self.inner.observe(observation)
    }
}

/// Wait for `signal`, failing the test with `what` after five seconds.
async fn signalled(signal: &Notify, what: &str) {
    if tokio::time::timeout(Duration::from_secs(5), signal.notified())
        .await
        .is_err()
    {
        panic!("{what}");
    }
}

/// Proves the provider channel is full behind a held attempt loop and the
/// reader's reservation is waiting on it.
///
/// The reader sends every fragment but a text delta as soon as it reads it,
/// so a reservation made while it is asked for fragment `n` is fragment `n`'s.
/// Once the loop is held having received `m` fragments, the channel holds
/// fragments `m + 1..=m + capacity`, so a waiting reservation for fragment
/// `m + capacity + 1` is the send that a full channel parked.
struct Saturation {
    reads_and_hold: std::sync::Mutex<(usize, Option<usize>)>,
    capacity: usize,
    parked: Notify,
}

impl Saturation {
    fn new(capacity: usize) -> Self {
        Self {
            reads_and_hold: std::sync::Mutex::new((0, None)),
            capacity,
            parked: Notify::new(),
        }
    }

    fn read(&self) {
        self.lock().0 += 1;
    }

    fn held_after(&self, received: usize) {
        self.lock().1 = Some(received);
    }

    fn reservation_parked(&self) {
        let (reads, held) = *self.lock();
        if held.is_some_and(|received| reads > received + self.capacity) {
            self.parked.notify_one();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (usize, Option<usize>)> {
        self.reads_and_hold
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// An owner that holds the first text commit made while both tools run, so
/// the attempt loop stops draining and the provider channel behind it fills.
/// Every received fragment is committed once, so its count is what the loop
/// has received.
struct HeldOwner {
    inner: RecordingOwner,
    gate: Arc<CommitGate>,
    saturation: Arc<Saturation>,
    inside: Arc<AtomicUsize>,
    received: usize,
    held_once: bool,
}

impl HeldOwner {
    fn new(gate: Arc<CommitGate>, saturation: Arc<Saturation>, inside: Arc<AtomicUsize>) -> Self {
        Self {
            inner: RecordingOwner::new(),
            gate,
            saturation,
            inside,
            received: 0,
            held_once: false,
        }
    }
}

/// Released once; a commit held past `bound` proceeds so a failed test cannot
/// wedge a runtime worker.
struct CommitGate {
    released: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
    bound: Duration,
}

impl CommitGate {
    fn new(bound: Duration) -> Self {
        Self {
            released: std::sync::Mutex::new(false),
            wake: std::sync::Condvar::new(),
            bound,
        }
    }

    fn release(&self) {
        *self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.wake.notify_all();
    }

    fn wait(&self) {
        let released = self
            .released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(
            self.wake
                .wait_timeout_while(released, self.bound, |released| !*released)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

impl AttemptOwner for HeldOwner {
    fn commit(&mut self, fact: DurableFact) -> Result<(), crate::OwnerError> {
        if let DurableFact::Fragment { fragment } = &fact {
            self.received += 1;
            let text = matches!(**fragment, AttemptFragment::TextOpened { .. });
            if text && !self.held_once && self.inside.load(Ordering::SeqCst) == 2 {
                self.held_once = true;
                self.saturation.held_after(self.received);
                // Hand this worker's queued tasks on first: the reader this
                // loop just woke must keep running while the commit is held.
                tokio::task::block_in_place(|| self.gate.wait());
            }
        }
        self.inner.commit(fact)
    }
    fn notify(&mut self, fact: &DurableFact) {
        self.inner.notify(fact)
    }
    fn observe(&mut self, observation: Observation) {
        self.inner.observe(observation)
    }
}

/// An owner that refuses the first fact matching a predicate, to prove a
/// refusal stops the attempt instead of being swallowed.
struct RejectingOwner {
    inner: RecordingOwner,
    reject: Box<dyn Fn(&DurableFact) -> bool + Send>,
    fired: bool,
}

impl RejectingOwner {
    fn rejecting_first_matching<P>(predicate: P) -> Self
    where
        P: Fn(&DurableFact) -> bool + Send + 'static,
    {
        Self {
            inner: RecordingOwner::new(),
            reject: Box::new(predicate),
            fired: false,
        }
    }
}

impl AttemptOwner for RejectingOwner {
    fn commit(&mut self, fact: DurableFact) -> Result<(), crate::OwnerError> {
        if !self.fired && (self.reject)(&fact) {
            self.fired = true;
            return Err(crate::OwnerError {
                reason: "store refused the fact".to_owned(),
            });
        }
        self.inner.commit(fact)
    }
    fn notify(&mut self, fact: &DurableFact) {
        self.inner.notify(fact)
    }
    fn observe(&mut self, observation: Observation) {
        self.inner.observe(observation)
    }
}

/// Ordinary admission for a fixture attempt, with evidence naming that attempt.
fn admitted<R>(unopened: UnopenedAttempt<R>) -> AttemptPreparation<R> {
    let evidence = ReservedAttemptAdmission::new(unopened.attempt().clone());
    AttemptPreparation::reserved(unopened, evidence)
}

fn prepare_admitted<F, R, T, O>(
    attempt: PhysicalAttemptId,
    make_reader: F,
    tools: Arc<T>,
    owner: O,
    residency: Residency,
) -> (InterruptHandle, impl Future<Output = (O, AttemptReport)>)
where
    F: FnOnce() -> R + Send + 'static,
    R: ProviderReader,
    T: ToolRunner,
    O: AttemptOwner,
{
    let preparation = admitted(UnopenedAttempt::new(attempt, make_reader));
    match AttemptProtocol::prepare(preparation, tools, owner, residency, step_watch()) {
        Ok(crate::attempt_admission::PreparedAttempt { interrupt, run, .. }) => (interrupt, run),
        Err((_owner, refusal)) => panic!("private admitted fixture refused: {refusal}"),
    }
}

#[test]
fn hostile_private_refusal_invokes_zero_reader_factories() {
    let factories = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&factories);
    let preparation = AttemptPreparation {
        admission: AttemptAdmission::Refused(AttemptAdmissionRefusal::NoIssuer),
        unopened: Some(UnopenedAttempt::new(
            attempt("att_hostile_refusal"),
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
                ScriptedReader::new(Vec::new())
            },
        )),
    };
    let result = AttemptProtocol::prepare(
        preparation,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        residency(),
        step_watch(),
    );
    assert!(matches!(
        result,
        Err((_, AttemptAdmissionRefusal::NoIssuer))
    ));
    assert_eq!(factories.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
mod coordinator;
mod session_application;
mod step;
mod step_policy;
mod supervision;
mod terminal;
