//! Local contention is a pre-admission wait, never a physical provider retry.
use super::*;
use crate::step::RetryNotice;
use crate::AttemptAdmissionRefusal;
use tokio::sync::Notify;
use tokio::time::Instant;

struct CapacitySource {
    source: ScriptSource,
    busy: usize,
    terminal: Option<AttemptAdmissionRefusal>,
    calls: Vec<(StepNumber, u32, u64, Instant)>,
    notices: Vec<Option<RetryNotice>>,
    waiting: Arc<Notify>,
}

impl CapacitySource {
    fn new(busy: usize) -> Self {
        Self {
            source: ScriptSource::new(vec![vec![stop()]]),
            busy,
            terminal: None,
            calls: Vec::new(),
            notices: Vec::new(),
            waiting: Arc::new(Notify::new()),
        }
    }
}

impl AttemptSource for CapacitySource {
    type Reader = Reader;

    fn prepare(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        self.calls
            .push((step, ordinal, generation.value(), Instant::now()));
        if self.calls.len() <= self.busy {
            assert_eq!(self.source.mints.load(Ordering::SeqCst), 0);
            assert_eq!(self.source.opens.load(Ordering::SeqCst), 0);
            return AttemptPreparation::refused(AttemptAdmissionRefusal::CapacityBusy);
        }
        if let Some(refusal) = &self.terminal {
            return AttemptPreparation::refused(refusal.clone());
        }
        self.source.prepare(step, ordinal, generation)
    }

    fn retrying(&mut self, notice: Option<RetryNotice>) {
        self.notices.push(notice);
        if notice.is_some() {
            self.waiting.notify_one();
        }
    }
}

async fn recovers(busy: usize, dropped_handle: bool) {
    let mut source = CapacitySource::new(busy);
    let mut history = FakeHistory::new();
    // Only one physical attempt is allowed, even after hundreds of local waits.
    let policy = StepPolicy::new_with_max_retry_delay(
        test_version(),
        1,
        Duration::from_secs(2),
        Duration::from_secs(8),
        1,
        Duration::from_secs(30),
    )
    .unwrap_or_else(|error| panic!("test policy invalid: {error}"));
    let ceiling = policy.max_retry_delay();
    let (interrupt, cancel) = step_interrupt();
    let retained = if dropped_handle {
        drop(interrupt);
        None
    } else {
        Some(interrupt)
    };
    let (owner, report) = tokio::time::timeout(
        Duration::from_secs(10_000),
        StepDriver::run(
            step(),
            policy,
            &mut source,
            &mut history,
            Arc::new(InstantTools),
            RecordingOwner::new(),
            cancel,
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("capacity recovery never completed"));
    drop(retained);
    assert!(
        matches!(report.result, StepResult::Ended { .. }),
        "{report:?}"
    );
    assert_eq!(source.calls.len(), busy + 1);
    assert!(
        source.calls.iter().all(|(s, ordinal, generation, _)| {
            *s == step() && *ordinal == 1 && *generation == 1
        }),
        "local contention consumed or replaced the coding coordinate"
    );
    assert_eq!(source.source.mints.load(Ordering::SeqCst), 1);
    assert_eq!(source.source.opens.load(Ordering::SeqCst), 1);
    assert_eq!(report.receipts.len(), 1);
    assert_eq!(report.compactions, 0);
    assert_eq!(history.compact_calls, 0);
    assert_eq!(owner.step_terminal_count(), 1);
    let notices: Vec<_> = source.notices.iter().flatten().collect();
    assert_eq!(
        notices.len(),
        busy,
        "each contention must publish its waiting state"
    );
    assert!(
        matches!(source.notices.last(), Some(None)),
        "waiting state was not cleared"
    );
    for (index, notice) in notices.iter().enumerate() {
        assert!(notice.after > Duration::ZERO && notice.after <= ceiling);
        assert_eq!(notice.attempt, 1, "waiting advanced the provider ordinal");
        let waited = notice
            .capacity_wait
            .unwrap_or_else(|| panic!("capacity notice lost its typed waiting cause"));
        assert!(waited >= source.calls[index].3 - source.calls[0].3);
        assert!(
            notice.class.is_none(),
            "local contention was labelled a provider failure"
        );
        assert!(
            source.calls[index + 1].3 - source.calls[index].3 >= notice.after,
            "contention retried without its advertised backoff"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn local_capacity_busy_recovers_before_one_provider_send() {
    recovers(1, false).await;
}

#[tokio::test(start_paused = true)]
async fn local_capacity_prolonged_contention_never_spends_the_attempt_limit() {
    recovers(257, false).await;
}

#[tokio::test(start_paused = true)]
async fn local_capacity_dropped_interrupt_handle_preserves_backoff_and_recovery() {
    recovers(3, true).await;
}

#[tokio::test(start_paused = true)]
async fn local_capacity_cancellation_stops_waiting_without_a_provider_send() {
    let mut source = CapacitySource::new(usize::MAX);
    let waiting = Arc::clone(&source.waiting);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    let driver = StepDriver::run(
        step(),
        policy(1),
        &mut source,
        &mut history,
        Arc::new(InstantTools),
        RecordingOwner::new(),
        cancel,
    );
    let stopping = async {
        waiting.notified().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        interrupt.interrupt();
    };
    let started = Instant::now();
    let ((owner, report), ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(driver, stopping)
    })
    .await
    .unwrap_or_else(|_| panic!("capacity waiting ignored cancellation"));
    assert!(Instant::now() - started < Duration::from_secs(1));
    assert!(matches!(report.result, StepResult::Failed { ref failure }
        if failure.reason() == &StepTerminalReason::Interrupted { by: InterruptionSource::User }));
    assert_eq!(source.calls.len(), 1);
    assert_eq!(source.source.mints.load(Ordering::SeqCst), 0);
    assert_eq!(source.source.opens.load(Ordering::SeqCst), 0);
    assert!(report.receipts.is_empty());
    assert_eq!(owner.step_terminal_count(), 1);
    assert!(matches!(source.notices.last(), Some(None)));
}

#[tokio::test(start_paused = true)]
async fn local_capacity_genuine_refusals_stay_terminal_despite_capacity_like_text() {
    for refusal in [
        AttemptAdmissionRefusal::PlanRefused {
            detail: "permission denied: read capacity is busy; retry the request".into(),
        },
        AttemptAdmissionRefusal::PlanRefused {
            detail: "invalid request plan".into(),
        },
        AttemptAdmissionRefusal::OpenRefused {
            detail: "policy does not permit this request".into(),
        },
        AttemptAdmissionRefusal::ContextUnavailable {
            failure: ContextFailure::ContinuityUnavailable,
            continuity: Some(grapt_session::execution::ContinuityFailure::Capability),
        },
    ] {
        let mut source = CapacitySource::new(0);
        source.terminal = Some(refusal.clone());
        let mut history = FakeHistory::new();
        let (_interrupt, cancel) = step_interrupt();
        let (owner, report) = tokio::time::timeout(
            Duration::from_secs(1),
            StepDriver::run(
                step(),
                policy(1),
                &mut source,
                &mut history,
                Arc::new(InstantTools),
                RecordingOwner::new(),
                cancel,
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("a genuine local refusal entered retry"));
        assert!(
            matches!(report.result, StepResult::AdmissionRefused { refusal: ref actual, .. }
            if actual == &refusal)
        );
        assert_eq!(source.calls.len(), 1);
        assert!(source.notices.is_empty());
        assert_eq!(source.source.mints.load(Ordering::SeqCst), 0);
        assert_eq!(source.source.opens.load(Ordering::SeqCst), 0);
        assert!(report.receipts.is_empty());
        assert_eq!(owner.step_terminal_count(), 1);
    }
}
