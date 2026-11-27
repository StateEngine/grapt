//! Provider silence ends at its terminal; owned tools retain their own bounds.
use super::*;
use grapt_provider::ToolExecution;

struct YieldingTools {
    delay: Duration,
    completed: Arc<AtomicUsize>,
}

impl ToolRunner for YieldingTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        let delay = self.delay;
        let completed = Arc::clone(&self.completed);
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            completed.fetch_add(1, Ordering::SeqCst);
            ToolOutcome::Completed { output: None }
        })
    }
}

fn call(tool: &str) -> Result<AttemptFragment, &'static str> {
    Ok(AttemptFragment::ToolCalled {
        call: ToolCallId::new("call_rel14_wait").ok_or("fixture call identity")?,
        tool: tool.into(),
        input: "{}".into(),
        execution: ToolExecution::Local,
        state: AdapterState::none(),
    })
}

fn watchdog(window: Duration) -> Result<StepPolicy, crate::PolicyError> {
    StepPolicy::new(test_version(), 1, Duration::from_secs(1), 1, window)
}

async fn completes_after_terminal(
    tool: &str,
    delay: Duration,
    window: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut source = ScriptSource::new(vec![vec![call(tool)?, stop()]]);
    let mut history = FakeHistory::new();
    let completed = Arc::new(AtomicUsize::new(0));
    let (_interrupt, cancellation) = step_interrupt();
    let started = tokio::time::Instant::now();
    let (owner, report) = StepDriver::run(
        step(),
        watchdog(window)?,
        &mut source,
        &mut history,
        Arc::new(YieldingTools {
            delay,
            completed: Arc::clone(&completed),
        }),
        RecordingOwner::new(),
        cancellation,
    )
    .await;
    assert_eq!(
        report.result,
        StepResult::Ended {
            finish: FinishReason::Stop
        }
    );
    assert_eq!(started.elapsed(), delay);
    assert_eq!(completed.load(Ordering::SeqCst), 1);
    assert_eq!(source.opens.load(Ordering::SeqCst), 1);
    assert_eq!(owner.attempt_terminal_count(), 1);
    assert_eq!(owner.step_terminal_count(), 1);
    assert!(matches!(
        owner.tool_terminals().as_slice(),
        [(_, ToolOutcome::Completed { .. })]
    ));
    let provider = owner.committed.iter().position(|fact| matches!(fact,
        DurableFact::Fragment { fragment } if matches!(fragment.as_ref(), AttemptFragment::ProviderTerminal { .. })
    )).ok_or("provider terminal absent")?;
    let terminal = owner
        .committed
        .iter()
        .position(|fact| matches!(fact, DurableFact::ToolTerminal { .. }))
        .ok_or("tool terminal absent")?;
    assert!(provider < terminal);
    assert!(
        terminal
            < owner
                .attempt_terminal_index()
                .ok_or("attempt terminal absent")?
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn rel14_write_stdin_can_poll_for_five_minutes_after_the_provider_terminal(
) -> Result<(), Box<dyn std::error::Error>> {
    completes_after_terminal(
        "write_stdin",
        Duration::from_secs(300),
        Duration::from_secs(60),
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn rel14_exec_command_yield_outlives_a_shorter_provider_watchdog(
) -> Result<(), Box<dyn std::error::Error>> {
    completes_after_terminal(
        "exec_command",
        Duration::from_secs(30),
        Duration::from_secs(1),
    )
    .await
}

#[tokio::test(start_paused = true)]
async fn rel14_a_running_tool_does_not_hide_an_unfinished_provider_stall(
) -> Result<(), Box<dyn std::error::Error>> {
    let window = Duration::from_secs(60);
    let mut source = ScriptSource::stalling(vec![vec![call("write_stdin")?]]);
    let mut history = FakeHistory::new();
    let completed = Arc::new(AtomicUsize::new(0));
    let (_interrupt, cancellation) = step_interrupt();
    let started = tokio::time::Instant::now();
    let (owner, report) = StepDriver::run(
        step(),
        watchdog(window)?,
        &mut source,
        &mut history,
        Arc::new(YieldingTools {
            delay: window * 5,
            completed: Arc::clone(&completed),
        }),
        RecordingOwner::new(),
        cancellation,
    )
    .await;
    assert_eq!(started.elapsed(), window);
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    assert_eq!(report.receipts.len(), 1);
    assert!(matches!(&report.receipts[0].outcome,
        StepOutcome::Failed { failure } if failure.reason() == &AttemptTerminalReason::ProviderFailure { class: TerminalClass::NoProgressTimeout }
    ));
    assert!(matches!(
        owner.tool_terminals().as_slice(),
        [(
            _,
            ToolOutcome::Cancelled {
                by: InterruptionSource::ManagedShutdown
            }
        )]
    ));
    assert_eq!(owner.step_terminal_count(), 1);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn rel14_a_completed_provider_does_not_disable_user_interruption_of_a_tool(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut source = ScriptSource::new(vec![vec![call("write_stdin")?, stop()]]);
    let mut history = FakeHistory::new();
    let completed = Arc::new(AtomicUsize::new(0));
    let (interrupt, cancellation) = step_interrupt();
    let started = tokio::time::Instant::now();
    let driver = StepDriver::run(
        step(),
        watchdog(Duration::from_secs(60))?,
        &mut source,
        &mut history,
        Arc::new(YieldingTools {
            delay: Duration::from_secs(300),
            completed: Arc::clone(&completed),
        }),
        RecordingOwner::new(),
        cancellation,
    );
    let user = async {
        tokio::time::sleep(Duration::from_secs(120)).await;
        interrupt.interrupt();
    };
    let ((owner, report), ()) = tokio::join!(driver, user);
    assert_eq!(started.elapsed(), Duration::from_secs(120));
    assert!(matches!(report.result, StepResult::Failed { failure }
        if failure.reason() == &StepTerminalReason::Interrupted { by: InterruptionSource::User }));
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    assert!(matches!(
        owner.tool_terminals().as_slice(),
        [(
            _,
            ToolOutcome::Cancelled {
                by: InterruptionSource::User
            }
        )]
    ));
    assert_eq!(owner.step_terminal_count(), 1);
    Ok(())
}
