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
}

impl ScriptedReader {
    fn new(fragments: Vec<AttemptFragment>) -> Self {
        Self {
            fragments: fragments.into_iter(),
        }
    }
}

impl ProviderReader for ScriptedReader {
    fn next(&mut self) -> impl std::future::Future<Output = Option<AttemptFragment>> + Send {
        let next = self.fragments.next();
        async move { next }
    }
}

/// A reader that never yields anything and never completes — a hung provider.
struct HungReader;

impl ProviderReader for HungReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
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
