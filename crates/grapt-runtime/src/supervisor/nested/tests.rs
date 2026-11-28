use super::*;
use crate::owner::RecordingOwner;
use crate::residency::DrainResidency;
use grapt_provider::{AdapterState, AttemptFragment, FinishReason, ToolExecution};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

struct DefaultRunner;
impl ToolRunner for DefaultRunner {
    fn run(
        &self,
        _call: ToolCallId,
        _tool: String,
        _input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        Box::pin(async { ToolOutcome::Completed { output: None } })
    }
}

/// See the sibling module's note: a Step watch with no sender and no value.
fn quiet_step() -> tokio::sync::watch::Receiver<Option<grapt_types::InterruptionSource>> {
    let (sender, receiver) = tokio::sync::watch::channel(None);
    drop(sender);
    receiver
}

fn call(value: &str) -> ToolCallId {
    ToolCallId::new(value).unwrap_or_else(|| panic!("call fixture"))
}
fn input() -> BoundedToolInput {
    BoundedToolInput::new(b"{}".to_vec()).unwrap_or_else(|error| panic!("input: {error}"))
}
fn tool() -> ToolDefinitionId {
    ToolDefinitionId::new("child").unwrap_or_else(|| panic!("tool fixture"))
}
fn catalog() -> ToolSchemaVersion {
    ToolSchemaVersion::new("catalog-1").unwrap_or_else(|| panic!("catalog fixture"))
}

struct Reader(std::vec::IntoIter<AttemptFragment>);
impl ProviderReader for Reader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        self.0.next()
    }
}

struct Composition {
    entered: Arc<AtomicUsize>,
    running: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    barrier: Arc<tokio::sync::Barrier>,
}

impl ToolRunner for Composition {
    fn is_composition(&self, name: &str) -> bool {
        name == "execute"
    }
    fn run(
        &self,
        _call: ToolCallId,
        name: String,
        _input: BoundedToolInput,
    ) -> BoxFuture<'static, ToolOutcome> {
        assert_eq!(name, "child");
        let entered = Arc::clone(&self.entered);
        let running = Arc::clone(&self.running);
        let peak = Arc::clone(&self.peak);
        let barrier = Arc::clone(&self.barrier);
        Box::pin(async move {
            entered.fetch_add(1, Ordering::SeqCst);
            let n = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(n, Ordering::SeqCst);
            barrier.wait().await;
            running.fetch_sub(1, Ordering::SeqCst);
            ToolOutcome::Completed { output: None }
        })
    }
    fn run_with_nested(
        &self,
        _attempt: Option<PhysicalAttemptId>,
        parent: ToolCallId,
        _name: String,
        _input: BoundedToolInput,
        nested: Option<NestedTools>,
    ) -> BoxFuture<'static, ToolOutcome> {
        let nested = nested.unwrap_or_else(|| panic!("composition handle missing"));
        Box::pin(async move {
            let mut children = JoinSet::new();
            for ordinal in 0..8 {
                let nested = nested.clone();
                let call = call(&format!("{}_child_{ordinal}", parent.as_str()));
                children.spawn(async move { nested.call(call, tool(), catalog(), input()).await });
            }
            while let Some(result) = children.join_next().await {
                match result {
                    Ok(Ok(ToolOutcome::Completed { .. })) => {}
                    _ => {
                        return ToolOutcome::Failed {
                            reason: ToolFailureReason::EffectFailed,
                            detail: None,
                        }
                    }
                }
            }
            ToolOutcome::Completed { output: None }
        })
    }
}

struct RejectNested(RecordingOwner);
impl AttemptOwner for RejectNested {
    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError> {
        if matches!(fact, DurableFact::NestedToolCalled { .. }) {
            return Err(OwnerError {
                reason: "injected nested admission refusal".into(),
            });
        }
        self.0.commit(fact)
    }
    fn notify(&mut self, fact: &DurableFact) {
        self.0.notify(fact);
    }
    fn observe(&mut self, observation: Observation) {
        self.0.observe(observation);
    }
}

async fn run<O: AttemptOwner + Send>(owner: O, tools: Arc<Composition>) -> (O, AttemptReport) {
    let mut script = Vec::new();
    for ordinal in 0..4 {
        script.push(AttemptFragment::ToolCalled {
            call: call(&format!("parent_{ordinal}")),
            tool: "execute".into(),
            input: "{}".into(),
            execution: ToolExecution::Local,
            state: AdapterState::none(),
        });
    }
    script.push(AttemptFragment::ProviderTerminal {
        finish: FinishReason::ToolCalls,
        usage: None,
        state: AdapterState::none(),
    });
    let (tx, rx) = watch::channel(None);
    let attempt =
        PhysicalAttemptId::new("attempt_nested").unwrap_or_else(|| panic!("attempt fixture"));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::attempt_loop::run_attempt(
            attempt,
            move || Reader(script.into_iter()),
            tools,
            owner,
            tx,
            rx,
            DrainResidency::new().token(),
            quiet_step(),
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("nested shared-capacity deadlock"))
}

fn composition() -> Arc<Composition> {
    Arc::new(Composition {
        entered: Arc::new(AtomicUsize::new(0)),
        running: Arc::new(AtomicUsize::new(0)),
        peak: Arc::new(AtomicUsize::new(0)),
        barrier: Arc::new(tokio::sync::Barrier::new(16)),
    })
}

#[tokio::test]
async fn compositions_release_effect_capacity_and_children_commit_before_parents() {
    let tools = composition();
    let (owner, report) = run(RecordingOwner::new(), Arc::clone(&tools)).await;
    assert!(matches!(report.outcome, StepOutcome::Ended { .. }));
    assert_eq!(tools.entered.load(Ordering::SeqCst), 32);
    assert_eq!(tools.peak.load(Ordering::SeqCst), 16);
    for (position, fact) in owner.committed.iter().enumerate() {
        if let DurableFact::NestedToolCalled { parent, call, .. } = fact {
            let terminal = owner
                .committed
                .iter()
                .position(
                    |fact| matches!(fact,DurableFact::ToolTerminal { call:done,.. } if done==call),
                )
                .unwrap_or_else(|| panic!("child terminal missing"));
            let parent_terminal=owner.committed.iter().position(|fact| matches!(fact,DurableFact::ToolTerminal { call:done,.. } if done==parent))
                .unwrap_or_else(|| panic!("parent terminal missing"));
            assert!(position < terminal && terminal < parent_terminal);
        }
    }
    assert_eq!(owner.tool_terminals().len(), 36);
}

#[tokio::test]
async fn refused_nested_admission_runs_zero_effects() {
    let tools = composition();
    let (_, report) = run(RejectNested(RecordingOwner::new()), Arc::clone(&tools)).await;
    assert!(matches!(report.outcome, StepOutcome::OwnerRejected { .. }));
    assert_eq!(tools.entered.load(Ordering::SeqCst), 0);
}

#[test]
fn abandoned_child_has_one_unknown_effect_terminal() {
    let mut state = NestedState::default();
    let parent = call("parent");
    let child = call("child");
    state.register(parent.clone());
    let (reply, _admitted) = oneshot::channel();
    let mut owner = RecordingOwner::new();
    state
        .handle(
            NestedRequest::Start {
                parent: parent.clone(),
                call: child.clone(),
                tool: tool(),
                catalog: catalog(),
                input: input(),
                reply,
            },
            &mut owner,
            &BTreeMap::new(),
            &DefaultRunner,
        )
        .unwrap_or_else(|error| panic!("admission: {error:?}"));
    state
        .handle(
            NestedRequest::Abandoned {
                call: child.clone(),
            },
            &mut owner,
            &BTreeMap::new(),
            &DefaultRunner,
        )
        .unwrap_or_else(|error| panic!("abandonment: {error:?}"));
    state
        .close_parent(&mut owner, &parent, &DefaultRunner)
        .unwrap_or_else(|error| panic!("parent closure: {error:?}"));
    assert!(matches!(
        owner.tool_terminals().as_slice(),
        [(
            _,
            ToolOutcome::Failed {
                reason: ToolFailureReason::EffectUnknown,
                ..
            }
        )]
    ));
}

#[test]
fn nested_fanout_parent_scope_and_terminal_replay_are_bounded() {
    let mut state = NestedState::default();
    let parent = call("bounded-parent");
    state.register(parent.clone());
    let mut owner = RecordingOwner::new();
    let admit = |state: &mut NestedState,
                 owner: &mut RecordingOwner,
                 parent: ToolCallId,
                 child: ToolCallId| {
        let (reply, mut received) = oneshot::channel();
        state
            .handle(
                NestedRequest::Start {
                    parent,
                    call: child,
                    tool: tool(),
                    catalog: catalog(),
                    input: input(),
                    reply,
                },
                owner,
                &BTreeMap::new(),
                &DefaultRunner,
            )
            .unwrap_or_else(|error| panic!("bounded admission: {error:?}"));
        received
            .try_recv()
            .unwrap_or_else(|error| panic!("admission acknowledgement missing: {error}"))
    };
    assert_eq!(
        admit(
            &mut state,
            &mut owner,
            call("foreign-parent"),
            call("foreign-child")
        ),
        Err(NestedCallError::Closed)
    );
    for ordinal in 0..64 {
        assert_eq!(
            admit(
                &mut state,
                &mut owner,
                parent.clone(),
                call(&format!("bounded-{ordinal}"))
            ),
            Ok(())
        );
    }
    assert_eq!(
        admit(&mut state, &mut owner, parent.clone(), call("bounded-0")),
        Err(NestedCallError::Duplicate)
    );
    assert_eq!(
        admit(&mut state, &mut owner, parent.clone(), call("bounded-64")),
        Err(NestedCallError::Capacity)
    );
    // No abandonment message is delivered: parent closure owns every open child.
    state
        .close_parent(&mut owner, &parent, &DefaultRunner)
        .unwrap_or_else(|error| panic!("closure: {error:?}"));
    assert_eq!(owner.tool_terminals().len(), 64);
    assert!(owner.tool_terminals().iter().all(|(_, outcome)| matches!(
        outcome,
        ToolOutcome::Failed {
            reason: ToolFailureReason::EffectUnknown,
            ..
        }
    )));
    let before = owner.committed.len();
    let (reply, mut received) = oneshot::channel();
    state
        .handle(
            NestedRequest::Terminal {
                call: call("bounded-0"),
                outcome: ToolOutcome::Completed { output: None },
                reply,
            },
            &mut owner,
            &BTreeMap::new(),
            &DefaultRunner,
        )
        .unwrap_or_else(|error| panic!("late terminal: {error:?}"));
    assert_eq!(received.try_recv(), Ok(Err(NestedCallError::Closed)));
    assert_eq!(owner.committed.len(), before);
    state.register(parent.clone());
    assert_eq!(
        admit(&mut state, &mut owner, parent, call("bounded-65")),
        Err(NestedCallError::Capacity)
    );
}

#[tokio::test]
async fn nested_effect_waits_for_committed_admission() {
    struct Counter(Arc<AtomicUsize>);
    impl ToolRunner for Counter {
        fn run(
            &self,
            _call: ToolCallId,
            _tool: String,
            _input: BoundedToolInput,
        ) -> BoxFuture<'static, ToolOutcome> {
            let entered = Arc::clone(&self.0);
            Box::pin(async move {
                entered.fetch_add(1, Ordering::SeqCst);
                ToolOutcome::Completed { output: None }
            })
        }
    }
    let entered = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = mpsc::channel(NESTED_CHANNEL_CAPACITY);
    let nested = NestedTools::new(
        call("admission-parent"),
        PhysicalAttemptId::new("admission-attempt").unwrap_or_else(|| panic!("attempt")),
        Arc::new(Counter(Arc::clone(&entered))),
        tx,
        Arc::new(Semaphore::new(16)),
        Arc::new(Semaphore::new(3)),
    );
    let task = tokio::spawn(async move {
        nested
            .call(call("admission-child"), tool(), catalog(), input())
            .await
    });
    let request = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("admission was not requested"));
    let Some(NestedRequest::Start { reply, .. }) = request else {
        panic!("first request must be admission")
    };
    assert_eq!(
        entered.load(Ordering::SeqCst),
        0,
        "effect ran before admission committed"
    );
    assert!(reply.send(Err(NestedCallError::Durability)).is_ok());
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap_or_else(|_| panic!("refused admission did not settle"))
        .unwrap_or_else(|error| panic!("nested task failed: {error}"));
    assert_eq!(result, Err(NestedCallError::Durability));
    assert_eq!(entered.load(Ordering::SeqCst), 0);
}

#[test]
fn abandoned_nested_approval_reports_effect_failed_and_clears_tracker() {
    use std::sync::Mutex;

    struct TrackedRunner {
        pending: Mutex<BTreeSet<ToolCallId>>,
    }
    impl ToolRunner for TrackedRunner {
        fn known_unexecuted(&self, call: &ToolCallId) -> bool {
            self.pending
                .lock()
                .map(|mut set| set.remove(call))
                .unwrap_or(false)
        }
        fn run(
            &self,
            _call: ToolCallId,
            _tool: String,
            _input: BoundedToolInput,
        ) -> BoxFuture<'static, ToolOutcome> {
            Box::pin(async { ToolOutcome::Completed { output: None } })
        }
    }

    let unexecuted_child = call("nested-unexecuted-approval");
    let dispatched_child = call("nested-dispatched-abandoned");
    let parent = call("nested-parent");
    let runner = TrackedRunner {
        pending: Mutex::new(BTreeSet::from([unexecuted_child.clone()])),
    };
    let mut state = NestedState::default();
    state.register(parent.clone());
    let mut owner = RecordingOwner::new();
    let top_level = BTreeMap::from([(parent.as_str().to_owned(), parent.clone())]);
    for child in [&unexecuted_child, &dispatched_child] {
        let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
        assert!(state
            .handle(
                NestedRequest::Start {
                    parent: parent.clone(),
                    call: child.clone(),
                    tool: tool(),
                    catalog: catalog(),
                    input: input(),
                    reply: reply_tx,
                },
                &mut owner,
                &top_level,
                &runner,
            )
            .is_ok());
    }
    assert!(state.close_parent(&mut owner, &parent, &runner).is_ok());
    assert!(
        runner
            .pending
            .lock()
            .map(|set| set.is_empty())
            .unwrap_or(false),
        "abandoned nested approval must be removed from the runner tracker"
    );
    let outcome_of = |target: &ToolCallId| {
        owner.committed.iter().find_map(|fact| match fact {
            DurableFact::ToolTerminal { call, outcome } if call == target => Some(outcome.clone()),
            _ => None,
        })
    };
    assert!(matches!(
        outcome_of(&unexecuted_child),
        Some(ToolOutcome::Failed {
            reason: ToolFailureReason::EffectFailed,
            ..
        })
    ));
    assert!(matches!(
        outcome_of(&dispatched_child),
        Some(ToolOutcome::Failed {
            reason: ToolFailureReason::EffectUnknown,
            ..
        })
    ));
}
