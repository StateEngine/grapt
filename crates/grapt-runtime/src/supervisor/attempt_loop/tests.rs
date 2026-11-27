//! Witnesses for the attempt loop.
//!
//! Split out of `attempt_loop.rs` unchanged: that file sat at 995 physical
//! lines against the workspace 1,000-line ceiling, so a witness could not be
//! added to it without breaching the gate. The split is mechanical and comes
//! before the change it makes room for, never after.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use grapt_provider::AttemptFragment::{
    HostedToolFailed, HostedToolResult, ProviderFailure, ProviderTerminal, TextClosed, TextDelta,
    TextOpened, ToolCalled,
};
use grapt_provider::{
    AdapterState, AttemptFragment, FinishReason, ProviderFailureClass, ToolExecution,
};
use grapt_types::{PhysicalAttemptId, ToolCallId, ToolFailureReason, ToolTerminalOutcome};
use tokio::sync::watch;

use super::run_attempt;
use crate::budgets::ATTEMPT_TOOL_CALL_LIMIT;
use crate::owner::DurableFact::{AttemptFailed, Fragment, ToolTerminal};
use crate::owner::{DurableFact, RecordingOwner};
use crate::residency::DrainResidency;
use crate::supervisor::{AttemptReport, BoxFuture, ProviderReader, StepOutcome, ToolRunner};

/// A Step watch with no sender and no published value: these fixtures drive
/// provider behaviour, not interruption, so for them losing the Step sender
/// must leave the attempt running. That is also the closed-watch contract.
pub(super) fn quiet_step() -> watch::Receiver<Option<grapt_types::InterruptionSource>> {
    let (sender, receiver) = watch::channel(None);
    drop(sender);
    receiver
}

mod nonclean_exit;
mod reader_cancellation;

struct ScriptedReader(std::vec::IntoIter<AttemptFragment>);

impl ProviderReader for ScriptedReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        self.0.next()
    }
}

struct NeverTools;

impl ToolRunner for NeverTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        panic!("provider-hosted tools must not enter the local tool runner")
    }
}

struct SignalledTools {
    completed: Arc<AtomicBool>,
}

impl ToolRunner for SignalledTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let completed = Arc::clone(&self.completed);
        Box::pin(async move {
            // ToolMessage queues synchronously after return; `true` is observed only after that poll, so failure releases after terminal queueing.
            completed.store(true, Ordering::SeqCst);
            ToolTerminalOutcome::Completed { output: None }
        })
    }
}

struct FailureAfterTool {
    stage: u8,
    completed: Arc<AtomicBool>,
}

impl ProviderReader for FailureAfterTool {
    async fn next(&mut self) -> Option<AttemptFragment> {
        match self.stage {
            0 => {
                self.stage = 1;
                Some(tool_call(call(), "local", ToolExecution::Local))
            }
            1 => {
                while !self.completed.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
                self.stage = 2;
                Some(ProviderFailure {
                    class: ProviderFailureClass::Transport,
                    classification: None,
                    diagnostic: None,
                    message: "provider failed after dispatch".to_owned(),
                    state: AdapterState::none(),
                })
            }
            _ => None,
        }
    }
}

struct NeverReturningTools;

impl ToolRunner for NeverReturningTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        Box::pin(std::future::pending())
    }
}

struct InputRecordingTools {
    observed: Arc<Mutex<Option<Vec<u8>>>>,
}

impl ToolRunner for InputRecordingTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let observed = Arc::clone(&self.observed);
        let input = input.as_bytes().to_vec();
        Box::pin(async move {
            match observed.lock() {
                Ok(mut held) => *held = Some(input),
                Err(poisoned) => *poisoned.into_inner() = Some(input),
            }
            ToolTerminalOutcome::Completed { output: None }
        })
    }
}

fn attempt(name: &str) -> PhysicalAttemptId {
    PhysicalAttemptId::new(name).unwrap_or_else(|| panic!("static attempt id must be valid"))
}

fn call() -> ToolCallId {
    call_named("call_hosted")
}

fn call_named(name: &str) -> ToolCallId {
    ToolCallId::new(name).unwrap_or_else(|| panic!("static call id must be valid"))
}

fn tool_call(call: ToolCallId, tool: &str, execution: ToolExecution) -> AttemptFragment {
    ToolCalled {
        call,
        tool: tool.to_owned(),
        input: "{}".to_owned(),
        execution,
        state: AdapterState::none(),
    }
}

#[tokio::test]
async fn local_tool_receives_the_exact_bounded_provider_input() {
    let expected = br#"{"path":"src/lib.rs","offset":4}"#.to_vec();
    let observed = Arc::new(Mutex::new(None));
    let script = vec![
        ToolCalled {
            call: call_named("call_input"),
            tool: "read".to_owned(),
            input: String::from_utf8_lossy(&expected).into_owned(),
            execution: ToolExecution::Local,
            state: AdapterState::none(),
        },
        ProviderTerminal {
            finish: FinishReason::ToolCalls,
            usage: None,
            state: AdapterState::none(),
        },
    ];
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let (_owner, report) = run_attempt(
        attempt("att_exact_input"),
        move || ScriptedReader(script.into_iter()),
        Arc::new(InputRecordingTools {
            observed: Arc::clone(&observed),
        }),
        RecordingOwner::new(),
        cancel_tx,
        cancel_rx,
        residency,
        quiet_step(),
    )
    .await;
    assert!(matches!(report.outcome, StepOutcome::Ended { .. }));
    let held = match observed.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    assert_eq!(held.as_deref(), Some(expected.as_slice()));
}

#[derive(Debug)]
struct HostedClock;
impl grapt_types::ClockAuthority for HostedClock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1)
    }
}
struct HostedScratch(std::path::PathBuf);
impl Drop for HostedScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct CapturingRecorder {
    inner: RecordingOwner,
    artifacts: grapt_artifact::FilesystemArtifactStore,
}
impl crate::owner::AttemptOwner for CapturingRecorder {
    fn capture_hosted_output(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, crate::owner::OwnerError> {
        use grapt_artifact::ArtifactStore;
        let bound = grapt_artifact::CaptureBound::new(
            crate::TOOL_ARTIFACT_MAX_BYTES,
            crate::TOOL_ARTIFACT_BUFFER_BYTES,
        )
        .ok_or_else(|| crate::owner::OwnerError {
            reason: "fixture capture bound".into(),
        })?;
        let captured = self
            .artifacts
            .capture(
                text.as_bytes(),
                bound,
                grapt_artifact::ArtifactKind::CapturedOutput,
                grapt_artifact::Sensitivity::SecretBearing,
            )
            .map_err(|_| crate::owner::OwnerError {
                reason: "fixture capture failed".into(),
            })?;
        grapt_types::ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
            .ok_or_else(|| crate::owner::OwnerError {
                reason: "fixture capture disposition".into(),
            })
    }
    fn commit(&mut self, fact: DurableFact) -> Result<(), crate::owner::OwnerError> {
        crate::owner::AttemptOwner::commit(&mut self.inner, fact)
    }
    fn notify(&mut self, fact: &DurableFact) {
        crate::owner::AttemptOwner::notify(&mut self.inner, fact);
    }
    fn observe(&mut self, observation: crate::owner::Observation) {
        crate::owner::AttemptOwner::observe(&mut self.inner, observation);
    }
}

async fn execute_report(
    script: Vec<AttemptFragment>,
    name: &str,
) -> (RecordingOwner, AttemptReport) {
    let scratch = HostedScratch(std::env::temp_dir().join(format!(
        "grapt-hosted-ordering-{}-{name}",
        std::process::id()
    )));
    let artifacts = grapt_artifact::FilesystemArtifactStore::open(
        &scratch.0,
        b"hosted-ordering-fixture".to_vec(),
        Box::new(HostedClock),
    )
    .unwrap_or_else(|error| panic!("hosted fixture artifact store: {error}"));
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let result = run_attempt(
        attempt(name),
        move || ScriptedReader(script.into_iter()),
        Arc::new(NeverTools),
        CapturingRecorder {
            inner: RecordingOwner::new(),
            artifacts,
        },
        cancel_tx,
        cancel_rx,
        residency,
        quiet_step(),
    )
    .await;
    let report = &result.1;
    assert!(matches!(report.outcome, StepOutcome::Ended { .. }));
    (result.0.inner, result.1)
}

async fn execute(script: Vec<AttemptFragment>, name: &str) -> RecordingOwner {
    execute_report(script, name).await.0
}

fn script(terminal: AttemptFragment) -> Vec<AttemptFragment> {
    vec![
        tool_call(call(), "hosted", ToolExecution::ProviderHosted),
        terminal,
        ProviderTerminal {
            finish: FinishReason::Stop,
            usage: None,
            state: AdapterState::none(),
        },
    ]
}

fn assert_terminal_after_fragment(owner: &RecordingOwner, failed: bool, expected_bytes: u64) {
    let facts = &owner.committed;
    let fragment_index = facts.iter().position(|fact| {
        matches!(
            fact,
            Fragment { fragment }
                if matches!(
                    fragment.as_ref(),
                    HostedToolResult { .. } | HostedToolFailed { .. }
                )
        )
    });
    let terminals: Vec<_> = facts
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| match fact {
            ToolTerminal { call, outcome } => Some((index, call, outcome)),
            _ => None,
        })
        .collect();
    assert_eq!(terminals.len(), 1, "hosted call settles exactly once");
    let fragment_index = match fragment_index {
        Some(index) => index,
        None => panic!("hosted terminal fragment must be durable"),
    };
    assert!(
        fragment_index < terminals[0].0,
        "hosted fragment must commit before its durable ToolTerminal"
    );
    assert_eq!(terminals[0].1, &call());
    let artifact = match (terminals[0].2, failed) {
        (
            ToolTerminalOutcome::Completed {
                output: Some(output),
            },
            false,
        ) => output,
        (
            ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::EffectFailed,
                detail: Some(detail),
            },
            true,
        ) => detail,
        other => {
            panic!("hosted result must retain its truthful artifact-backed outcome: {other:?}")
        }
    };
    assert_eq!(artifact.reference().length(), expected_bytes);
    assert!(artifact.disposition().is_complete());
    assert_ne!(terminals[0].2, &ToolTerminalOutcome::ResultMissing);
}

#[tokio::test]
async fn hosted_result_commits_fragment_before_exactly_one_terminal() {
    let owner = execute(
        script(HostedToolResult {
            call: call(),
            tool: "hosted".to_owned(),
            result: "ok".to_owned(),
            state: AdapterState::none(),
        }),
        "att_hosted_result",
    )
    .await;
    assert_terminal_after_fragment(&owner, false, 2);
}

#[tokio::test]
async fn hosted_failure_commits_fragment_before_exactly_one_terminal() {
    let owner = execute(
        script(HostedToolFailed {
            call: call(),
            tool: "hosted".to_owned(),
            error: "nope".to_owned(),
            state: AdapterState::none(),
        }),
        "att_hosted_failed",
    )
    .await;
    assert_terminal_after_fragment(&owner, true, 4);
}

#[tokio::test]
async fn fixture_8_hosted_eof_settles_result_missing_before_attempt_failure() {
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let script = vec![tool_call(call(), "hosted", ToolExecution::ProviderHosted)];
    let (owner, report) = run_attempt(
        attempt("att_hosted_eof"),
        move || ScriptedReader(script.into_iter()),
        Arc::new(NeverTools),
        RecordingOwner::new(),
        cancel_tx,
        cancel_rx,
        residency,
        quiet_step(),
    )
    .await;
    assert!(matches!(report.outcome, StepOutcome::Failed { .. }));
    let facts = &owner.committed;
    let terminal_index = facts.iter().position(|fact| {
        matches!(
            fact,
            ToolTerminal {
                outcome: ToolTerminalOutcome::ResultMissing,
                ..
            }
        )
    });
    let failure_index = facts
        .iter()
        .position(|fact| matches!(fact, AttemptFailed { .. }));
    assert!(matches!(
        (terminal_index, failure_index),
        (Some(terminal), Some(failure)) if terminal < failure
    ));
}

#[tokio::test]
async fn fixture_7_local_tool_provider_failure_joins_and_settles_before_failure() {
    let completed = Arc::new(AtomicBool::new(false));
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let reader_completed = Arc::clone(&completed);
    let (owner, report) = run_attempt(
        attempt("att_fixture_7"),
        move || FailureAfterTool {
            stage: 0,
            completed: reader_completed,
        },
        Arc::new(SignalledTools {
            completed: Arc::clone(&completed),
        }),
        RecordingOwner::new(),
        cancel_tx,
        cancel_rx,
        residency,
        quiet_step(),
    )
    .await;
    assert!(matches!(report.outcome, StepOutcome::Failed { .. }));
    let facts = &owner.committed;
    let terminal = facts.iter().enumerate().find(|(_, fact)| {
        matches!(
            fact,
            DurableFact::ToolTerminal {
                outcome: ToolTerminalOutcome::Completed { .. },
                ..
            }
        )
    });
    let failure_index = facts
        .iter()
        .position(|fact| matches!(fact, AttemptFailed { .. }));
    assert!(matches!(
        (terminal, failure_index),
        (Some((terminal, _)), Some(failure)) if terminal < failure
    ));
}

#[tokio::test]
async fn over_limit_tool_call_is_refused_before_durable_opening() {
    let mut script = Vec::new();
    for ordinal in 0..=ATTEMPT_TOOL_CALL_LIMIT {
        script.push(tool_call(
            call_named(&format!("call_limit_{ordinal}")),
            "hosted",
            ToolExecution::ProviderHosted,
        ));
    }
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let (owner, report) = run_attempt(
        attempt("att_call_limit"),
        move || ScriptedReader(script.into_iter()),
        Arc::new(NeverTools),
        RecordingOwner::new(),
        cancel_tx,
        cancel_rx,
        residency,
        quiet_step(),
    )
    .await;
    assert!(matches!(report.outcome, StepOutcome::Failed { .. }));
    let facts = &owner.committed;
    let openings = facts
        .iter()
        .filter(|fact| {
            matches!(
                fact,
                Fragment { fragment } if matches!(fragment.as_ref(), ToolCalled { .. })
            )
        })
        .count();
    let terminals: Vec<_> = facts
        .iter()
        .enumerate()
        .filter(|(_, fact)| matches!(fact, ToolTerminal { .. }))
        .collect();
    assert_eq!(openings, ATTEMPT_TOOL_CALL_LIMIT);
    assert_eq!(terminals.len(), openings, "every durable opening closes");
    let failure_index = facts
        .iter()
        .position(|fact| matches!(fact, AttemptFailed { .. }));
    assert!(matches!(
        (terminals.last(), failure_index),
        (Some((terminal, _)), Some(failure)) if *terminal < failure
    ));
}

#[tokio::test]
async fn provider_failure_aborts_never_returning_tool_before_attempt_failure() {
    let script = vec![
        tool_call(
            call_named("call_never_returns"),
            "local",
            ToolExecution::Local,
        ),
        ProviderFailure {
            class: ProviderFailureClass::Transport,
            classification: None,
            diagnostic: None,
            message: "provider failed while local tool was running".to_owned(),
            state: AdapterState::none(),
        },
    ];
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        run_attempt(
            attempt("att_provider_failure_abort"),
            move || ScriptedReader(script.into_iter()),
            Arc::new(NeverReturningTools),
            RecordingOwner::new(),
            cancel_tx,
            cancel_rx,
            residency,
            quiet_step(),
        ),
    )
    .await;
    let (owner, report) = match result {
        Ok(result) => result,
        Err(_) => panic!("provider failure waited forever for its local tool"),
    };
    assert!(matches!(report.outcome, StepOutcome::Failed { .. }));
    let facts = &owner.committed;
    let terminal_index = facts
        .iter()
        .position(|fact| matches!(fact, ToolTerminal { .. }));
    let failure_index = facts
        .iter()
        .position(|fact| matches!(fact, AttemptFailed { .. }));
    assert!(matches!(
        (terminal_index, failure_index),
        (Some(terminal), Some(failure)) if terminal < failure
    ));
    assert_eq!(
        facts
            .iter()
            .filter(|fact| matches!(fact, ToolTerminal { .. }))
            .count(),
        1,
        "the aborted tool closes exactly once"
    );
}

#[tokio::test]
async fn same_block_provider_deltas_report_one_exact_coalesced_fragment() {
    let block = grapt_provider::BlockOrdinal::new(0);
    let (owner, report) = execute_report(
        vec![
            TextOpened {
                block,
                state: AdapterState::none(),
            },
            TextDelta {
                block,
                text: "ab".to_owned(),
                state: AdapterState::none(),
            },
            TextDelta {
                block,
                text: "cd".to_owned(),
                state: AdapterState::none(),
            },
            TextClosed {
                block,
                state: AdapterState::none(),
            },
            ProviderTerminal {
                finish: FinishReason::Stop,
                usage: None,
                state: AdapterState::none(),
            },
        ],
        "att_coalesced_diagnostic",
    )
    .await;
    let deltas: Vec<_> = owner
        .committed
        .iter()
        .filter_map(|fact| match fact {
            Fragment { fragment } => match fragment.as_ref() {
                TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["abcd"]);
    assert_eq!(report.diagnostics.provider_fragments_coalesced, 1);
}

/// Drive one bare provider terminal and return what the owner recorded.
///
/// Deliberately not `execute_report`: that helper asserts `StepOutcome::Ended`
/// on the way past, which is the very thing a failure-shaped finish must not
/// produce.
async fn terminal_only(finish: FinishReason, name: &str) -> (RecordingOwner, AttemptReport) {
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let script = vec![ProviderTerminal {
        finish,
        usage: None,
        state: AdapterState::none(),
    }];
    run_attempt(
        attempt(name),
        move || ScriptedReader(script.into_iter()),
        Arc::new(NeverTools),
        RecordingOwner::new(),
        cancel_tx,
        cancel_rx,
        residency,
        quiet_step(),
    )
    .await
}

/// The arm was actually driven by the finish under test.
///
/// Every accepted fragment commits a `DurableFact::Fragment`, so the terminal's
/// own presence in the committed list proves the loop consumed it. Without this
/// the outcome assertions below could score off an earlier guard — a validator
/// refusal or an EOF-with-no-terminal — and still look green.
fn assert_terminal_reached_the_loop(owner: &RecordingOwner, expected: FinishReason) {
    let observed: Vec<FinishReason> = owner
        .committed
        .iter()
        .filter_map(|fact| match fact {
            Fragment { fragment } => match fragment.as_ref() {
                ProviderTerminal { finish, .. } => Some(*finish),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        observed,
        [expected],
        "the loop must have consumed exactly the terminal under test"
    );
}

/// A content-policy refusal is not an ending, and must not be settled as one.
///
/// `FinishReason::ContentFilter` normalizes to `NormalizedFinish::Failed`, and
/// the responses route mints it for real (`routes/responses.rs`, from
/// `incomplete_details.reason == "content_filter"`). Before this witness the
/// terminal arm built `AttemptEnded` from the RAW finish, so the refusal became
/// a success terminal. The only normalization gate in the workspace
/// (`application.rs:581`) runs on `report.result` — after `step.rs:766` has
/// already committed the Step terminal — so it governs routing and cannot
/// govern the durable record.
#[tokio::test]
async fn a_content_policy_finish_settles_as_failure_not_as_an_ending() {
    let (owner, report) = terminal_only(FinishReason::ContentFilter, "att_content_filter").await;

    assert_terminal_reached_the_loop(&owner, FinishReason::ContentFilter);

    let terminals: Vec<&DurableFact> = owner
        .committed
        .iter()
        .filter(|fact| {
            matches!(
                fact,
                DurableFact::AttemptEnded { .. } | DurableFact::AttemptFailed { .. }
            )
        })
        .collect();
    assert_eq!(terminals.len(), 1, "exactly one attempt terminal");
    match terminals[0] {
        AttemptFailed { failure } => assert_eq!(
            failure.reason(),
            &grapt_types::AttemptTerminalReason::ProviderFailure {
                class: grapt_types::ProviderFailureClass::ContentPolicy,
            },
            "a content-policy refusal carries its own typed class"
        ),
        other => panic!("a failure-shaped finish must not commit {other:?}"),
    }
    assert!(
        matches!(report.outcome, StepOutcome::Failed { .. }),
        "the Step must see a failure, not an ending: {:?}",
        report.outcome
    );
}

/// The disposition-bearing finishes are untouched by that classification.
///
/// The mutant that deletes the guard has to stay red for the right reason, and
/// a guard that swallowed every finish would pass the witness above while
/// breaking every ordinary completion.
#[tokio::test]
async fn a_disposition_bearing_finish_still_settles_as_an_ending() {
    for (finish, name) in [
        (FinishReason::Stop, "att_stop"),
        (FinishReason::ToolCalls, "att_tool_calls"),
        (FinishReason::Length, "att_length"),
        (FinishReason::Unknown, "att_unknown"),
    ] {
        let (owner, report) = terminal_only(finish, name).await;
        assert_terminal_reached_the_loop(&owner, finish);
        assert!(
            matches!(report.outcome, StepOutcome::Ended { .. }),
            "{finish:?} carries a disposition and still ends: {:?}",
            report.outcome
        );
        assert!(
            owner
                .committed
                .iter()
                .any(|fact| matches!(fact, DurableFact::AttemptEnded { .. })),
            "{finish:?} must still commit AttemptEnded"
        );
    }
}

#[derive(Debug)]
struct TimingWitness {
    clock: std::sync::atomic::AtomicI64,
    samples: std::sync::mpsc::SyncSender<grapt_types::timing::TimingSample>,
}
impl grapt_types::timing::TimingObserver for TimingWitness {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(self.clock.fetch_add(5, Ordering::Relaxed))
    }
    fn record(&self, sample: grapt_types::timing::TimingSample) {
        assert!(
            self.samples.try_send(sample).is_ok(),
            "timing fixture overflow"
        );
    }
}
struct TimedOwner {
    inner: RecordingOwner,
    context: grapt_types::timing::TimingContext,
}
impl crate::owner::AttemptOwner for TimedOwner {
    fn timing_context(&self) -> Option<grapt_types::timing::TimingContext> {
        Some(self.context.clone())
    }
    fn commit(&mut self, fact: DurableFact) -> Result<(), crate::owner::OwnerError> {
        self.inner.commit(fact)
    }
    fn notify(&mut self, fact: &DurableFact) {
        self.inner.notify(fact);
    }
    fn observe(&mut self, observation: crate::owner::Observation) {
        self.inner.observe(observation);
    }
}
#[tokio::test]
async fn reader_join_and_tool_drain_have_separate_injected_boundaries(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::timing::{TimingClass, TimingContext, TimingScope, TimingStage};
    let (sender, samples) = std::sync::mpsc::sync_channel(8);
    let observer = Arc::new(TimingWitness {
        clock: std::sync::atomic::AtomicI64::new(100),
        samples: sender,
    });
    let attempt = attempt("att_tail_drain");
    let context = TimingContext {
        observer,
        scope: TimingScope {
            session: Some("ses_tail_drain".parse()?),
            attempt: Some(attempt.clone()),
            class: TimingClass::Other,
        },
    };
    let owner = TimedOwner {
        inner: RecordingOwner::new(),
        context: context.clone(),
    };
    let script = vec![ProviderTerminal {
        finish: FinishReason::Stop,
        usage: None,
        state: AdapterState::none(),
    }];
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let (owner, report) = run_attempt(
        attempt,
        move || ScriptedReader(script.into_iter()),
        Arc::new(NeverTools),
        owner,
        cancel_tx,
        cancel_rx,
        DrainResidency::new().token(),
        quiet_step(),
    )
    .await;
    assert!(matches!(report.outcome, StepOutcome::Ended { .. }));
    assert_eq!(owner.inner.attempt_terminal_count(), 1);
    assert_eq!(owner.inner.step_terminal_count(), 0);
    assert_eq!(owner.inner.committed.len(), owner.inner.notified.len());
    let joined = samples.try_recv()?;
    let drained = samples.try_recv()?;
    assert_eq!(joined.stage, TimingStage::ReaderJoin);
    assert_eq!(drained.stage, TimingStage::ToolDrain);
    assert_eq!(joined.scope, context.scope);
    assert_eq!(drained.scope, context.scope);
    assert_eq!(
        (joined.start.as_millis(), joined.end.as_millis()),
        (100, 105)
    );
    assert_eq!(
        (drained.start.as_millis(), drained.end.as_millis()),
        (110, 115)
    );
    assert!(samples.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn missing_hosted_capture_port_refuses_success_instead_of_discarding_output() {
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let residency = DrainResidency::new().token();
    let fragments = script(HostedToolResult {
        call: call(),
        tool: "hosted".into(),
        result: "must survive".into(),
        state: AdapterState::none(),
    });
    let (owner, report) = run_attempt(
        attempt("hosted-capture-refused"),
        move || ScriptedReader(fragments.into_iter()),
        Arc::new(NeverTools),
        RecordingOwner::new(),
        cancel_tx,
        cancel_rx,
        residency,
        quiet_step(),
    )
    .await;
    assert!(matches!(report.outcome, StepOutcome::OwnerRejected { .. }));
    assert!(!owner
        .tool_terminals()
        .iter()
        .any(|(_, outcome)| matches!(outcome, ToolTerminalOutcome::Completed { .. })));
}
