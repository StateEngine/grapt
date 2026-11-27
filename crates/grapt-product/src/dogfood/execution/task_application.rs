//! A subagent operation owns one task, including its Steers. Queued human
//! followups keep their own serialized wake and execution identity.
use super::*;
use grapt_runtime::supervisor::BoxFuture;
use grapt_runtime::{
    Boundary, Cancellation, CoordinatorError, RunOutcome, RuntimeDrain, SessionApplication,
};

pub(super) struct OneLocation(pub(super) Arc<LiveLocation>);

impl std::fmt::Debug for OneLocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OneLocation(<live-provider>)")
    }
}

impl LocationServiceMap for OneLocation {
    type Runtime = LiveLocation;

    fn get(&self, _location: &LocationId) -> Result<Arc<Self::Runtime>, LocationError> {
        Ok(Arc::clone(&self.0))
    }
}

#[derive(Debug)]
pub(super) struct LiveApplication {
    pub(super) runtime: RuntimeApplication<OneLocation>,
    pub(super) inbox: Arc<Mutex<Inbox>>,
    pub(super) principals: Arc<PromptPrincipals>,
}

pub(super) struct TaskDrain {
    runtime: RuntimeDrain<LiveLocation>,
    task: Option<MessageId>,
    promoted: bool,
    external_notice: Option<super::super::host_control::ExternalNoticeFence>,
}

impl SessionApplication for LiveApplication {
    type Drain = TaskDrain;

    fn session_exists(&self, session: &SessionId) -> Result<bool, CoordinatorError> {
        self.runtime.session_exists(session)
    }

    fn begin_drain(&self, session: &SessionId) -> Result<Self::Drain, CoordinatorError> {
        let single_task = {
            let inbox = self
                .inbox
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            grapt_subagent::SubagentService::child_context_on(inbox.store(), session)
                .map_err(|error| CoordinatorError::Inbox {
                    session: session.as_str().into(),
                    reason: error.to_string(),
                })?
                .is_some()
        };
        let task = single_task
            .then(|| {
                self.principals
                    .get(session)
                    .map(|principal| principal.input)
                    .ok_or_else(|| CoordinatorError::Run {
                        session: session.as_str().into(),
                        reason: "subagent task has no authenticated input".into(),
                    })
            })
            .transpose()?;
        Ok(TaskDrain {
            runtime: self.runtime.begin_drain(session)?,
            task,
            promoted: false,
            external_notice: self
                .principals
                .get(session)
                .and_then(|principal| principal.external_notice),
        })
    }

    fn continuation_owed(&self, drain: &Self::Drain) -> bool {
        self.runtime.continuation_owed(&drain.runtime)
    }

    fn next_eligible(
        &self,
        drain: &mut Self::Drain,
        boundary: Boundary,
    ) -> Result<Option<Promotion>, CoordinatorError> {
        // A busy Queue has its own WakeSlots owner waiting behind this task.
        // Finish this execution before that owner can promote and run it.
        // Steers still join at every safe boundary of the original task.
        if drain.promoted && boundary == Boundary::Idle {
            return Ok(None);
        }
        let external_notice = drain.external_notice.as_ref();
        let session = drain.runtime.session().clone();
        let promotion = self
            .runtime
            .next_eligible_if(&mut drain.runtime, boundary, |inbox| {
                if let Some(fence) = external_notice {
                    fence
                        .validate_within(inbox)
                        .map_err(|refusal| CoordinatorError::Run {
                            session: session.as_str().into(),
                            reason: refusal.to_string(),
                        })?;
                }
                Ok(true)
            })?;
        if promotion.is_some() {
            // Later safe boundaries belong to the drain that actually started.
            drain.external_notice = None;
        }
        drain.promoted |= promotion
            .as_ref()
            .is_some_and(|promotion| drain.task.as_ref() == Some(promotion.input.message()));
        Ok(promotion)
    }

    fn advance<'a>(
        &'a self,
        drain: &'a mut Self::Drain,
        cancellation: Cancellation,
    ) -> BoxFuture<'a, Result<RunOutcome, CoordinatorError>> {
        self.runtime.advance(&mut drain.runtime, cancellation)
    }
}

/// A Steer already consumed by the preceding task has no independent drain.
/// Its waiting wake cannot take ownership of a later human Queue.
pub(super) fn already_promoted(
    inbox: &Mutex<Inbox>,
    session: &SessionId,
    input: &MessageId,
) -> Result<bool, String> {
    let inbox = inbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if grapt_subagent::SubagentService::child_context_on(inbox.store(), session)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Ok(false);
    }
    inbox
        .promoted_cursor(session, input)
        .map(|cursor| cursor.is_some())
        .map_err(|error| error.to_string())
}

impl LiveExecutor {
    /// Keep operation cancellation inside the wake that owns its task. A late
    /// signal cannot reach a successor after this wake releases its custody.
    pub(super) async fn wake_task(
        &self,
        session: &SessionId,
        cancelled: &mut tokio::sync::watch::Receiver<bool>,
    ) -> (Result<WakeOutcome, CoordinatorError>, bool) {
        let initial_interrupt = std::sync::atomic::AtomicBool::new(false);
        let wake = self.coordinator.wake_with(session.clone(), || {
            let interrupted = self.active.hand_off(session);
            initial_interrupt.store(interrupted, std::sync::atomic::Ordering::Release);
            interrupted.then(|| self.active.interruption_source(session))
        });
        tokio::pin!(wake);
        let mut stopped = false;
        let result = tokio::select! {
            biased;
            _ = async { let _ = cancelled.wait_for(|interrupted| *interrupted).await; } => {
                match self.coordinator.interrupt_by(session, self.active.interruption_source(session)) {
                    Ok(outcome) => {
                        stopped = outcome == grapt_runtime::InterruptOutcome::Interrupted;
                        wake.await
                    }
                    Err(error) => Err(error),
                }
            }
            result = &mut wake => result,
        };
        (
            result,
            stopped || initial_interrupt.load(std::sync::atomic::Ordering::Acquire),
        )
    }
}

impl LiveExecutor {
    pub(in crate::dogfood) fn shutdown(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        reason: super::super::lifecycle::StopReason,
    ) -> Result<(), ExecutionRefusal> {
        self.active
            .cancel_for_shutdown(session, execution, reason)
            .map_err(ExecutionRefusal::Run)
    }
    pub(in crate::dogfood) fn interrupt_target(
        &self,
        session: &SessionId,
        expected: Option<&ExecutionId>,
    ) -> Result<Option<ExecutionId>, String> {
        self.active.cancel_target(session, expected)
    }

    pub fn interrupt(
        &self,
        session: &SessionId,
        execution: Option<&ExecutionId>,
    ) -> Result<grapt_runtime::InterruptOutcome, ExecutionRefusal> {
        if let Some(execution) = execution {
            self.active
                .cancel_execution(session, execution)
                .map_err(ExecutionRefusal::Run)?;
            return Ok(grapt_runtime::InterruptOutcome::Interrupted);
        }
        self.active
            .validate(session, None)
            .map_err(ExecutionRefusal::Run)?;
        let outcome = self.interrupted.observe(session, || {
            // A preparing Session has no drain yet; its signal carries the
            // interrupt into the drain it starts. Once that drain exists the
            // coordinator interrupts it directly.
            self.active.cancel_for_user(session);
            let preparing = self.active.cancel_preparing(session);
            let draining = self
                .coordinator
                .interrupt_by(session, self.active.interruption_source(session))
                .map_err(|error| ExecutionRefusal::Run(error.to_string()))?;
            Ok(if preparing {
                grapt_runtime::InterruptOutcome::Interrupted
            } else {
                draining
            })
        })?;
        self.wake_slots.cancel_waiters(session);
        Ok(outcome)
    }
}
