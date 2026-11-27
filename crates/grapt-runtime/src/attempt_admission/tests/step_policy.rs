//! R2-g acceptance witnesses — Step/PhysicalAttempt policy.
//!
//! Time is paused throughout, so backoff and stall windows elapse exactly and
//! the suite asserts policy rather than wall clock.
//!
//! The provider-open counter lives inside the deferred FACTORY, never in
//! `AttemptSource::open`. That is deliberate: "zero provider opens" only means
//! something if the counter tracks the stream actually opening, not the
//! identity being minted.

use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "step_policy/credential_recovery.rs"]
mod credential_recovery;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::attempt_admission::UnopenedAttempt;
use crate::{
    step_interrupt, AttemptOwner, AttemptPreparation, AttemptSource, BoxFuture, CompactionError,
    DurableFact, HistoryGeneration, HistorySource, Observation, RecordingOwner, StepDriver,
    StepOutcome, StepPolicy, StepResult, ToolOutcome, ToolRunner, RULED_DEFAULT_POLICY_VERSION,
};
use grapt_provider::{
    AdapterState, AttemptFragment, BlockOrdinal, FailureClassification, FinishReason,
    ProviderFailureClass,
};
use grapt_types::{
    AttemptTerminalReason, ContextFailure, InterruptionSource, PhysicalAttemptId, PolicyVersion,
    ProviderFailureClass as TerminalClass, StepNumber, StepTerminalReason, ToolCallId,
};

fn step() -> StepNumber {
    match StepNumber::new(1) {
        Some(step) => step,
        None => panic!("step 1 is valid"),
    }
}

/// A named version for the policies these tests build. `PolicyLimit`
/// carries the version of the policy whose bound fired, so a test policy
/// without an identity could not witness that field at all.
fn test_version() -> PolicyVersion {
    match PolicyVersion::new("step-policy-tests/1") {
        Some(version) => version,
        None => panic!("a non-empty literal must build a PolicyVersion"),
    }
}

fn policy(max_attempts: u32) -> StepPolicy {
    match StepPolicy::new(
        test_version(),
        max_attempts,
        Duration::from_secs(2),
        1,
        Duration::from_secs(30),
    ) {
        Ok(policy) => policy,
        Err(error) => panic!("test policy must be valid: {error}"),
    }
}

fn text(body: &str) -> Vec<AttemptFragment> {
    let block = BlockOrdinal::new(0);
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

fn failure(class: ProviderFailureClass) -> AttemptFragment {
    AttemptFragment::ProviderFailure {
        class,
        classification: None,
        diagnostic: None,
        message: "boom".into(),
        state: AdapterState::none(),
    }
}

fn overflow() -> AttemptFragment {
    AttemptFragment::ProviderFailure {
        class: ProviderFailureClass::InvalidRequest,
        classification: Some(FailureClassification::ContextOverflow),
        diagnostic: None,
        message: "context window exceeded".into(),
        state: AdapterState::none(),
    }
}

/// Yields a script, then either ends or stalls forever.
struct Reader {
    fragments: std::vec::IntoIter<AttemptFragment>,
    stall_at_end: bool,
}

impl crate::ProviderReader for Reader {
    fn next(&mut self) -> impl std::future::Future<Output = Option<AttemptFragment>> + Send {
        let next = self.fragments.next();
        let stall = self.stall_at_end && next.is_none();
        async move {
            if stall {
                std::future::pending::<()>().await;
            }
            next
        }
    }
}

/// Records what the driver asked for, and counts streams actually OPENED.
struct ScriptSource {
    scripts: Vec<Vec<AttemptFragment>>,
    stall_at_end: bool,
    /// Identities minted — incremented in `open`.
    mints: Arc<AtomicUsize>,
    /// Streams opened — incremented inside the deferred factory.
    opens: Arc<AtomicUsize>,
    /// The generation each attempt was built from, in order.
    generations: Arc<Mutex<Vec<u64>>>,
}

impl ScriptSource {
    fn new(scripts: Vec<Vec<AttemptFragment>>) -> Self {
        Self {
            scripts,
            stall_at_end: false,
            mints: Arc::new(AtomicUsize::new(0)),
            opens: Arc::new(AtomicUsize::new(0)),
            generations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn stalling(scripts: Vec<Vec<AttemptFragment>>) -> Self {
        let mut source = Self::new(scripts);
        source.stall_at_end = true;
        source
    }

    fn generations(&self) -> Vec<u64> {
        match self.generations.lock() {
            Ok(values) => values.clone(),
            Err(_) => panic!("generations lock poisoned"),
        }
    }
}

impl AttemptSource for ScriptSource {
    type Reader = Reader;

    fn prepare(
        &mut self,
        _step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        self.mints.fetch_add(1, Ordering::SeqCst);
        match self.generations.lock() {
            Ok(mut values) => values.push(generation.value()),
            Err(_) => panic!("generations lock poisoned"),
        }
        let attempt = match PhysicalAttemptId::new(format!("att_{ordinal}")) {
            Some(attempt) => attempt,
            None => panic!("attempt id must be non-empty"),
        };
        let script = if self.scripts.is_empty() {
            Vec::new()
        } else {
            let index = ((ordinal as usize) - 1).min(self.scripts.len() - 1);
            self.scripts[index].clone()
        };
        let opens = Arc::clone(&self.opens);
        let stall_at_end = self.stall_at_end;
        let unopened = UnopenedAttempt::new(attempt, move || {
            // Counted HERE: this is the moment provider I/O would begin.
            opens.fetch_add(1, Ordering::SeqCst);
            Reader {
                fragments: script.into_iter(),
                stall_at_end,
            }
        });
        super::admitted(unopened)
    }
}

/// Records compaction/reload order and can fail either half.
#[derive(Debug)]
struct FakeHistory {
    calls: Vec<&'static str>,
    generation: u64,
    fail_compact: bool,
    fail_reload: bool,
    /// How many times the compaction provider call was made.
    compact_calls: u32,
    compact_duration: Duration,
    /// When set, reload returns this instead of advancing.
    reload_generation: Option<u64>,
}

impl FakeHistory {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            generation: 1,
            fail_compact: false,
            fail_reload: false,
            compact_calls: 0,
            compact_duration: Duration::ZERO,
            reload_generation: None,
        }
    }

    fn slow_compact(delay: Duration) -> Self {
        Self {
            compact_duration: delay,
            ..Self::new()
        }
    }

    fn reload_returning(generation: u64) -> Self {
        Self {
            reload_generation: Some(generation),
            ..Self::new()
        }
    }
    fn failing_compact() -> Self {
        Self {
            fail_compact: true,
            ..Self::new()
        }
    }
    fn failing_reload() -> Self {
        Self {
            fail_reload: true,
            ..Self::new()
        }
    }
}

impl HistorySource for FakeHistory {
    async fn compact(&mut self) -> Result<(), CompactionError> {
        self.calls.push("compact");
        self.compact_calls += 1;
        // A real compaction is a provider call; model that with a delay so an
        // interrupt has something to land inside.
        tokio::time::sleep(self.compact_duration).await;
        if self.fail_compact {
            return Err(CompactionError {
                reason: "compaction call failed".to_owned(),
            });
        }
        Ok(())
    }
    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
        self.calls.push("reload");
        if self.fail_reload {
            return Err(CompactionError {
                reason: "reload failed".to_owned(),
            });
        }
        if let Some(fixed) = self.reload_generation {
            return Ok(HistoryGeneration::new(fixed));
        }
        self.generation += 1;
        Ok(HistoryGeneration::new(self.generation))
    }
    fn generation(&self) -> HistoryGeneration {
        HistoryGeneration::new(self.generation)
    }
}

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

/// Refuses the first fact matching a predicate.
struct RejectingOwner {
    inner: RecordingOwner,
    reject: Box<dyn Fn(&DurableFact) -> bool + Send>,
    fired: bool,
}

impl RejectingOwner {
    fn rejecting<P>(predicate: P) -> Self
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

fn assert_ids_distinct(report: &crate::StepReport) {
    let ids: Vec<&str> = report
        .receipts
        .iter()
        .map(|receipt| receipt.attempt.as_str())
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        ids.len(),
        sorted.len(),
        "attempt ids must be distinct: {ids:?}"
    );
}

// ---------------------------------------------------------------------------
// Ordering: nothing opens a stream before the attempt exists
// ---------------------------------------------------------------------------

mod admission_compaction_overflow;
mod cancellation_watchdogs;
mod decision_waits;
mod generation_dropped_handle_timing;
mod preflight_failure_retry_cap;
mod receipt_dedup_interrupt;
mod terminal_vocabulary_policy_defaults;
mod tool_watchdog;

mod context_preparation;
mod local_capacity;

mod failure_diagnosis;
