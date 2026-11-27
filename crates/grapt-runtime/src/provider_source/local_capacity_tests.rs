//! The production source must carry typed planning contention to its driver.
use super::*;
use crate::provider_source::ContextAdmissionRefusal;

#[derive(Default)]
struct AdmissionCounts {
    plans: AtomicUsize,
    refreshes: AtomicUsize,
    admissions: AtomicUsize,
    waiting: tokio::sync::Notify,
}

struct OneShotPlan {
    inner: RegistryPlan,
    counts: Arc<AdmissionCounts>,
    busy: usize,
}

impl AttemptPlan for OneShotPlan {
    fn refresh_context(
        &mut self,
        _step: StepNumber,
        _generation: HistoryGeneration,
    ) -> BoxFuture<'_, Result<(), ContextPreparationFailure>> {
        self.counts.refreshes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn retrying(&mut self, notice: Option<crate::step::RetryNotice>) {
        if notice.is_some() {
            self.counts.waiting.notify_one();
        }
    }

    fn plan(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        if self.counts.plans.fetch_add(1, Ordering::SeqCst) != 0 {
            return Err(PlanRefusal::new(
                "one-shot request input was already consumed".into(),
                None,
            ));
        }
        self.inner.plan(step, ordinal, generation)
    }

    fn context_admission(
        &mut self,
        _planned: &PlannedAttempt,
    ) -> Result<Option<grapt_types::CompactionTrigger>, ContextAdmissionRefusal> {
        if self.counts.admissions.fetch_add(1, Ordering::SeqCst) < self.busy {
            Err(ContextAdmissionRefusal::CapacityBusy)
        } else {
            Ok(None)
        }
    }
}

fn one_shot_source(busy: usize) -> (Harness, Arc<AdmissionCounts>, Arc<SeqAttemptIds>) {
    let counts = Arc::new(AdmissionCounts::default());
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_late_capacity",
        opener,
        Box::new(OneShotPlan {
            inner: RegistryPlan {
                registry: ModelRegistry::install(catalog("late-capacity")),
            },
            counts: Arc::clone(&counts),
            busy,
        }),
    );
    let ids = Arc::new(SeqAttemptIds(AtomicUsize::new(0)));
    harness.source.ids = ids.clone();
    (harness, counts, ids)
}

#[tokio::test(start_paused = true)]
async fn local_capacity_late_admission_reuses_one_shot_input_until_capacity_recovers() {
    let (harness, counts, _ids) = one_shot_source(7);
    let (harness, report) = tokio::time::timeout(
        Duration::from_secs(1000),
        run(harness, policy(Duration::from_secs(1))),
    )
    .await
    .unwrap_or_else(|_| panic!("late admission never recovered"));
    assert!(
        matches!(report.result, StepResult::Ended { .. }),
        "{report:?}"
    );
    assert_eq!(
        counts.plans.load(Ordering::SeqCst),
        1,
        "one-shot request was rebuilt"
    );
    assert_eq!(
        counts.refreshes.load(Ordering::SeqCst),
        1,
        "continuity was refreshed again"
    );
    assert_eq!(counts.admissions.load(Ordering::SeqCst), 8);
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(
        report.receipts.first().map(|receipt| receipt.ordinal),
        Some(1)
    );
    assert_eq!(harness.source.ledger().held(), 0);
}

#[tokio::test(start_paused = true)]
async fn local_capacity_cancelling_late_admission_sends_nothing_and_joins_shutdown() {
    let (harness, counts, ids) = one_shot_source(usize::MAX);
    let (interrupt, cancellation) = step_interrupt();
    let stopping = async {
        counts.waiting.notified().await;
        interrupt.interrupt();
    };
    let ((mut harness, report), ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(
            run_with(harness, policy(Duration::from_secs(2)), cancellation),
            stopping
        )
    })
    .await
    .unwrap_or_else(|_| panic!("late admission did not enter cancellable waiting"));
    assert!(matches!(report.result, StepResult::Failed { ref failure }
        if failure.reason() == &StepTerminalReason::Interrupted { by: InterruptionSource::User }));
    assert_eq!(ids.0.load(Ordering::SeqCst), 0);
    assert!(report.receipts.is_empty());
    assert_eq!(harness.source.ledger().held(), 0);
    assert_eq!(counts.plans.load(Ordering::SeqCst), 1);
    assert_eq!(counts.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(counts.admissions.load(Ordering::SeqCst), 1);
    harness
        .source
        .shutdown()
        .await
        .unwrap_or_else(|error| panic!("shutdown failed: {error}"));
    assert_eq!(ids.0.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn local_capacity_retained_input_does_not_cross_step_ordinal_generation_or_promotion() {
    for change in ["step", "ordinal", "generation", "promotion"] {
        let (mut harness, counts, ids) = one_shot_source(usize::MAX);
        let _first = harness
            .source
            .prepare(harness.step, 1, HistoryGeneration::new(0));
        assert_eq!(counts.admissions.load(Ordering::SeqCst), 1);
        let mut ordinal = 1;
        let mut generation = HistoryGeneration::new(0);
        match change {
            "step" => {
                harness.step = StepNumber::new(2).unwrap_or_else(|| panic!("step"));
                harness.source.begin_step(StepTerminalHandoff::new(
                    harness.step,
                    "evt_late_capacity_successor"
                        .parse()
                        .unwrap_or_else(|error| panic!("event: {error}")),
                    100,
                ));
            }
            "ordinal" => ordinal = 2,
            "generation" => generation = HistoryGeneration::new(1),
            "promotion" => harness
                .source
                .promote_input(
                    &"msg_late_capacity_followup"
                        .parse()
                        .unwrap_or_else(|error| panic!("input: {error}")),
                )
                .unwrap_or_else(|error| panic!("promotion failed: {error}")),
            _ => unreachable!(),
        }
        let _next = harness.source.prepare(harness.step, ordinal, generation);
        assert_eq!(
            counts.plans.load(Ordering::SeqCst),
            2,
            "{change} reused old request input"
        );
        assert_eq!(ids.0.load(Ordering::SeqCst), 0);
        assert_eq!(harness.source.ledger().held(), 0);
    }
}

struct RefusingPlan(PlanRefusal);
impl AttemptPlan for RefusingPlan {
    fn plan(
        &mut self,
        _step: StepNumber,
        _ordinal: u32,
        _generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        Err(self.0.clone())
    }
}

#[test]
fn local_capacity_production_source_preserves_busy_before_identity_and_receipt() {
    verify_refusal(PlanRefusal::capacity_busy(), true);
}

#[test]
fn local_capacity_production_source_does_not_promote_capacity_like_prose_to_busy() {
    verify_refusal(
        PlanRefusal::new(
            "permission denied: read capacity is busy; retry the request".into(),
            None,
        ),
        false,
    );
}

fn verify_refusal(refusal: PlanRefusal, busy: bool) {
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let mut harness = harness(
        "ses_local_capacity_admission",
        opener,
        Box::new(RefusingPlan(refusal)),
    );
    let ids = Arc::new(SeqAttemptIds(AtomicUsize::new(0)));
    harness.source.ids = ids.clone();
    let preparation = harness
        .source
        .prepare(harness.step, 1, HistoryGeneration::new(0));
    let (_handle, cancellation) = step_interrupt();
    let result = crate::attempt_admission::AttemptProtocol::prepare(
        preparation,
        Arc::new(NoTools),
        RecordingOwner::new(),
        cancellation.residency(),
        cancellation.signal(),
    );
    match result {
        Err((_, actual)) if busy => assert_eq!(actual, AttemptAdmissionRefusal::CapacityBusy),
        Err((_, actual)) => assert!(matches!(
            actual,
            AttemptAdmissionRefusal::PlanRefused { .. }
        )),
        Ok(_) => panic!("a refused plan admitted a provider attempt"),
    }
    assert_eq!(ids.0.load(Ordering::SeqCst), 0, "refusal minted an attempt");
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert_eq!(harness.source.ledger().held(), 0);
}
