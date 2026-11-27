//! Public Step policy and fail-closed attempt-admission witnesses.

use std::sync::Arc;
use std::time::Duration;

use grapt_provider::{AttemptFragment, ProviderFailureClass};
use grapt_runtime::{
    step_interrupt, AttemptAdmissionRefusal, AttemptPreparation, AttemptSource, BoxFuture,
    CompactionError, DurableFact, HistoryGeneration, HistorySource, RecordingOwner, StepDriver,
    StepPolicy, StepResult, ToolOutcome, ToolRunner,
};
use grapt_types::{PolicyVersion, StepNumber, ToolCallId};

fn test_version() -> PolicyVersion {
    match PolicyVersion::new("step-policy-tests/1") {
        Some(version) => version,
        None => panic!("a non-empty literal must build a PolicyVersion"),
    }
}

#[test]
fn hostile_retry_after_is_capped_by_policy() {
    let policy = match StepPolicy::new_with_max_retry_delay(
        test_version(),
        5,
        Duration::from_secs(2),
        Duration::from_secs(16),
        1,
        Duration::from_secs(30),
    ) {
        Ok(policy) => policy,
        Err(error) => panic!("test policy must be valid: {error}"),
    };
    let hostile = ProviderFailureClass::RateLimit {
        retry_after: Some(Duration::from_secs(u64::MAX)),
    };
    assert_eq!(policy.delay_for(1, &hostile), Duration::from_secs(16));
    assert_eq!(policy.max_retry_delay(), Duration::from_secs(16));
}

#[test]
fn zero_retry_ceiling_is_refused() {
    assert!(StepPolicy::new_with_max_retry_delay(
        test_version(),
        5,
        Duration::from_secs(2),
        Duration::ZERO,
        1,
        Duration::from_secs(30),
    )
    .is_err());
}

struct NeverReader;

impl grapt_runtime::ProviderReader for NeverReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        None
    }
}

struct RefusedSource;

impl AttemptSource for RefusedSource {
    type Reader = NeverReader;

    fn prepare(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        AttemptPreparation::refused(AttemptAdmissionRefusal::NoIssuer)
    }
}

struct FixedHistory;

impl HistorySource for FixedHistory {
    async fn compact(&mut self) -> Result<(), CompactionError> {
        Ok(())
    }

    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
        Ok(HistoryGeneration::new(1))
    }

    fn generation(&self) -> HistoryGeneration {
        HistoryGeneration::new(1)
    }
}

struct NeverTools;

impl ToolRunner for NeverTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: grapt_runtime::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        panic!("attempt refusal must not invoke a tool")
    }
}

#[tokio::test]
async fn public_step_driver_refusal_opens_zero_provider_streams() {
    let mut source = RefusedSource;
    let mut history = FixedHistory;
    let step = match StepNumber::new(1) {
        Some(step) => step,
        None => panic!("step one must be valid"),
    };
    let policy = match StepPolicy::new(
        test_version(),
        1,
        Duration::from_secs(1),
        0,
        Duration::from_secs(1),
    ) {
        Ok(policy) => policy,
        Err(error) => panic!("test policy must be valid: {error}"),
    };
    let (_interrupt, cancellation) = step_interrupt();
    let (owner, report) = StepDriver::run(
        step,
        policy,
        &mut source,
        &mut history,
        Arc::new(NeverTools),
        RecordingOwner::new(),
        cancellation,
    )
    .await;

    assert!(matches!(
        report.result,
        StepResult::AdmissionRefused {
            refusal: AttemptAdmissionRefusal::NoIssuer,
            ..
        }
    ));
    assert!(report.receipts.is_empty());
    assert!(matches!(
        owner.committed.as_slice(),
        [DurableFact::StepFailed { .. }]
    ));
    assert!(matches!(
        owner.notified.as_slice(),
        [DurableFact::StepFailed { .. }]
    ));
    assert!(owner.observations.is_empty());
}
