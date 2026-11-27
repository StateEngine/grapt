//! The production settlement wrapper must drive the actual message owner.
use super::*;
use crate::commit_fault;
use crate::settlement::{StepSettlementContext, StepSettlementOwner};
use grapt_artifact::{
    ArtifactMetadata, CaptureError, CapturedArtifact, FetchError, ReadBudget, ReadPage, StatError,
};
use grapt_types::{ArtifactId, ArtifactRef};

#[test]
fn composed_tool_terminal_notifies_once_only_after_commit() -> Result<(), Box<dyn std::error::Error>>
{
    for refuse in [false, true] {
        let scratch = Scratch::new(if refuse {
            "tool-refused"
        } else {
            "tool-committed"
        });
        let inbox = Arc::new(Mutex::new(Inbox::new(EventStore::open_in_memory()?)?));
        let inner = DurableAttemptOwner::elect_inbox_owned(
            Arc::clone(&inbox),
            scratch.artifacts(),
            Arc::new(FrozenClock),
            Arc::new(CountingIds::default()),
            RecordingSink::default(),
            session(),
            step(1),
            None,
        )?;
        let writer = inbox
            .lock()
            .map_err(|_| "poisoned")?
            .store()
            .fencing_token(&AggregateId::Session(session()))?;
        let context = StepSettlementContext::new(
            session(),
            step(1),
            "evt_composed_step".parse()?,
            1234,
            writer,
            Arc::new(TerminalIds),
            Arc::new(TerminalClock),
        );
        let mut owner = StepSettlementOwner::new(inner, Arc::clone(&inbox), context);
        owner.commit(DurableFact::AttemptStarted {
            attempt: attempt("pat_composed_tool"),
        })?;
        let _fault = if refuse {
            Some(commit_fault::CommitFault::install(
                inbox.lock().map_err(|_| "poisoned")?.store_mut(),
                rusqlite::hooks::Action::SQLITE_INSERT,
                "event",
                0,
            )?)
        } else {
            None
        };
        let fact = DurableFact::ToolTerminal {
            call: ToolCallId::new("call_composed_tool").ok_or("call")?,
            outcome: grapt_types::ToolTerminalOutcome::Completed { output: None },
        };
        assert_eq!(owner.commit(fact.clone()).is_err(), refuse);
        owner.notify(&fact);
        owner.notify(&fact);
        if !refuse {
            owner.commit(fact.clone())?;
            owner.notify(&fact);
        }
        let inner = owner.into_inner();
        let notices = inner
            .sink
            .offered
            .iter()
            .filter(|fact| matches!(fact, DurableFact::ToolTerminal { .. }))
            .count();
        assert_eq!(
            notices,
            usize::from(!refuse),
            "committed terminal advisory missing or duplicated"
        );
        let count: i64 = inbox
            .lock()
            .map_err(|_| "poisoned")?
            .store()
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM event WHERE type = 'session.tool.terminal.1'",
                [],
                |row| row.get(0),
            )?;
        assert_eq!(count, i64::from(!refuse));
    }
    Ok(())
}

#[derive(Debug)]
struct TerminalClock;
impl crate::Clock for TerminalClock {
    fn now_ms(&self) -> i64 {
        1_784_090_757_114
    }
}

#[derive(Debug)]
struct TerminalIds;
impl crate::EventIds for TerminalIds {
    fn mint(&self) -> EventId {
        "evt_unused_tool"
            .parse()
            .unwrap_or_else(|error| panic!("fixture identity: {error}"))
    }
}

struct CaptureFault {
    inner: FilesystemArtifactStore,
    fail: bool,
    calls: Arc<AtomicU64>,
}
impl ArtifactStore for CaptureFault {
    fn capture<R: std::io::Read>(
        &self,
        source: R,
        bound: CaptureBound,
        kind: ArtifactKind,
        sensitivity: Sensitivity,
    ) -> Result<CapturedArtifact, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(CaptureError::Store(std::io::Error::other(
                "injected capture failure",
            )));
        }
        self.inner.capture(source, bound, kind, sensitivity)
    }
    fn stat(&self, id: &ArtifactId) -> Result<ArtifactMetadata, StatError> {
        self.inner.stat(id)
    }
    fn fetch(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        budget: ReadBudget,
    ) -> Result<ReadPage, FetchError> {
        self.inner.fetch(reference, offset, budget)
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn composed(fail_capture: bool, replay_terminal: bool) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new(if fail_capture {
        "composition-fault"
    } else if replay_terminal {
        "composition-replay"
    } else {
        "composition"
    });
    let inbox = Arc::new(Mutex::new(Inbox::new(EventStore::open_in_memory()?)?));
    let captures = Arc::new(AtomicU64::new(0));
    let mut inner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(&inbox),
        CaptureFault {
            inner: scratch.artifacts(),
            fail: fail_capture,
            calls: Arc::clone(&captures),
        },
        Arc::new(FrozenClock),
        Arc::new(CountingIds::default()),
        RecordingSink::default(),
        session(),
        step(1),
        None,
    )?;
    let steps = if fail_capture { 1 } else { 2 };
    for number in 1..=steps {
        inner.begin_step(step(number))?;
        let writer = inbox
            .lock()
            .map_err(|_| "inbox poisoned")?
            .store()
            .fencing_token(&AggregateId::Session(session()))?;
        let context = StepSettlementContext::new(
            session(),
            step(number),
            format!("evt_composed_terminal_{number}").parse()?,
            1234,
            writer,
            Arc::new(TerminalIds),
            Arc::new(TerminalClock),
        );
        let mut owner = StepSettlementOwner::new(inner, Arc::clone(&inbox), context);
        owner.commit(DurableFact::AttemptStarted {
            attempt: attempt(&format!("pat_composed_{number}")),
        })?;
        owner.commit(text_opened(0))?;
        owner.commit(text_delta(0, "partial"))?;
        // No provider close: preparation must capture before the outer writer
        // commits this terminal, including on a later Step using the same owner.
        let terminal = if number == 1 && !fail_capture {
            DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            }
        } else {
            DurableFact::StepFailed {
                failure: crate::owner::StepFailure::new(
                    StepTerminalReason::Interrupted {
                        by: InterruptionSource::User,
                    },
                    grapt_provider::BoundedDetail::from("interrupt"),
                ),
            }
        };
        let committed = owner.commit(terminal.clone());
        if fail_capture {
            assert!(committed.is_err(), "capture failure became success");
        } else {
            committed?;
            if replay_terminal && number == 1 {
                // The first append committed, but its advisory was lost. Replay
                // must reconcile the owner even though it earns no new notice.
                owner.commit(terminal.clone())?;
            }
            owner.notify(&terminal);
        }
        inner = owner.into_inner();
        assert!(
            inner.is_settled(),
            "outer terminal did not settle the returned message owner"
        );
    }
    assert_eq!(
        captures.load(Ordering::SeqCst),
        u64::from(steps),
        "capture guard never fired"
    );
    let inbox = inbox.lock().map_err(|_| "inbox poisoned")?;
    let history = grapt_session::SessionHistoryStore::over(inbox.store()).fold(&session())?;
    let messages = history.messages()?;
    assert_eq!(messages.len(), steps as usize);
    for (index, message) in messages.iter().enumerate() {
        assert_eq!(message.step.value() as usize, index + 1);
        if fail_capture {
            assert!(message.content.is_none());
            assert_eq!(
                message.terminal,
                Some(StepTerminal::Failed {
                    reason: StepTerminalReason::DurabilityFailure
                })
            );
        } else {
            let content = message
                .content
                .as_ref()
                .ok_or("observed bytes disappeared")?;
            assert_eq!(content.items()[0].item.payload().length(), 7);
            assert_eq!(content.items()[0].item.ordinal(), ContentOrdinal::FIRST);
            let captured = content.items()[0]
                .committed
                .as_ref()
                .ok_or("capture commit missing")?;
            let terminal = message
                .terminal_committed
                .as_ref()
                .ok_or("terminal commit missing")?;
            assert!(
                captured.seq < terminal.seq,
                "content was written after its Step terminal"
            );
        }
    }
    if !fail_capture {
        assert_ne!(messages[0].message, messages[1].message);
        let terminal_notices = inner
            .sink
            .offered
            .iter()
            .filter(|fact| {
                matches!(
                    fact,
                    DurableFact::StepEnded { .. } | DurableFact::StepFailed { .. }
                )
            })
            .count();
        assert_eq!(terminal_notices, if replay_terminal { 1 } else { 2 });
    }
    let count: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type IN ('session.step.failed.3','session.step.ended.3')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count,
        i64::from(steps),
        "a Step terminal resurrected or disappeared"
    );
    Ok(())
}

#[test]
fn production_wrapper_preserves_partial_content_and_separates_successive_steps(
) -> Result<(), Box<dyn std::error::Error>> {
    composed(false, false)
}

#[test]
fn capture_failure_still_commits_one_typed_failure_terminal(
) -> Result<(), Box<dyn std::error::Error>> {
    composed(true, false)
}

#[test]
fn replayed_terminal_settles_owner_without_duplicate_notice_and_allows_next_step(
) -> Result<(), Box<dyn std::error::Error>> {
    composed(false, true)
}
