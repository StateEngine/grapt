//! Typed subagent admission and execution through the existing Session owner.
//!
//! Admission selects empty or forked conversation with parent lineage. Foreground operations
//! deliver tool results; background operations deliver typed inbox facts.

use std::sync::{Arc, Mutex};

use grapt_subagent::{SubagentMode, SubagentService};
use grapt_types::{AgentId, ClockAuthority, SessionId, SubagentOperationId, ToolCallId};

use super::DogfoodApplication;

mod admission;
mod placement;
mod run_output;
pub(super) use admission::{admit_with_options, ChildOptions};
pub(super) use run_output::child_output_for_execution;

/// What a child run BECAME, as a value rather than as a sentence.
///
/// The caller has to turn this into a durable terminal, and it used to do that
/// by asking whether the error string contained "budget". That is a classifier
/// built on prose: the parent-interrupted and parent-gone paths say neither
/// "budget" nor anything else it looked for, so both settled `Failed` when what
/// actually happened was a cancellation. A reader of the operation row could not
/// tell an interrupted child from a child that broke.
///
/// Each variant carries the reason it will be recorded with, so the mapping in
/// `run_subagent` is a match on a type and cannot drift from the words.
#[derive(Debug)]
pub(super) enum ChildOutcome {
    /// The child settled `Ended` and said something.
    Answered(String),
    /// The parent stopped: interrupted, or its execution ended under it.
    Cancelled(String),
    /// The child outlived its budget and was cancelled for it.
    Budget(String),
    /// Anything else — the child's own failure, an unsettled Step, a refusal.
    Failed(String),
}

impl ChildOutcome {
    /// The reason text, for the tool's own failure detail.
    pub(super) fn reason(&self) -> &str {
        match self {
            Self::Answered(text) => text,
            Self::Cancelled(reason) | Self::Budget(reason) | Self::Failed(reason) => reason,
        }
    }
}

/// The late-bound route from the synchronous tool seam back to the execution
/// owner.
///
/// The application builds the executor, which builds the location, which builds
/// the tool runner — so the runner cannot simply be handed the application. The
/// seam is created before any of them, threaded down, and populated with a
/// `Weak` immediately after the application is in its `Arc`.
///
/// It holds a `Weak` on purpose: the tool runner outlives nothing, but the
/// application owns the executor that owns this, and an `Arc` here would be a
/// cycle that never drops. A seam that cannot be upgraded means the application
/// is gone, which is a refusal, never a silently skipped child.
pub(super) type ChildSeam = Arc<std::sync::OnceLock<std::sync::Weak<DogfoodApplication>>>;

/// One admitted subagent operation and the Session it will run in.
#[derive(Debug, Clone)]
pub(super) struct AdmittedChild {
    /// The child's own Session. Distinct from the operation address, and never
    /// a substitute for it.
    pub session: SessionId,
    /// The durable operation address.
    pub operation: SubagentOperationId,
}

/// Admit a foreground child in its parent's directory.
pub(super) fn admit(
    application: &DogfoodApplication,
    parent: &SessionId,
    agent: &AgentId,
    description: &str,
    tool_call: Option<ToolCallId>,
) -> Result<AdmittedChild, String> {
    admit_with_options(
        application,
        parent,
        agent,
        description,
        tool_call,
        &ChildOptions::new(SubagentMode::Foreground),
    )
}

/// The model one child runs on.
struct ChildRouting {
    /// Selected on the child Session; `None` leaves it on the configured target,
    /// exactly where an unselected parent runs.
    selection: Option<serde_json::Value>,
    /// The resolved target recorded on the admission.
    target: Option<grapt_types::ModelRef>,
}

/// Explicit Subagent model selection wins, then the named definition's model,
/// then the parent's target. Unusable selections refuse before creating a child.
fn child_routing(
    application: &DogfoodApplication,
    parent: &SessionId,
    parent_model: Option<&serde_json::Value>,
    definition_model: Option<&serde_json::Value>,
) -> Result<ChildRouting, String> {
    let explicit = {
        let held = application.lock_inbox();
        grapt_session::selection::subagent_model(held.store().connection(), parent)
            .map_err(|error| format!("the Subagent model selection is unreadable: {error}"))?
    };
    let selection = match explicit {
        Some(model) => {
            let model = super::selection::resolve_model(application, &model).map_err(|error| {
                format!("the selected Subagent model cannot run this child: {error}")
            })?;
            Some(model)
        }
        None => definition_model
            .or(parent_model)
            .map(|model| super::selection::resolve_model(application, model))
            .transpose()?,
    };
    let target = application
        .executor
        .session_target(selection.as_ref())
        .map_err(|error| format!("the child model target cannot be resolved: {error}"))?;
    Ok(ChildRouting { selection, target })
}

/// Take this process's runtime generation, and settle what the last one left.
///
/// Called once at composition, before anything can run. An operation left
/// `running` by a process that is gone has lost its execution owner, so nothing
/// will ever settle it, and a reconcile
/// would read it as work still to do. It settles `Cancelled(ProcessLoss)` —
/// never replayed, because a child that may already have spoken to a provider
/// must not be silently run a second time.
///
/// The generation is one past the highest ever recorded, so this process cannot
/// be mistaken for its predecessor and cannot settle its OWN live operations.
pub(super) fn adopt_runtime(
    inbox: &Mutex<grapt_inbox::Inbox>,
    now_ms: i64,
) -> Result<grapt_types::RuntimeGeneration, String> {
    let mut held = inbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    SubagentService::ensure_schema_on(held.store()).map_err(|error| error.to_string())?;
    let generation =
        SubagentService::next_generation_on(held.store()).map_err(|error| error.to_string())?;
    SubagentService::settle_dead_generation_on(held.store_mut(), generation, now_ms)
        .map_err(|error| error.to_string())?;
    SubagentService::cancel_stale_followups_on(held.store_mut(), generation, now_ms)
        .map_err(|error| error.to_string())?;
    Ok(generation)
}

/// Only the initial tool submission claims the operation. Its answer and
/// cancellation are bound to the execution minted by that caller.
pub(super) fn claim_single_prompt(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<(), String> {
    use grapt_types::CasOutcome;

    let now = application.clock.now().as_millis();
    let mut held = application.lock_inbox();
    let Some((operation, _status)) =
        SubagentService::child_operation_on(held.store(), session).map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    // Started under THIS PROCESS's generation, so a restart can tell the
    // operations it owns from the ones a dead runtime left behind. `settle`
    // still calls `start_on` for the operation whose run failed BEFORE it was
    // ever prompted, where this claim never ran; that call is a no-op CAS once
    // this one has applied.
    match SubagentService::start_on(
        held.store_mut(),
        &operation,
        application.runtime_generation,
        now,
    )
    .map_err(|e| e.to_string())?
    {
        CasOutcome::Applied => Ok(()),
        CasOutcome::AlreadyTerminal => Err(format!(
            "subagent operation {} already has its initial prompt",
            operation.as_str()
        )),
        CasOutcome::NotLive => Err(format!(
            "subagent operation {} disappeared between reading and claiming it",
            operation.as_str()
        )),
    }
}

/// Human input uses the normal Session inbox once initial custody is claimed.
/// Never let it race the tool for the operation's first execution.
pub(super) fn allow_human_prompt(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<(), String> {
    use grapt_subagent::OperationStatus;
    let context = {
        let held = application.lock_inbox();
        SubagentService::child_context_on(held.store(), session)
            .map_err(|error| error.to_string())?
    };
    match context.map(|context| context.status) {
        Some(OperationStatus::ExternallyAmbiguous(_)) => {
            Err("the subagent outcome is ambiguous; resolve it before prompting".into())
        }
        Some(OperationStatus::Terminal(grapt_subagent::SubagentTerminal::Cancelled(
            grapt_subagent::SubagentCancelReason::ProcessLoss,
        ))) => Err("the subagent was lost with its runtime; start a successor instead".into()),
        Some(OperationStatus::Admitted) => {
            Err("the subagent is starting; retry once it is running".into())
        }
        Some(OperationStatus::Running(generation))
            if generation != application.runtime_generation =>
        {
            Err("the subagent belongs to an earlier runtime; resume it before prompting".into())
        }
        Some(OperationStatus::Running(_)) if live_execution(application, session).is_none() => {
            Err("the subagent is starting or settling; retry once its status changes".into())
        }
        _ => Ok(()),
    }
}

/// Resolve a direct child for interruption, cancelling an unclaimed prompt
/// while the same inbox guard excludes `claim_single_prompt`.
pub(super) fn interrupt_target_context(
    application: &DogfoodApplication,
    parent: &SessionId,
    child: &SessionId,
) -> Result<grapt_subagent::ChildOperationContext, String> {
    use grapt_subagent::{OperationStatus, SubagentCancelReason, SubagentTerminal};
    use grapt_types::CasOutcome;

    let mut held = application.lock_inbox();
    let context = SubagentService::child_context_on(held.store(), child)
        .map_err(|error| error.to_string())?
        .filter(|context| &context.parent == parent)
        .ok_or("the target is not a direct child of this session")?;
    // One inbox guard orders this cancellation against every followup claim.
    SubagentService::cancel_child_followups_on(
        held.store_mut(),
        child,
        application.clock.now().as_millis(),
    )
    .map_err(|error| error.to_string())?;
    application.wake_slots.cancel_waiters(child);
    if context.status == OperationStatus::Admitted {
        let now = application.clock.now().as_millis();
        let started = SubagentService::start_on(
            held.store_mut(),
            &context.operation,
            application.runtime_generation,
            now,
        )
        .map_err(|error| error.to_string())?;
        if started != CasOutcome::Applied {
            return Err("the child's admission changed; retry its interruption".into());
        }
        let settled = SubagentService::settle_on(
            held.store_mut(),
            &context.operation,
            &SubagentTerminal::Cancelled(SubagentCancelReason::Interrupted),
            now,
        )
        .map_err(|error| error.to_string())?;
        if settled != CasOutcome::Applied {
            return Err("the child's cancellation could not be recorded".into());
        }
    }
    Ok(context)
}

/// Record what the child's run actually became.
///
/// The caller decides the terminal, because only the caller knows whether the
/// answer could be captured — `Completed` carries a verified artifact and
/// cannot be minted here from a bare string.
///
/// The operation is started and settled in one call rather than left admitted:
/// an operation that stayed `admitted` after its child had already run would be
/// replayed by a later reconcile as work still to do.
pub(super) fn settle(
    application: &DogfoodApplication,
    operation: &SubagentOperationId,
    terminal: &grapt_subagent::SubagentTerminal,
) -> Result<(), String> {
    let now = application.clock.now().as_millis();
    let mut held = application.lock_inbox();
    // This process's generation: the same one the prompt claim used, so a
    // settlement here cannot look like a different runtime's.
    SubagentService::start_on(
        held.store_mut(),
        operation,
        application.runtime_generation,
        now,
    )
    .map_err(|error| error.to_string())?;
    SubagentService::settle_on(held.store_mut(), operation, terminal, now)
        .map_err(|error| error.to_string())?;
    drop(held);
    application.agent_activity.send_replace(());
    Ok(())
}

/// The verified, artifact-backed terminal for a child that answered.
///
/// `Completed` carries an `ArtifactBackedOutput`, which VERIFIES: it stats the
/// artifact, checks its reference, disposition and kind, and re-reads it whole.
/// So the answer has to be captured before the operation can be settled as
/// completed — which is why the capture happens in the tool's own bounded
/// writer first and this is called with what that produced, rather than the
/// settlement trying to manufacture an artifact of its own.
/// The operation's output is captured HERE rather than borrowed from the tool's
/// own terminal artifact. `CapturedToolArtifact` deliberately seals its
/// writer-minted provenance — an executor may name a terminal class but may not
/// mint artifact provenance — and reaching across that seal to reuse the
/// reference would defeat exactly the guarantee it exists for. The operation's
/// durable output and the tool's terminal artifact are two different facts with
/// two different owners, so each captures its own.
pub(super) fn completed(
    application: &DogfoodApplication,
    answer: &str,
) -> Result<grapt_subagent::SubagentTerminal, String> {
    use grapt_artifact::{ArtifactKind, ArtifactStore, CaptureBound, Sensitivity};

    let store = application.executor.artifacts();
    let bound = CaptureBound::new(super::ARTIFACT_READ_BYTES, 64 * 1_024)
        .ok_or("the child output capture bound is zero")?;
    let captured = store
        .capture(
            std::io::Cursor::new(answer.as_bytes().to_vec()),
            bound,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .map_err(|error| format!("the child's output could not be captured: {error:?}"))?;

    // The projection is BOUNDED separately from the artifact. The whole answer
    // lives in the artifact; this is the summary the durable row carries, and
    // a large child answer must not be able to grow that row without limit.
    let projection: String = answer.chars().take(MAX_PROJECTION_CHARS).collect();
    let output = grapt_subagent::ArtifactBackedOutput::verify(
        store,
        captured.reference().clone(),
        captured.disposition(),
        projection,
        MAX_PROJECTION_BYTES,
    )
    .map_err(|error| format!("the child's output did not verify: {error:?}"))?;
    Ok(grapt_subagent::SubagentTerminal::Completed { output })
}

/// Characters kept in the durable projection. Deliberately below
/// [`MAX_PROJECTION_BYTES`] so a multi-byte answer cannot exceed the byte bound
/// the verifier enforces.
const MAX_PROJECTION_CHARS: usize = 2_000;

/// The model-facing projection ceiling for one child's answer.
///
/// The whole answer is in the artifact; this bounds only the summary the
/// operation row carries, so a large child answer cannot grow a durable row
/// without limit.
const MAX_PROJECTION_BYTES: u64 = 8 * 1_024;

/// How often the child's liveness is re-read while waiting for its terminal.
const CHILD_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Run one admitted child to its terminal, and hand back what it produced.
///
/// ## Why this waits, and on what
///
/// The prompt future is NOT the answer. It resolves on custody acceptance —
/// the execution owner submits an owned task and reports back once it has taken
/// custody, after which runtime and Step terminals own the outcome. Settling a
/// foreground operation on that would report a child as finished the instant it
/// started, and the tool would return an empty result as a success.
///
/// Wait for the exact initial execution's durable terminal. Live projection
/// alone misses fast runs and may already describe a human followup instead.
/// Capacity remains a synchronous refusal; no inbox lock survives a poll.
pub(super) async fn run_child(
    application: &DogfoodApplication,
    principal: grapt_api::SealedPrincipal,
    parent: &SessionId,
    child: &SessionId,
    prompt: &str,
    budget: std::time::Duration,
    cancellation: &grapt_runtime::ToolCancellation,
) -> ChildOutcome {
    match waited(
        application,
        principal,
        parent,
        child,
        prompt,
        budget,
        cancellation,
    )
    .await
    {
        Ok(answer) => ChildOutcome::Answered(answer),
        Err(outcome) => outcome,
    }
}

/// The wait itself. Its `Err` IS the outcome, so every exit is classified where
/// it is decided rather than re-derived from a message later.
#[allow(clippy::too_many_arguments)]
async fn waited(
    application: &DogfoodApplication,
    principal: grapt_api::SealedPrincipal,
    parent: &SessionId,
    child: &SessionId,
    prompt: &str,
    budget: std::time::Duration,
    cancellation: &grapt_runtime::ToolCancellation,
) -> Result<String, ChildOutcome> {
    use grapt_api::u3a::WireId;

    let request = super::support::decode(serde_json::json!({
        "text": prompt,
        "delivery": "steer",
        "resume": true,
    }))
    .map_err(|error| ChildOutcome::Failed(format!("child prompt is invalid: {error}")))?;
    let addressed: WireId<SessionId> = WireId::parse(child.as_str().to_owned())
        .ok_or_else(|| ChildOutcome::Failed("child session id is invalid".to_owned()))?;

    let execution = application.mint_execution().map_err(ChildOutcome::Failed)?;
    application
        .submit_child_prompt(principal, addressed, request, execution.clone())
        .await
        .map_err(|error| ChildOutcome::Failed(format!("the child was not admitted: {error:?}")))?;

    // Bounded. A child that never settles must fail this call rather than hold
    // the parent's Step open forever — and under a gate, a deadlock has to
    // surface as a failure rather than as a hang.
    let deadline = tokio::time::Instant::now() + budget;

    // Parent liveness still covers cancellation when its tool future survives.
    // A durable terminal below handles a child too fast to observe live.
    let parent_execution = live_execution(application, parent);

    // Dropping a parent tool cancels only the execution it started. A human
    // may already have started a successor in the same child Session.
    let cancel = CancelChildOnDrop {
        application,
        child,
        execution: &execution,
        armed: std::cell::Cell::new(true),
    };
    loop {
        if let Some(source) = cancellation.cancelled_by() {
            return Err(ChildOutcome::Cancelled(format!(
                "the parent was interrupted ({source:?})"
            )));
        }
        if parent_execution.is_some() && live_execution(application, parent) != parent_execution {
            // The Step that asked for this child is gone, so its answer can no
            // longer be delivered to anyone. Stop waiting; the guard cancels.
            return Err(ChildOutcome::Cancelled(
                "the parent's execution ended before its child did".to_owned(),
            ));
        }
        if settled_execution(application, child, &execution).map_err(ChildOutcome::Failed)? {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            // A tool call that timed out and walked away would leave the child
            // running against a parent Step that has already failed — an orphan
            // spending capacity and provider budget for an answer nobody will
            // read. The guard cancels it on the way out.
            return Err(ChildOutcome::Budget(
                "the child did not settle within its budget".to_owned(),
            ));
        }
        tokio::time::sleep(CHILD_POLL).await;
    }

    // Settled on its own: there is nothing left to cancel, and interrupting a
    // Session that has already finished would publish an interruption that did
    // not happen.
    cancel.disarm();
    let answer = child_output_for_execution(application, child, &execution)?;
    if answer.trim().is_empty() {
        return Err(ChildOutcome::Failed(
            "the child settled without producing an answer".into(),
        ));
    }
    Ok(answer)
}

/// Cancellation follows the original execution, never a later human followup.
struct CancelChildOnDrop<'a> {
    application: &'a DogfoodApplication,
    child: &'a SessionId,
    execution: &'a grapt_types::ExecutionId,
    armed: std::cell::Cell<bool>,
}

impl CancelChildOnDrop<'_> {
    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for CancelChildOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        // The refusal is deliberately not propagated: this runs on a drop path,
        // including one where the parent's Step no longer exists to be told.
        // A child that had already stopped refuses here, which is the outcome
        // this wanted anyway.
        let _cancelled = self
            .application
            .executor
            .interrupt(self.child, Some(self.execution));
    }
}

/// The execution running for one Session right now, if any.
///
/// The tool and active-session API read the same live execution registry.
fn live_execution(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Option<grapt_types::ExecutionId> {
    application
        .executor
        .active_executions()
        .into_iter()
        .find(|(held, _)| held == session)
        .map(|(_, execution)| execution)
}

/// Poll without blocking the async tool on an inbox owned by the child.
/// A closed observation is final even if a successor is already active.
fn settled_execution(
    application: &DogfoodApplication,
    child: &SessionId,
    execution: &grapt_types::ExecutionId,
) -> Result<bool, String> {
    let Ok(held) = application.inbox.try_lock() else {
        return Ok(false);
    };
    let observations = grapt_session::execution::read(held.store().connection(), child, execution)?;
    Ok(observations.last().is_some_and(|row| row.outcome.is_some()))
}

/// What the child said, read from its own durable history.
///
/// The child's transcript is the product's record, not something this tool
/// accumulates while watching: a result assembled from live frames would be a
/// second transcript that could disagree with the one the child actually has.
pub(super) fn child_output(
    application: &DogfoodApplication,
    child: &SessionId,
    admitted_seq: u64,
    mode: SubagentMode,
) -> Result<String, ChildOutcome> {
    // ABSENCE FROM THE ACTIVE MAP IS LIVENESS, NOT SUCCESS.
    //
    // This is the correction that matters most here. Waiting for the child to
    // stop running says only that it stopped — it says nothing about HOW. A
    // child that was cancelled, that failed, or whose Step never settled at all
    // would still leave that map, and reading the transcript at that point
    // would hand the parent whatever text happened to be there and call it an
    // answer. So the output is gated on the durable Step TERMINAL.
    //
    // `terminal: None` is an unsettled Step and is explicitly NOT a success.
    // `Failed` is not a success. Only `Ended` is, and only then is any text
    // read at all.
    // Bound to THIS admission by durable position: the answer must have opened
    // after the prompt this call admitted. That holds whether or not the live
    // interval was ever observed, which is what makes the fast-child race safe
    // rather than merely unlikely.
    // ONE SELECTION, and the text comes from the message that selection
    // validated.
    //
    // This used to be two independent selections: a terminal was proved on the
    // newest settled message, and then the newest ASSISTANT ROW was rendered
    // from a separate history read with nothing tying the two together. Because
    // the emitted text never depended on the validated message, a later partial
    // result could borrow an earlier success — and mutating the terminal gate
    // changed no observable behaviour at all, which is exactly how the gate came
    // to have a surviving mutant.
    //
    // The message is chosen once, validated, and then rendered BY IDENTITY.
    if let Some(reason) =
        run_output::capacity_failure_after_admission(application, child, admitted_seq)?
    {
        return Err(ChildOutcome::Failed(reason));
    }
    let candidate = application
        .message_history(child)
        .map_err(ChildOutcome::Failed)?
        .into_iter()
        .rfind(|message| message.opened_at_seq > admitted_seq);
    let Some(candidate) = candidate else {
        return Err(ChildOutcome::Failed(format!(
            "the child opened no Step after admission {admitted_seq}"
        )));
    };

    // History projections select by the admission floor; operation owners
    // select a closed execution instead.
    match &candidate.terminal {
        Some(grapt_session::StepTerminal::Ended { .. }) => {}
        Some(grapt_session::StepTerminal::Failed { reason }) => {
            let detail = format!("the child did not succeed: {reason:?}");
            return Err(
                if matches!(reason, grapt_types::StepTerminalReason::Interrupted { .. }) {
                    ChildOutcome::Cancelled(detail)
                } else {
                    ChildOutcome::Failed(detail)
                },
            );
        }
        // An unsettled Step. Distinct from `Failed` because the two say
        // different things to a reader: one ran and lost, the other never
        // finished. Neither is a success.
        None => {
            return Err(ChildOutcome::Failed(format!(
                "the child's Step {} never settled",
                candidate.message.as_str()
            )))
        }
    }

    let rendered = application
        .assistant_json(child, &candidate)
        .map_err(ChildOutcome::Failed)?;
    let row: serde_json::Value =
        serde_json::from_str(&rendered).map_err(|error| ChildOutcome::Failed(error.to_string()))?;
    let text = assistant_text(&row);
    if mode == SubagentMode::Foreground && text.trim().is_empty() {
        // A foreground tool needs an answer to return. A background task may
        // finish through tools alone; its durable success remains a success.
        return Err(ChildOutcome::Failed(format!(
            "the child's Step {} settled without producing an answer",
            candidate.message.as_str()
        )));
    }
    Ok(text)
}

/// The TEXT parts of one assistant row, joined.
///
/// Reasoning and tool parts are deliberately dropped: what the parent asked for
/// is the child's answer, and folding a child's private reasoning into a tool
/// result would put it in the parent's transcript where the user never asked
/// for it.
fn assistant_text(row: &serde_json::Value) -> String {
    row.get("content")
        .and_then(serde_json::Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "subagent_tests.rs"]
mod tests;
