use super::*;

struct DecisionTools(tokio::sync::watch::Receiver<u32>);

impl ToolRunner for DecisionTools {
    fn decision_waits(&self) -> Option<tokio::sync::watch::Receiver<u32>> {
        Some(self.0.clone())
    }

    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        panic!("this witness isolates the watchdog from effect execution")
    }
}

#[tokio::test(start_paused = true)]
async fn decision_wait_outlives_three_stall_windows_then_effect_gets_a_full_window() {
    let (waiting, receiver) = tokio::sync::watch::channel(1);
    let mut source = ScriptSource::stalling(vec![text("decision requested")]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let start = tokio::time::Instant::now();
    let driver = StepDriver::run(
        step(),
        policy(1),
        &mut source,
        &mut history,
        Arc::new(DecisionTools(receiver)),
        RecordingOwner::new(),
        cancel,
    );
    let human = async {
        tokio::time::sleep(Duration::from_secs(90)).await;
        waiting.send_replace(0);
    };
    let ((owner, report), ()) = tokio::join!(driver, human);
    assert_eq!(start.elapsed(), Duration::from_secs(120));
    match report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::ProviderFailure {
                class: TerminalClass::NoProgressTimeout
            }
        ),
        other => panic!("effect silence must still time out: {other:?}"),
    }
    assert_eq!(owner.step_terminal_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn decision_wait_can_be_interrupted_after_multiple_stall_windows() {
    let (_waiting, receiver) = tokio::sync::watch::channel(1);
    let mut source = ScriptSource::stalling(vec![text("decision requested")]);
    let mut history = FakeHistory::new();
    let (interrupt, cancel) = step_interrupt();
    let start = tokio::time::Instant::now();
    let driver = StepDriver::run(
        step(),
        policy(1),
        &mut source,
        &mut history,
        Arc::new(DecisionTools(receiver)),
        RecordingOwner::new(),
        cancel,
    );
    let human = async {
        tokio::time::sleep(Duration::from_secs(90)).await;
        interrupt.interrupt();
    };
    let ((owner, report), ()) = tokio::join!(driver, human);
    assert_eq!(start.elapsed(), Duration::from_secs(90));
    match report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::Interrupted {
                by: InterruptionSource::User
            }
        ),
        other => panic!("human cancellation must win: {other:?}"),
    }
    assert_eq!(owner.step_terminal_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn lost_decision_owner_cannot_disable_stall_detection_forever() {
    let (waiting, receiver) = tokio::sync::watch::channel(1);
    let mut source = ScriptSource::stalling(vec![text("decision requested")]);
    let mut history = FakeHistory::new();
    let (_interrupt, cancel) = step_interrupt();
    let start = tokio::time::Instant::now();
    let driver = StepDriver::run(
        step(),
        policy(1),
        &mut source,
        &mut history,
        Arc::new(DecisionTools(receiver)),
        RecordingOwner::new(),
        cancel,
    );
    let owner_lost = async move {
        tokio::time::sleep(Duration::from_secs(90)).await;
        drop(waiting);
    };
    let ((owner, report), ()) = tokio::join!(driver, owner_lost);
    assert_eq!(start.elapsed(), Duration::from_secs(120));
    match report.result {
        StepResult::Failed { failure } => assert_eq!(
            failure.reason(),
            &StepTerminalReason::ProviderFailure {
                class: TerminalClass::NoProgressTimeout
            }
        ),
        other => panic!("closed decision custody must restore the watchdog: {other:?}"),
    }
    assert_eq!(owner.step_terminal_count(), 1);
}
