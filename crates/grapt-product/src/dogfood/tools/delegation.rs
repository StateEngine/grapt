//! Dispatching the `subagent` tool: admit a child, run it, settle the operation.
//!
//! Named for the ACT rather than for the tool, because `dogfood::subagent` — the
//! durable domain this dispatches into — already owns that name, and a module
//! called `tools::subagent` would shadow it for every path written inside here.
//!
//! It sits beside the other effect runners rather than inside the executor's
//! match, because it is the one tool whose work is a whole durable operation
//! with a lifecycle of its own — and because `tools.rs` is a module about
//! authority and dispatch, not about any single tool's domain.

use super::*;

tool_arguments! {
    pub(super) struct SubagentArguments => SUBAGENT_TOOL {
        required task: NonEmptyString,
    }
}

pub(super) fn prepare_subagent(
    root: &Path,
    input: &BoundedToolInput,
) -> Result<PreparedEffect, String> {
    let args: SubagentArguments = decode_input(input)?;
    if args.task.as_str().trim().is_empty() {
        return Err("subagent task must not be empty".to_owned());
    }
    // `ToolInvoke`, NOT `ProcessSpawn`. A subagent child is an in-process
    // durable operation on this same product; claiming a process spawn would
    // assert a program identity and an argv that do not exist, and would put
    // the call under capability law written for executables.
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::Subagent {
            task: args.task.as_str().to_owned(),
        },
        action: SUBAGENT_TOOL.to_owned(),
        // The RESOURCE is the task, because that is what the approval is
        // actually about: a user approving a subagent is approving this
        // question being asked, not the word "subagent".
        resource: args.task.as_str().to_owned(),
    })
}

/// Admit one child, run it, settle its operation, and hand back its answer.
///
/// The three steps are deliberately separate durable facts. Admission records
/// the child and its lineage before anything runs; the run belongs to the one
/// execution owner; the settlement records what the run actually became. A
/// tool that returned text without settling would leave a durable operation
/// running forever with nothing to reconcile it against.
pub(super) async fn run_subagent(
    seam: &crate::dogfood::subagent::ChildSeam,
    parent: &SessionId,
    agent: &AgentId,
    principal: &SealedPrincipal,
    call: &ToolCallId,
    task: &str,
    cancellation: &grapt_runtime::ToolCancellation,
) -> Result<Vec<u8>, String> {
    // A seam that cannot be upgraded means the application is gone. That is a
    // composition defect, and it REFUSES rather than quietly answering without
    // a child — an empty success here would be a subagent tool that never ran
    // one and said nothing about it.
    let application = seam
        .get()
        .ok_or("the child execution seam was never bound")?
        .upgrade()
        .ok_or("the application is gone; no child can be run")?;

    let admitted =
        crate::dogfood::subagent::admit(&application, parent, agent, task, Some(call.clone()))?;

    // THE OPERATION IS SETTLED EVEN IF THIS CALL NEVER RETURNS.
    //
    // Review blocker B1, reproduced twice: the settlement below sits AFTER the await,
    // and an interrupted parent DROPS this future — so it never ran, the row
    // stayed `running` with a live generation, and the NEXT startup relabelled
    // it `ProcessLoss`. That is two lies for the price of one: the operation was
    // not running, and it was not lost with its runtime — it was cancelled, and
    // this process knew it at the time.
    //
    // I had already learned this about the CHILD and guarded that; the operation
    // is the other half of the same fact and did not get the same treatment. It
    // does now, in its own guard rather than folded into the child's: the two
    // settle different things, and a guard that did both would have one reason
    // to be disarmed and two jobs to leave half-done.
    //
    // Settling twice is not a hazard here. `settle_on` is a CAS from `running`,
    // so the explicit settlement below and this drop path cannot both apply —
    // whichever runs first wins and the other is a no-op.
    let settlement = SettleOnDrop {
        application: &application,
        operation: &admitted.operation,
        armed: std::cell::Cell::new(true),
    };

    let outcome = crate::dogfood::subagent::run_child(
        &application,
        principal.clone(),
        parent,
        &admitted.session,
        task,
        subagent_budget(),
        cancellation,
    )
    .await;

    // SETTLED AS WHAT ACTUALLY HAPPENED, by matching the outcome's TYPE.
    //
    // This used to ask whether the error string contained "budget", which meant
    // the two cancellation paths that say nothing of the sort — an interrupted
    // parent, and a parent whose execution ended — were recorded as `Failed`.
    // The child had not failed; it had been cancelled, and the row said the
    // wrong thing about why.
    //
    // The success terminal is built FIRST, because `Completed` carries a
    // verified artifact and cannot be minted from nothing. If that capture or
    // verification fails, the operation settles Failed with that reason rather
    // than being reported completed on the strength of text nothing durable
    // holds.
    use crate::dogfood::subagent::ChildOutcome;
    let terminal = match &outcome {
        ChildOutcome::Answered(answer) => crate::dogfood::subagent::completed(&application, answer)
            .unwrap_or_else(|error| grapt_subagent::SubagentTerminal::Failed { error }),
        unfinished => unfinished_terminal(unfinished),
    };
    let completed = matches!(terminal, grapt_subagent::SubagentTerminal::Completed { .. });
    crate::dogfood::subagent::settle(&application, &admitted.operation, &terminal)?;
    settlement.disarm();

    // A child whose answer could not be captured is NOT a successful tool call,
    // even though the child itself answered: the parent would be handed text
    // the operation record does not agree it produced.
    let ChildOutcome::Answered(answer) = outcome else {
        return Err(outcome.reason().to_owned());
    };
    if !completed {
        return Err("the child answered but its output could not be recorded".to_owned());
    }
    Ok(answer.into_bytes())
}

/// The durable terminal for a child that did not answer.
///
/// Separate and pure so it can be witnessed directly. It replaces a classifier
/// that asked whether the error string contained "budget" — under which the two
/// cancellation paths that say nothing of the sort, an interrupted parent and a
/// parent whose execution ended, were both recorded as `Failed`. The child had
/// not failed. A reader of the row could not tell a cancelled child from a
/// broken one, which is the difference between "somebody stopped this" and
/// "this does not work".
fn unfinished_terminal(
    outcome: &crate::dogfood::subagent::ChildOutcome,
) -> grapt_subagent::SubagentTerminal {
    use crate::dogfood::subagent::ChildOutcome;
    match outcome {
        // A budget expiry is a cancellation this process performed, not a
        // failure of the child's own: it was stopped, and `Interrupted` is the
        // reason the domain has for that.
        ChildOutcome::Cancelled(_) | ChildOutcome::Budget(_) => {
            grapt_subagent::SubagentTerminal::Cancelled(
                grapt_subagent::SubagentCancelReason::Interrupted,
            )
        }
        ChildOutcome::Failed(error) => grapt_subagent::SubagentTerminal::Failed {
            error: error.clone(),
        },
        // Unreachable in the caller, which handles the answer before asking
        // this. Recorded as a failure rather than silently as a cancellation,
        // because an answer that arrived here would mean the caller's own match
        // had drifted.
        ChildOutcome::Answered(answer) => grapt_subagent::SubagentTerminal::Failed {
            error: format!("an answered child reached the unfinished terminal: {answer}"),
        },
    }
}

/// Settles the operation `Cancelled(Interrupted)` if this call ends without
/// settling it itself.
///
/// The one case it exists for is the one no `return` can cover: the tool future
/// is DROPPED when its parent is interrupted, so the settlement after the await
/// never runs. A durable operation left `running` by a process that is still
/// alive is a lie about the present, and a lie about the past as soon as the
/// next startup calls it process loss.
struct SettleOnDrop<'a> {
    application: &'a crate::dogfood::ServerApplication,
    operation: &'a grapt_types::SubagentOperationId,
    armed: std::cell::Cell<bool>,
}

impl SettleOnDrop<'_> {
    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for SettleOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed.get() {
            return;
        }
        // Interrupted, not ProcessLoss: this runtime is alive and is recording
        // what it knows. ProcessLoss is what a LATER runtime concludes about an
        // operation nobody settled, and it must stay reserved for that.
        //
        // The refusal is deliberately not propagated — this runs on a drop path
        // where there is no caller left to tell. An operation that was already
        // settled refuses the CAS here, which is the outcome this wanted.
        let terminal = grapt_subagent::SubagentTerminal::Cancelled(
            grapt_subagent::SubagentCancelReason::Interrupted,
        );
        let _settled =
            crate::dogfood::subagent::settle(self.application, self.operation, &terminal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dogfood::subagent::ChildOutcome;

    #[test]
    fn a_cancelled_child_is_recorded_as_cancelled_and_not_as_a_failure() {
        // THE CLASSIFIER USED TO BE A STRING TEST. `error.contains("budget")`
        // meant the two paths that stop a child without it failing — an
        // interrupted parent, and a parent whose execution ended under it —
        // were both written to the durable row as `Failed`. The review named it
        // alongside B1, and it is the same defect in a different costume: the
        // row said the child broke when somebody had stopped it.
        for cancelled in [
            ChildOutcome::Cancelled("the parent was interrupted (User)".to_owned()),
            ChildOutcome::Cancelled("the parent's execution ended before its child did".to_owned()),
            ChildOutcome::Budget("the child did not settle within its budget".to_owned()),
        ] {
            assert!(
                matches!(
                    unfinished_terminal(&cancelled),
                    grapt_subagent::SubagentTerminal::Cancelled(
                        grapt_subagent::SubagentCancelReason::Interrupted
                    )
                ),
                "{cancelled:?} is a cancellation, whatever words it carries"
            );
        }
    }

    #[test]
    fn a_child_that_really_failed_keeps_its_reason() {
        // The other half, and what keeps the one above honest: a classifier
        // that answered `Cancelled` for everything would pass that test and
        // erase every real failure from the record.
        let failed = ChildOutcome::Failed("the child did not succeed: ProviderFailure".to_owned());
        match unfinished_terminal(&failed) {
            grapt_subagent::SubagentTerminal::Failed { error } => assert_eq!(
                error, "the child did not succeed: ProviderFailure",
                "the child's own reason reaches the durable row"
            ),
            other => panic!("a failed child must settle Failed, got {other:?}"),
        }
    }
}
