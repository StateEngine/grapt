//! Background terminals belong to the existing service-owned prompt future.

use super::{backend_result::ExecutionFailure, subagent, ServerApplication};
use grapt_subagent::{
    OperationStatus, SubagentCancelReason, SubagentMode, SubagentService, SubagentTerminal,
};
use grapt_types::{ExecutionId, SessionId, SubagentOperationId};
use tokio::sync::watch;

pub(super) struct BackgroundChild {
    application: ServerApplication,
    operation: SubagentOperationId,
    child: SessionId,
    admitted_seq: u64,
    execution: Option<ExecutionId>,
    wake_cancellation: Option<watch::Receiver<bool>>,
    cancellation: watch::Receiver<bool>,
    armed: bool,
}

impl BackgroundChild {
    pub(super) fn for_prompt(
        application: &ServerApplication,
        child: &SessionId,
        admitted_seq: u64,
        cancellation: watch::Receiver<bool>,
    ) -> Result<Option<Self>, String> {
        let context = {
            let held = application.lock_inbox();
            SubagentService::child_context_on(held.store(), child)
                .map_err(|error| error.to_string())?
        };
        let Some(context) = context.filter(|context| context.mode == SubagentMode::Background)
        else {
            return Ok(None);
        };
        if !matches!(context.status, OperationStatus::Running(generation) if generation == application.runtime_generation)
        {
            return Err(
                "the background child has no running admission owned by this runtime".into(),
            );
        }
        Ok(Some(Self {
            application: application.clone(),
            operation: context.operation,
            child: child.clone(),
            admitted_seq,
            execution: None,
            wake_cancellation: None,
            cancellation,
            armed: true,
        }))
    }

    pub(super) fn for_execution(
        application: &ServerApplication,
        child: &SessionId,
        admitted_seq: u64,
        execution: &ExecutionId,
        cancellation: watch::Receiver<bool>,
    ) -> Result<Option<Self>, String> {
        let mut owner = Self::for_prompt(application, child, admitted_seq, cancellation)?;
        if let Some(owner) = &mut owner {
            owner.execution = Some(execution.clone());
        }
        Ok(owner)
    }

    /// The atomic claim already validated this new background run. Construct
    /// its drop owner before the next await can abandon the claimed operation.
    pub(super) fn for_followup(
        application: &ServerApplication,
        run: &grapt_subagent::FollowupRun,
        cancellation: watch::Receiver<bool>,
        wake_cancellation: watch::Receiver<bool>,
    ) -> Self {
        Self {
            application: application.clone(),
            operation: run.operation.clone(),
            child: run.child.clone(),
            admitted_seq: run.admitted_seq,
            execution: Some(run.execution.clone()),
            wake_cancellation: Some(wake_cancellation),
            cancellation,
            armed: true,
        }
    }

    fn was_cancelled(&self) -> bool {
        *self.cancellation.borrow()
            || self
                .wake_cancellation
                .as_ref()
                .is_some_and(|cancelled| *cancelled.borrow())
    }

    pub(super) fn finish(
        &mut self,
        execution: &Result<(), ExecutionFailure>,
    ) -> Result<(), String> {
        use subagent::ChildOutcome;
        let outcome = match &self.execution {
            Some(execution) => {
                subagent::child_output_for_execution(&self.application, &self.child, execution)
            }
            None => subagent::child_output(
                &self.application,
                &self.child,
                self.admitted_seq,
                SubagentMode::Background,
            ),
        };
        let terminal = match (execution, outcome) {
            (Ok(()), Ok(answer)) => subagent::completed(&self.application, &answer)
                .unwrap_or_else(|error| SubagentTerminal::Failed { error }),
            (_, Err(ChildOutcome::Cancelled(_) | ChildOutcome::Budget(_))) => {
                SubagentTerminal::Cancelled(SubagentCancelReason::Interrupted)
            }
            (Err(ExecutionFailure::Published(error)), _) => SubagentTerminal::Failed {
                error: error.clone(),
            },
            _ if self.was_cancelled() => {
                SubagentTerminal::Cancelled(SubagentCancelReason::Interrupted)
            }
            (Err(ExecutionFailure::Unpublished(error)), _) => SubagentTerminal::Failed {
                error: error.clone(),
            },
            (Ok(()), Err(outcome)) => SubagentTerminal::Failed {
                error: outcome.reason().to_owned(),
            },
        };
        subagent::settle(&self.application, &self.operation, &terminal)?;
        self.application.agent_activity.send_replace(());
        self.armed = false;
        Ok(())
    }
}

impl Drop for BackgroundChild {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let terminal = if self.was_cancelled() {
            SubagentTerminal::Cancelled(SubagentCancelReason::Interrupted)
        } else {
            SubagentTerminal::Failed {
                error: "the owned background execution was abandoned".into(),
            }
        };
        if let Err(error) = subagent::settle(&self.application, &self.operation, &terminal) {
            self.application.hub.invalidate_session_because(
                &self.child,
                &format!(
                    "session {}: background settlement failed: {error}",
                    self.child
                ),
            );
        }
        self.application.agent_activity.send_replace(());
    }
}
