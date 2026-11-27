//! Closed context diagnosis before provider admission.
use super::plan::UnavailableContinuityPlan;
use super::*;

#[tokio::test(start_paused = true)]
async fn native_continuity_refusal_preserves_cause_before_identity_and_reservation() {
    use grapt_session::execution::ContinuityFailure;
    for cause in [
        ContinuityFailure::Capability,
        ContinuityFailure::Transport,
        ContinuityFailure::Principal,
        ContinuityFailure::TimedOut { seconds: 7 },
    ] {
        let opener = Arc::new(AttemptOpener::deterministic(
            [transcript_script()],
            route(),
            credential(),
        ));
        let mut harness = harness(
            "ses_native_continuity",
            opener,
            Box::new(UnavailableContinuityPlan(cause)),
        );
        let ids = Arc::new(SeqAttemptIds(AtomicUsize::new(0)));
        harness.source.ids = ids.clone();
        let (harness, report) = run(harness, policy(Duration::from_millis(5))).await;
        let StepResult::AdmissionRefused { refusal, failure } = report.result else {
            panic!("continuity was not refused")
        };
        assert_eq!(
            refusal,
            AttemptAdmissionRefusal::ContextUnavailable {
                failure: grapt_types::ContextFailure::ContinuityUnavailable,
                continuity: Some(cause),
            }
        );
        assert_eq!(
            failure.reason(),
            &StepTerminalReason::ContextFailure {
                failure: grapt_types::ContextFailure::ContinuityUnavailable,
            }
        );
        assert_eq!(ids.0.load(Ordering::SeqCst), 0);
        assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
        assert_eq!(harness.source.ledger().held(), 0);
        assert!(report.receipts.is_empty());
        assert_eq!(
            terminal_rows(&harness.inbox, &harness.session, "session.step.failed.3"),
            1
        );
    }
}

struct ContextLease(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for ContextLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct ClosingPlan {
    inner: super::plan::RegistryPlan,
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    active: Arc<std::sync::atomic::AtomicBool>,
    closed: Arc<AtomicUsize>,
    pending: bool,
    close_error: bool,
}
impl AttemptPlan for ClosingPlan {
    fn refresh_context(
        &mut self,
        _: StepNumber,
        _: HistoryGeneration,
    ) -> BoxFuture<'_, Result<(), ContextPreparationFailure>> {
        Box::pin(async move {
            self.active.store(true, Ordering::SeqCst);
            let _lease = ContextLease(self.active.clone());
            if let Some(entered) = self.entered.take() {
                let _closed = entered.send(());
            }
            if self.pending {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
    fn finish_context(&mut self, cancelled: bool) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            assert!(
                !self.active.load(Ordering::SeqCst),
                "cleanup ran while preparation still borrowed its owner"
            );
            assert_eq!(cancelled, self.pending);
            tokio::task::yield_now().await;
            self.closed.fetch_add(1, Ordering::SeqCst);
            if self.close_error {
                Err("named context cleanup failure".into())
            } else {
                Ok(())
            }
        })
    }
    fn plan(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> Result<PlannedAttempt, PlanRefusal> {
        self.inner.plan(step, ordinal, generation)
    }
}
fn closing_plan(
    pending: bool,
    close_error: bool,
) -> (
    Box<dyn AttemptPlan>,
    tokio::sync::oneshot::Receiver<()>,
    Arc<AtomicUsize>,
) {
    let (entered, signal) = tokio::sync::oneshot::channel();
    let closed = Arc::new(AtomicUsize::new(0));
    (
        Box::new(ClosingPlan {
            inner: super::plan::RegistryPlan {
                registry: ModelRegistry::install(catalog("native-context-cleanup")),
            },
            entered: Some(entered),
            active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            closed: closed.clone(),
            pending,
            close_error,
        }),
        signal,
        closed,
    )
}
#[tokio::test]
async fn cancelled_context_is_dropped_then_closed_before_interrupted_terminal() {
    for close_error in [false, true] {
        let opener = Arc::new(AttemptOpener::deterministic(
            [transcript_script()],
            route(),
            credential(),
        ));
        let (plan, entered, closed) = closing_plan(true, close_error);
        let harness = harness("ses_context_cancel_close", opener, plan);
        let (handle, cancellation) = step_interrupt();
        let (result, _) = tokio::join!(
            run_with(harness, policy(Duration::from_millis(5)), cancellation),
            async {
                assert!(entered.await.is_ok());
                handle.interrupt();
            }
        );
        let (harness, report) = result;
        assert!(matches!(report.result, StepResult::Failed { ref failure }
            if failure.reason() == &StepTerminalReason::Interrupted { by: InterruptionSource::User }));
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(harness.source.ledger().held(), 0);
        assert!(report.receipts.is_empty());
    }
}
#[tokio::test]
async fn context_deadline_closes_before_any_provider_identity_or_receipt() {
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let (plan, _entered, closed) = closing_plan(true, false);
    let harness = harness("ses_context_timeout_close", opener, plan);
    let policy = StepPolicy::new(
        PolicyVersion::new("context-timeout").unwrap_or_else(|| panic!("policy")),
        1,
        Duration::from_millis(1),
        0,
        Duration::from_millis(20),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let (harness, report) = run(harness, policy).await;
    let StepResult::AdmissionRefused { refusal, failure } = report.result else {
        panic!("a context deadline must refuse admission: {report:?}")
    };
    assert!(
        matches!(
            refusal,
            AttemptAdmissionRefusal::ContextPreparationTimedOut { .. }
        ),
        "{refusal:?}"
    );
    assert_eq!(
        failure.reason(),
        &StepTerminalReason::PolicyLimit {
            policy: PolicyVersion::new("context-timeout").unwrap_or_else(|| panic!("policy")),
        },
        "an unopened provider must not be blamed for context exhaustion"
    );
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(receipt_row(&harness.inbox, &attempt("pat_u2_1")), None);
    assert!(report.receipts.is_empty());
}
#[tokio::test]
async fn unconfirmed_context_cleanup_cannot_open_a_prepared_provider_stream() {
    let opener = Arc::new(AttemptOpener::deterministic(
        [transcript_script()],
        route(),
        credential(),
    ));
    let (plan, _entered, closed) = closing_plan(false, true);
    let harness = harness("ses_context_close_refuse", opener, plan);
    let (harness, report) = run(harness, policy(Duration::from_millis(5))).await;
    assert!(matches!(report.result, StepResult::AdmissionRefused { .. }));
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert!(
        report.receipts.is_empty(),
        "cleanup failure bought a provider stream"
    );
    assert_eq!(harness.source.ledger().held(), 0);
}
