use super::*;
use crate::step::RetryNotice;

type Notices = Arc<Mutex<Vec<Option<RetryNotice>>>>;

struct ObservedPlan {
    inner: RegistryPlan,
    notices: Notices,
    ready: Arc<tokio::sync::Notify>,
}
impl AttemptPlan for ObservedPlan {
    fn retrying(&mut self, notice: Option<RetryNotice>) {
        let mut notices = self
            .notices
            .lock()
            .unwrap_or_else(|error| panic!("notice mutex: {error}"));
        assert!(notices.len() < 4, "bounded fixture notice count");
        notices.push(notice);
        if notice.is_some() {
            self.ready.notify_one();
        }
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
fn notice_plan(label: &str) -> (Box<dyn AttemptPlan>, Notices, Arc<tokio::sync::Notify>) {
    let notices = Arc::new(Mutex::new(Vec::new()));
    let ready = Arc::new(tokio::sync::Notify::new());
    (
        Box::new(ObservedPlan {
            inner: RegistryPlan {
                registry: ModelRegistry::install(catalog(label)),
            },
            notices: Arc::clone(&notices),
            ready: Arc::clone(&ready),
        }),
        notices,
        ready,
    )
}

#[tokio::test(start_paused = true)]
async fn actual_backoff_notifies_then_clears_before_the_next_attempt() {
    let (plan, notices, ready) = notice_plan("retry-feedback-success");
    let opener = Arc::new(AttemptOpener::deterministic(
        [
            ScriptBuilder::new().fail(
                ProviderFailureClass::RateLimit {
                    retry_after: Some(Duration::from_secs(7)),
                },
                None,
                "private upstream text",
            ),
            transcript_script(),
        ],
        route(),
        credential(),
    ));
    let harness = harness("ses_feedback_success", opener, plan);
    let task = tokio::spawn(async move { run(harness, policy(Duration::from_secs(1))).await });
    ready.notified().await;
    assert_eq!(
        *notices.lock().unwrap_or_else(|error| panic!("{error}")),
        vec![Some(RetryNotice {
            attempt: 1,
            after: Duration::from_secs(7),
            class: Some(TerminalClass::RateLimit),
            preparation_stage: None,
            capacity_wait: None,
        })]
    );
    tokio::time::advance(Duration::from_secs(6)).await;
    assert_eq!(
        notices
            .lock()
            .unwrap_or_else(|error| panic!("{error}"))
            .len(),
        1
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let (_, report) = task.await.unwrap_or_else(|error| panic!("{error}"));
    assert!(matches!(report.result, StepResult::Ended { .. }));
    assert_eq!(report.receipts.len(), 2);
    assert_eq!(
        notices
            .lock()
            .unwrap_or_else(|error| panic!("{error}"))
            .last(),
        Some(&None)
    );
}

#[tokio::test(start_paused = true)]
async fn backoff_interrupt_clears_notice_without_opening_a_second_attempt() {
    let (plan, notices, ready) = notice_plan("retry-feedback-cancel");
    let opener = Arc::new(AttemptOpener::deterministic(
        [
            ScriptBuilder::new().fail(ProviderFailureClass::Transport, None, "reset"),
            transcript_script(),
        ],
        route(),
        credential(),
    ));
    let harness = harness("ses_feedback_cancel", opener, plan);
    let (interrupt, cancellation) = step_interrupt();
    let task = tokio::spawn(async move {
        run_with(harness, policy(Duration::from_secs(10)), cancellation).await
    });
    ready.notified().await;
    interrupt.interrupt();
    let (_, report) = task.await.unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(report.receipts.len(), 1);
    assert!(matches!(report.result, StepResult::Failed { .. }));
    assert_eq!(
        notices
            .lock()
            .unwrap_or_else(|error| panic!("{error}"))
            .last(),
        Some(&None)
    );
}
