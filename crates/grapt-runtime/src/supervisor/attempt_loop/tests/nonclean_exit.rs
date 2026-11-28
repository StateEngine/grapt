use super::*;
use grapt_provider::{BlockOrdinal, BoundedDetail};
use grapt_types::{AttemptTerminalReason, ProviderFailureClass as TerminalClass};
use tokio::sync::Notify;

struct ApprovalTools {
    waiting: Arc<Notify>,
    approved: watch::Receiver<bool>,
    dropped: Arc<AtomicBool>,
    executed: Arc<AtomicBool>,
    settled: Option<std::sync::mpsc::SyncSender<()>>,
}

struct ApprovalGuard {
    dropped: Arc<AtomicBool>,
    settled: Option<std::sync::mpsc::SyncSender<()>>,
}

impl Drop for ApprovalGuard {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        if let Some(settled) = &self.settled {
            let _ = settled.try_send(());
        }
    }
}

impl ToolRunner for ApprovalTools {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: crate::tool_protocol::BoundedToolInput,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let waiting = Arc::clone(&self.waiting);
        let mut approved = self.approved.clone();
        let dropped = Arc::clone(&self.dropped);
        let executed = Arc::clone(&self.executed);
        let settled = self.settled.clone();
        Box::pin(async move {
            let _guard = ApprovalGuard { dropped, settled };
            waiting.notify_one();
            if approved.wait_for(|value| *value).await.is_err() {
                return ToolTerminalOutcome::Failed {
                    reason: ToolFailureReason::EffectFailed,
                    detail: None,
                };
            }
            executed.store(true, Ordering::SeqCst);
            ToolTerminalOutcome::Completed { output: None }
        })
    }
}

struct FailureAfterApprovalWait {
    stage: u8,
    waiting: Arc<Notify>,
    remaining: std::vec::IntoIter<AttemptFragment>,
}

impl ProviderReader for FailureAfterApprovalWait {
    async fn next(&mut self) -> Option<AttemptFragment> {
        if self.stage == 0 {
            self.stage = 1;
            return Some(tool_call(call(), "approval", ToolExecution::Local));
        }
        if self.stage == 1 {
            self.waiting.notified().await;
            self.stage = 2;
        }
        self.remaining.next()
    }
}

async fn assert_failure_aborts_approval(
    fragments: Vec<AttemptFragment>,
    class: TerminalClass,
    detail: String,
) {
    let waiting = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let executed = Arc::new(AtomicBool::new(false));
    let (approval_tx, approved) = watch::channel(false);
    let tools = Arc::new(ApprovalTools {
        waiting: Arc::clone(&waiting),
        approved,
        dropped: Arc::clone(&dropped),
        executed: Arc::clone(&executed),
        settled: None,
    });
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_attempt(
            attempt("att_nonclean_approval"),
            move || FailureAfterApprovalWait {
                stage: 0,
                waiting,
                remaining: fragments.into_iter(),
            },
            tools,
            RecordingOwner::new(),
            cancel_tx,
            cancel_rx,
            DrainResidency::new().token(),
            quiet_step(),
        ),
    )
    .await;
    let (owner, report) = match result {
        Ok(result) => result,
        Err(_) => panic!("non-clean provider exit waited for approval: {detail}"),
    };
    assert!(
        dropped.load(Ordering::SeqCst),
        "approval task was not joined"
    );
    approval_tx.send_replace(true);
    tokio::task::yield_now().await;
    assert!(
        !executed.load(Ordering::SeqCst),
        "late approval ran the effect"
    );
    let failure = match report.outcome {
        StepOutcome::Failed { failure } => failure,
        other => panic!("failure was relabelled: {other:?}"),
    };
    assert_eq!(
        failure.reason(),
        &AttemptTerminalReason::ProviderFailure { class }
    );
    assert_eq!(failure.detail(), &BoundedDetail::from(detail));
    let tool_terminals: Vec<_> = owner
        .committed
        .iter()
        .enumerate()
        .filter(|(_, fact)| matches!(fact, ToolTerminal { call: id, .. } if id == &call()))
        .collect();
    assert_eq!(
        tool_terminals.len(),
        1,
        "pending tool must close exactly once"
    );
    assert!(matches!(
        tool_terminals[0].1,
        ToolTerminal {
            outcome: ToolTerminalOutcome::Failed {
                reason: ToolFailureReason::EffectUnknown,
                ..
            },
            ..
        }
    ));
    let failures: Vec<_> = owner
        .committed
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| match fact {
            AttemptFailed { failure } => Some((index, failure)),
            _ => None,
        })
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].1, &failure);
    assert!(tool_terminals[0].0 < failures[0].0);
    assert!(
        !report.diagnostics.cancelled,
        "provider failure became an interrupt"
    );
}

#[tokio::test(start_paused = true)]
async fn content_filter_aborts_pending_approval() {
    assert_failure_aborts_approval(
        vec![ProviderTerminal {
            finish: FinishReason::ContentFilter,
            usage: None,
            state: AdapterState::none(),
        }],
        TerminalClass::ContentPolicy,
        "provider finish ContentFilter is a failure wearing a terminal's shape".into(),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn error_finish_aborts_pending_approval() {
    assert_failure_aborts_approval(
        vec![ProviderTerminal {
            finish: FinishReason::Error,
            usage: None,
            state: AdapterState::none(),
        }],
        TerminalClass::ProviderInternal,
        "provider finish Error is a failure wearing a terminal's shape".into(),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn call_limit_aborts_pending_approval() {
    let fragments = (1..=ATTEMPT_TOOL_CALL_LIMIT)
        .map(|ordinal| {
            tool_call(
                call_named(&format!("call_bound_{ordinal}")),
                "hosted",
                ToolExecution::ProviderHosted,
            )
        })
        .collect();
    assert_failure_aborts_approval(
        fragments,
        TerminalClass::InvalidProviderOutput,
        format!("attempt exceeded the {ATTEMPT_TOOL_CALL_LIMIT}-call tool limit"),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn protocol_violation_aborts_pending_approval() {
    assert_failure_aborts_approval(
        vec![TextDelta {
            block: BlockOrdinal::new(9),
            text: "unopened".into(),
            state: AdapterState::none(),
        }],
        TerminalClass::InvalidProviderOutput,
        "text block 9 received a delta or close with no matching open".into(),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn invalid_tool_input_aborts_pending_approval() {
    let mut invalid = tool_call(
        call_named("call_invalid_input"),
        "local",
        ToolExecution::Local,
    );
    if let ToolCalled { input, .. } = &mut invalid {
        *input = "x".repeat(crate::budgets::TOOL_INPUT_MAX_BYTES + 1);
    }
    assert_failure_aborts_approval(
        vec![invalid],
        TerminalClass::InvalidProviderOutput,
        format!(
            "tool input is {} bytes; the limit is {}",
            crate::budgets::TOOL_INPUT_MAX_BYTES + 1,
            crate::budgets::TOOL_INPUT_MAX_BYTES
        ),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn missing_terminal_aborts_pending_approval() {
    assert_failure_aborts_approval(
        vec![],
        TerminalClass::InvalidProviderOutput,
        "reader completed with no ProviderTerminal or ProviderFailure".into(),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn post_terminal_fragment_aborts_pending_approval() {
    assert_failure_aborts_approval(
        vec![
            ProviderTerminal {
                finish: FinishReason::Stop,
                usage: None,
                state: AdapterState::none(),
            },
            TextDelta {
                block: BlockOrdinal::new(9),
                text: "after terminal".into(),
                state: AdapterState::none(),
            },
        ],
        TerminalClass::InvalidProviderOutput,
        "1 fragment(s) arrived after the provider terminal".into(),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn clean_finish_keeps_pending_approval_live() {
    let waiting = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let executed = Arc::new(AtomicBool::new(false));
    let (approval_tx, approved) = watch::channel(false);
    let tools = Arc::new(ApprovalTools {
        waiting: Arc::clone(&waiting),
        approved,
        dropped: Arc::clone(&dropped),
        executed: Arc::clone(&executed),
        settled: None,
    });
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let mut run = Box::pin(run_attempt(
        attempt("att_clean_approval"),
        move || FailureAfterApprovalWait {
            stage: 0,
            waiting,
            remaining: vec![ProviderTerminal {
                finish: FinishReason::ToolCalls,
                usage: None,
                state: AdapterState::none(),
            }]
            .into_iter(),
        },
        tools,
        RecordingOwner::new(),
        cancel_tx,
        cancel_rx,
        DrainResidency::new().token(),
        quiet_step(),
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(1), &mut run)
            .await
            .is_err()
    );
    assert!(!dropped.load(Ordering::SeqCst));
    approval_tx.send_replace(true);
    let (_, report) = match tokio::time::timeout(std::time::Duration::from_secs(2), run).await {
        Ok(result) => result,
        Err(_) => panic!("clean attempt did not finish after approval"),
    };
    assert!(executed.load(Ordering::SeqCst));
    assert!(dropped.load(Ordering::SeqCst));
    assert!(matches!(
        report.outcome,
        StepOutcome::Ended {
            finish: FinishReason::ToolCalls
        }
    ));
}

struct ApproveDuringReaderCleanup {
    inner: FailureAfterApprovalWait,
    approval: watch::Sender<bool>,
    tool_settled: std::sync::mpsc::Receiver<()>,
    cleanup_observed: Arc<AtomicBool>,
    queued: std::sync::mpsc::SyncSender<()>,
}

impl ProviderReader for ApproveDuringReaderCleanup {
    async fn next(&mut self) -> Option<AttemptFragment> {
        match self.inner.next().await {
            Some(fragment) => Some(fragment),
            None => {
                let _ = self.queued.try_send(());
                std::future::pending().await
            }
        }
    }
}

impl Drop for ApproveDuringReaderCleanup {
    fn drop(&mut self) {
        // Reader Drop runs on a Tokio worker. Hand its core back before
        // waking approval, so the bounded handshake cannot strand that task
        // in the blocked worker's non-stealable LIFO slot.
        let settled = tokio::task::block_in_place(|| {
            self.approval.send_replace(true);
            self.tool_settled
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok()
        });
        self.cleanup_observed.store(settled, Ordering::SeqCst);
    }
}

struct WaitForQueuedOutput {
    inner: RecordingOwner,
    queued: std::sync::mpsc::Receiver<()>,
}

impl crate::owner::AttemptOwner for WaitForQueuedOutput {
    fn commit(&mut self, fact: DurableFact) -> Result<(), crate::owner::OwnerError> {
        if matches!(&fact, Fragment { fragment } if matches!(fragment.as_ref(), ProviderTerminal { .. }))
        {
            // The second worker finishes enqueueing the invalid fragment before
            // the attempt can leave its terminal branch. Missing evidence fails.
            assert!(self
                .queued
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok());
        }
        crate::owner::AttemptOwner::commit(&mut self.inner, fact)
    }

    fn notify(&mut self, fact: &DurableFact) {
        crate::owner::AttemptOwner::notify(&mut self.inner, fact);
    }

    fn observe(&mut self, observation: crate::owner::Observation) {
        crate::owner::AttemptOwner::observe(&mut self.inner, observation);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_violation_aborts_before_approval_during_reader_cleanup() {
    let waiting = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let executed = Arc::new(AtomicBool::new(false));
    let cleanup_observed = Arc::new(AtomicBool::new(false));
    let (approval, approved) = watch::channel(false);
    let (settled, tool_settled) = std::sync::mpsc::sync_channel(1);
    let (queued, queued_rx) = std::sync::mpsc::sync_channel(1);
    let tools = Arc::new(ApprovalTools {
        waiting: Arc::clone(&waiting),
        approved,
        dropped: Arc::clone(&dropped),
        executed: Arc::clone(&executed),
        settled: Some(settled),
    });
    let cleanup = Arc::clone(&cleanup_observed);
    let (cancel_tx, cancel_rx) = watch::channel(None);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_attempt(
            attempt("att_reader_cleanup_approval"),
            move || ApproveDuringReaderCleanup {
                inner: FailureAfterApprovalWait {
                    stage: 0,
                    waiting,
                    remaining: vec![
                        ProviderTerminal {
                            finish: FinishReason::ToolCalls,
                            usage: None,
                            state: AdapterState::none(),
                        },
                        TextOpened {
                            block: BlockOrdinal::new(9),
                            state: AdapterState::none(),
                        },
                    ]
                    .into_iter(),
                },
                approval,
                tool_settled,
                cleanup_observed: cleanup,
                queued,
            },
            tools,
            WaitForQueuedOutput {
                inner: RecordingOwner::new(),
                queued: queued_rx,
            },
            cancel_tx,
            cancel_rx,
            DrainResidency::new().token(),
            quiet_step(),
        ),
    )
    .await;
    let (_, report) = match result {
        Ok(result) => result,
        Err(_) => panic!("reader cleanup did not settle its approval handshake"),
    };
    assert!(
        cleanup_observed.load(Ordering::SeqCst),
        "reader cleanup handshake was not observed"
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert!(
        !executed.load(Ordering::SeqCst),
        "approval during reader cleanup ran the effect"
    );
    match report.outcome {
        StepOutcome::Failed { failure } => {
            assert_eq!(
                failure.reason(),
                &AttemptTerminalReason::ProviderFailure {
                    class: TerminalClass::InvalidProviderOutput,
                }
            );
            assert_eq!(
                failure.detail(),
                &BoundedDetail::from("1 fragment(s) arrived after the provider terminal")
            );
        }
        other => panic!("queued protocol failure was relabelled: {other:?}"),
    }
}
