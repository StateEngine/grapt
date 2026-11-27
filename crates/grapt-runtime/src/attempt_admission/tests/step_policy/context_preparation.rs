use super::*;

struct ThresholdContext {
    source: ScriptSource,
    always_over: bool,
}

impl AttemptSource for ThresholdContext {
    type Reader = Reader;
    fn prepare(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        if self.always_over || generation.value() == 1 {
            return AttemptPreparation::refused(
                crate::AttemptAdmissionRefusal::CompactionRequired {
                    trigger: grapt_types::CompactionTrigger::PolicyThreshold,
                },
            );
        }
        self.source.prepare(step, ordinal, generation)
    }
}

#[tokio::test(start_paused = true)]
async fn cr_proactive_rebuild_precedes_mint_and_repeated_threshold_stops() {
    for always_over in [false, true] {
        let mut source = ThresholdContext {
            source: ScriptSource::new(vec![vec![stop()]]),
            always_over,
        };
        let mut history = FakeHistory::new();
        let (_interrupt, cancel) = step_interrupt();
        let (owner, report) = StepDriver::run(
            step(),
            policy(5),
            &mut source,
            &mut history,
            Arc::new(InstantTools),
            RecordingOwner::new(),
            cancel,
        )
        .await;
        assert_eq!(history.compact_calls, 1);
        assert_eq!(report.compactions, 1);
        assert_eq!(owner.step_terminal_count(), 1);
        if always_over {
            assert_eq!(source.source.mints.load(Ordering::SeqCst), 0);
            assert_eq!(source.source.opens.load(Ordering::SeqCst), 0);
            let StepResult::Failed { failure } = report.result else {
                panic!("repeated threshold did not stop with a typed failure")
            };
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::ContextFailure {
                    failure: ContextFailure::CompactionDidNotFit,
                }
            );
        } else {
            assert_eq!(source.source.mints.load(Ordering::SeqCst), 1);
            assert_eq!(source.source.opens.load(Ordering::SeqCst), 1);
            assert_eq!(source.source.generations(), vec![2]);
            assert!(matches!(report.result, StepResult::Ended { .. }));
        }
    }
}

struct WaitingContext(ScriptSource);
impl AttemptSource for WaitingContext {
    type Reader = <ScriptSource as AttemptSource>::Reader;
    fn prepare(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        self.0.prepare(step, ordinal, generation)
    }
    fn prepare_context(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> BoxFuture<'_, AttemptPreparation<Self::Reader>> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test(start_paused = true)]
async fn scc_context_preparation_interrupt_or_timeout_never_mints_or_opens() {
    for cancelled in [true, false] {
        let mut source = WaitingContext(ScriptSource::new(vec![vec![stop()]]));
        let mut history = FakeHistory::new();
        let (interrupt, cancel) = step_interrupt();
        let driver = StepDriver::run(
            step(),
            policy(5),
            &mut source,
            &mut history,
            Arc::new(InstantTools),
            RecordingOwner::new(),
            cancel,
        );
        let interrupter = async {
            if cancelled {
                tokio::time::sleep(Duration::from_millis(20)).await;
                interrupt.interrupt();
            }
        };
        let ((owner, report), ()) = tokio::join!(driver, interrupter);
        assert_eq!(source.0.mints.load(Ordering::SeqCst), 0);
        assert_eq!(source.0.opens.load(Ordering::SeqCst), 0);
        assert!(report.receipts.is_empty());
        assert_eq!(owner.step_terminal_count(), 1);
        if cancelled {
            let StepResult::Failed { failure } = report.result else {
                panic!("interruption did not fail Step")
            };
            assert_eq!(
                failure.reason(),
                &StepTerminalReason::Interrupted {
                    by: InterruptionSource::User
                }
            );
        } else {
            assert!(matches!(report.result, StepResult::AdmissionRefused { .. }));
        }
    }
}

struct UnavailableContext(ScriptSource);
impl AttemptSource for UnavailableContext {
    type Reader = <ScriptSource as AttemptSource>::Reader;
    fn prepare(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        self.0.prepare(step, ordinal, generation)
    }
    fn prepare_context(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> BoxFuture<'_, AttemptPreparation<Self::Reader>> {
        Box::pin(async {
            AttemptPreparation::refused(crate::AttemptAdmissionRefusal::ContextUnavailable {
                failure: ContextFailure::ContinuityUnavailable,
                continuity: Some(grapt_session::execution::ContinuityFailure::Capability),
            })
        })
    }
}

#[tokio::test]
async fn scc_unavailable_continuity_is_a_typed_terminal_before_provider_admission() {
    let mut source = UnavailableContext(ScriptSource::new(vec![vec![stop()]]));
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let (owner, report) = StepDriver::run(
        step(),
        policy(5),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    )
    .await;
    let StepResult::AdmissionRefused { failure, .. } = report.result else {
        panic!("missing context refusal")
    };
    assert_eq!(
        failure.reason(),
        &StepTerminalReason::ContextFailure {
            failure: ContextFailure::ContinuityUnavailable
        }
    );
    assert_eq!(source.0.mints.load(Ordering::SeqCst), 0);
    assert_eq!(source.0.opens.load(Ordering::SeqCst), 0);
    assert!(report.receipts.is_empty());
    assert_eq!(owner.step_terminal_count(), 1);
}
