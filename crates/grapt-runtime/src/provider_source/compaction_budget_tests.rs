//! A compaction's further summary call is admitted only after the previous
//! call ENDED and committed nothing, within its own call bound. The calls are
//! not charged against the Step's coding attempt budget.
use super::*;
use crate::step::{CompactionError, CompactionFailure, HistorySource, MAX_TOTAL_ATTEMPTS};
use crate::CompactionContext;
use grapt_provider::FailureClassification;
use grapt_types::InterruptionSource;

fn context(
    harness: &Harness,
    cancellation: crate::Cancellation,
    max_attempts: u32,
) -> CompactionContext {
    CompactionContext {
        trigger: grapt_types::CompactionTrigger::ContextOverflow,
        step: harness.step,
        preceding_ordinal: 1,
        generation: HistoryGeneration::new(0),
        cancellation,
        max_attempts,
    }
}

/// A summary call's future, admitted and not yet returned.
type PendingCall<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    (RecordingOwner, crate::supervisor::AttemptReport),
                    super::SummaryRefusal,
                >,
            > + Send
            + 'a,
    >,
>;

/// A first summary call admitted and polled once: still running.
struct Pending<'a> {
    planner: RegistryPlan,
    context: CompactionContext,
    first: Option<AttemptPreparation<ProviderStreamReader>>,
    running: PendingCall<'a>,
}

fn pending<'a>(
    harness: &mut Harness,
    port: &'a mut ProviderCompactionCall,
    label: &str,
) -> Result<Pending<'a>, Box<dyn std::error::Error>> {
    let mut planner = RegistryPlan {
        registry: ModelRegistry::install(catalog(&format!("{label}-plan"))),
    };
    let first = harness
        .source
        .prepare(harness.step, 1, HistoryGeneration::new(0));
    let (_handle, cancellation) = step_interrupt();
    let context = context(harness, cancellation, MAX_TOTAL_ATTEMPTS);
    let planned = planner.plan(harness.step, 2, context.generation)?;
    let mut running = Box::pin(port.run(context.clone(), planned, RecordingOwner::new()));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(
        std::future::Future::poll(running.as_mut(), &mut cx).is_pending(),
        "the first summary call must still be running for this witness"
    );
    Ok(Pending {
        planner,
        context,
        first: Some(first),
        running,
    })
}

fn summary_harness(label: &str) -> Harness {
    let opener = Arc::new(AttemptOpener::deterministic(
        [
            transcript_script(),
            transcript_script(),
            transcript_script(),
            transcript_script(),
            transcript_script(),
        ],
        route(),
        credential(),
    ));
    harness(
        label,
        opener,
        Box::new(RegistryPlan {
            registry: ModelRegistry::install(catalog(label)),
        }),
    )
}

/// While the first summary call runs nothing has ended, so a second port
/// sharing the binding is refused. Once the first call returns (its attempt
/// joined, nothing committed), further calls are admitted up to the call bound.
#[tokio::test]
async fn scc_no_fallback_until_the_first_summary_call_has_ended(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut harness = summary_harness("ses_scc_running");
    let mut port = harness.source.compaction_call();
    let mut other = harness.source.compaction_call();
    let binding = harness.source.compaction.clone();
    let mut first = pending(&mut harness, &mut port, "ses_scc_running")?;
    assert!(super::compaction::lock(&binding).running);
    let concurrent = first
        .planner
        .plan(harness.step, 3, first.context.generation)?;
    let refused = other
        .run(first.context.clone(), concurrent, RecordingOwner::new())
        .await
        .err()
        .ok_or("a second summary call was admitted while the first was running")?;
    assert_eq!(refused, "summary call is repeated or names stale history");
    first.running.as_mut().await?;
    assert!(!super::compaction::lock(&binding).running);
    let fallback = first
        .planner
        .plan(harness.step, 3, first.context.generation)?;
    other
        .run(first.context.clone(), fallback, RecordingOwner::new())
        .await?;
    let repair = first
        .planner
        .plan(harness.step, 4, first.context.generation)?;
    other
        .run(first.context.clone(), repair, RecordingOwner::new())
        .await?;
    let fourth = first
        .planner
        .plan(harness.step, 5, first.context.generation)?;
    other
        .run(first.context.clone(), fourth, RecordingOwner::new())
        .await?;
    let fifth = first
        .planner
        .plan(harness.step, 6, first.context.generation)?;
    let refused = other
        .run(first.context.clone(), fifth, RecordingOwner::new())
        .await
        .err()
        .ok_or("a fifth compaction call was admitted")?;
    assert_eq!(refused, "summary call is repeated or names stale history");
    drop(first.first.take());
    Ok(())
}

/// A first call DROPPED mid-flight never ended: its attempt was not joined
/// and its receipt is unsettled, so no fallback follows it in this Step.
#[tokio::test]
async fn scc_a_dropped_first_summary_call_admits_no_fallback(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut harness = summary_harness("ses_scc_dropped");
    let mut port = harness.source.compaction_call();
    let mut other = harness.source.compaction_call();
    let binding = harness.source.compaction.clone();
    let Pending {
        mut planner,
        context,
        first: prepared,
        running,
    } = pending(&mut harness, &mut port, "ses_scc_dropped")?;
    drop(running);
    assert!(
        super::compaction::lock(&binding).running,
        "a dropped call must not count as ended"
    );
    let fallback = planner.plan(harness.step, 3, context.generation)?;
    let refused = other
        .run(context.clone(), fallback, RecordingOwner::new())
        .await
        .err()
        .ok_or("a fallback was admitted after a dropped first call")?;
    assert_eq!(refused, "summary call is repeated or names stale history");
    drop(prepared);
    Ok(())
}

/// A Step policy below the hard cap bounds summary calls too: with two
/// attempts allowed, the summary takes the second and its fallback is refused.
#[tokio::test]
async fn scc_summary_calls_have_their_own_bound_and_ignore_the_policy_budget(
) -> Result<(), Box<dyn std::error::Error>> {
    let opener = Arc::new(AttemptOpener::deterministic(
        (0..5).map(|_| transcript_script()).collect::<Vec<_>>(),
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_scc_budget",
        opener,
        Box::new(RegistryPlan {
            registry: ModelRegistry::install(catalog("scc-budget")),
        }),
    );
    let mut planner = RegistryPlan {
        registry: ModelRegistry::install(catalog("scc-budget-plan")),
    };
    let first = harness
        .source
        .prepare(harness.step, 1, HistoryGeneration::new(0));
    let (_handle, cancellation) = step_interrupt();
    // The policy budget is already spent by the preceding coding attempt.
    let context = context(&harness, cancellation, 1);
    let mut port = harness.source.compaction_call();
    for call in 2..=5 {
        let planned = planner.plan(harness.step, call, context.generation)?;
        port.run(context.clone(), planned, RecordingOwner::new())
            .await?;
    }
    let fifth = planner.plan(harness.step, 6, context.generation)?;
    let refused = port
        .run(context.clone(), fifth, RecordingOwner::new())
        .await
        .err()
        .ok_or("a fifth summary call was admitted")?;
    assert_eq!(refused, "summary call is repeated or names stale history");
    drop(first);
    Ok(())
}

/// A history whose compaction makes the summary call AND its fallback, as
/// the product's provider compaction does when the first call commits
/// nothing.
struct FallbackHistory {
    port: ProviderCompactionCall,
    planner: RegistryPlan,
    generation: HistoryGeneration,
}

impl HistorySource for FallbackHistory {
    async fn compact(&mut self) -> Result<(), CompactionError> {
        Err(CompactionError {
            reason: "the witness compacts through compact_with".to_owned(),
        })
    }

    async fn compact_with(
        &mut self,
        context: CompactionContext,
    ) -> Result<Result<(), CompactionFailure>, InterruptionSource> {
        for call in 1..=2 {
            let ordinal = context.preceding_ordinal + call;
            let planned = match self.planner.plan(context.step, ordinal, context.generation) {
                Ok(planned) => planned,
                Err(refusal) => {
                    return Ok(Err(CompactionFailure::Failed(CompactionError {
                        reason: refusal.detail,
                    })))
                }
            };
            if let Err(reason) = self
                .port
                .run(context.clone(), planned, RecordingOwner::new())
                .await
            {
                return Ok(Err(CompactionFailure::Failed(CompactionError {
                    reason: reason.into(),
                })));
            }
        }
        Ok(Ok(()))
    }

    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
        self.generation = HistoryGeneration::new(self.generation.value() + 1);
        Ok(self.generation)
    }

    fn generation(&self) -> HistoryGeneration {
        self.generation
    }
}

/// Cap 5, two traces. First trace: coding 1 overflows; two summary calls; the
/// rebuilt coding attempts fail retryably. Second trace: coding 1 and 2 fail
/// retryably, coding 3 overflows, two summary calls. Either way the Step runs
/// all FIVE coding attempts under its policy limit, never a sixth: the summary
/// calls spend none of that budget.
#[tokio::test]
async fn scc_the_step_does_not_charge_summary_calls_against_its_attempt_bound(
) -> Result<(), Box<dyn std::error::Error>> {
    let overflow = || {
        ScriptBuilder::new().fail(
            ProviderFailureClass::ProviderInternal { retry_after: None },
            Some(FailureClassification::ContextOverflow),
            "context overflow",
        )
    };
    let retryable = || {
        ScriptBuilder::new().fail(
            ProviderFailureClass::ProviderInternal { retry_after: None },
            None,
            "provider internal",
        )
    };
    for (label, scripts) in [
        (
            "ses_scc_charged_early",
            vec![
                overflow(),
                transcript_script(),
                transcript_script(),
                retryable(),
                retryable(),
                retryable(),
                retryable(),
            ],
        ),
        (
            "ses_scc_charged_late",
            vec![
                retryable(),
                retryable(),
                overflow(),
                transcript_script(),
                transcript_script(),
                retryable(),
                retryable(),
            ],
        ),
    ] {
        let opener = Arc::new(AttemptOpener::deterministic(scripts, route(), credential()));
        let mut harness = harness(
            label,
            opener,
            Box::new(RegistryPlan {
                registry: ModelRegistry::install(catalog(label)),
            }),
        );
        let mut history = FallbackHistory {
            port: harness.source.compaction_call(),
            planner: RegistryPlan {
                registry: ModelRegistry::install(catalog(&format!("{label}-plan"))),
            },
            generation: HistoryGeneration::new(0),
        };
        let owner = harness.take_owner();
        let (_handle, cancellation) = step_interrupt();
        let (owner, report) = StepDriver::run(
            harness.step,
            policy(Duration::from_millis(1)),
            &mut harness.source,
            &mut history,
            Arc::new(NoTools),
            owner,
            cancellation,
        )
        .await;
        harness.owner = Some(owner);
        assert_eq!(
            super::compaction::lock(&harness.source.compaction).calls,
            2,
            "{label}"
        );
        assert_eq!(
            report.receipts.len(),
            5,
            "{label}: the whole coding budget beside two summary calls: {:?}",
            report.receipts
        );
        assert!(
            matches!(
                &report.result,
                StepResult::Failed { failure }
                    if matches!(failure.reason(), StepTerminalReason::PolicyLimit { .. })
            ),
            "{label}: {:?}",
            report.result
        );
        assert_eq!(harness.source.ledger().held(), 0, "{label}");
    }
    Ok(())
}

/// SC-REL-07a: a structural capacity refusal that a committed compaction does
/// not clear ends the Step as bounded no progress: one compaction, no loop.
#[tokio::test]
async fn sc_rel_07a_a_structural_refusal_compaction_cannot_clear_ends_as_no_progress() {
    struct Reloading {
        generation: u64,
        compactions: u32,
    }
    impl HistorySource for Reloading {
        async fn compact(&mut self) -> Result<(), CompactionError> {
            self.compactions += 1;
            // Past the bound this fails instead of spinning, so a lost bound
            // is a failed assertion, not a hung test.
            if self.compactions > 3 {
                return Err(CompactionError {
                    reason: "compaction looped past its bound".to_owned(),
                });
            }
            Ok(())
        }
        fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
            self.generation += 1;
            Ok(HistoryGeneration::new(self.generation))
        }
        fn generation(&self) -> HistoryGeneration {
            HistoryGeneration::new(self.generation)
        }
    }
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_rel07_no_progress",
        opener,
        Box::new(StructuralRefusalPlan),
    );
    let owner = harness.take_owner();
    let mut history = Reloading {
        generation: 0,
        compactions: 0,
    };
    let (_handle, cancellation) = step_interrupt();
    let run = StepDriver::run(
        harness.step,
        policy(Duration::from_millis(2)),
        &mut harness.source,
        &mut history,
        Arc::new(NoTools),
        owner,
        cancellation,
    );
    let (_, report) = run.await;
    assert_eq!(history.compactions, 1, "one bounded compaction");
    assert_eq!(report.compactions, 1);
    match &report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::ContextFailure {
                failure: grapt_types::ContextFailure::CompactionNoProgress,
            }
        ),
        other => panic!("an uncleared structural refusal must stop as no progress: {other:?}"),
    }
}
