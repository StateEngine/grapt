use super::*;
use grapt_session::execution::{Failure, Observation, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ObservationPublication {
    Published,
    Unavailable,
}

pub(super) fn start(
    location: &LiveLocation,
    session: &SessionId,
    principal: &PromptPrincipal,
) -> Result<(), ExecutionRefusal> {
    if principal
        .observed
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Ok(());
    }
    // Called only by the coordinator-owned chain, after its capacity and
    // cancellation channel exist. A wake with no eligible input starts nothing.
    let observation = {
        let mut inbox = match location.inbox.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        let authorities = DeterministicAuthorities::new(&*location.clock, &*location.ids);
        grapt_service_state::lifecycle::begin(
            inbox.store_mut(),
            &authorities,
            session,
            &principal.execution,
        )
        .map_err(ExecutionRefusal::Run)?
    };
    principal
        .observed
        .store(true, std::sync::atomic::Ordering::Release);
    match crate::dogfood::session_status::lifecycle_event(observation) {
        Ok(event) => {
            if let Err(error) = location.hub.publish(&event) {
                location.hub.invalidate_because(&format!(
                    "session {session}: execution start could not be published: {error}"
                ));
            }
        }
        Err(error) => location.hub.invalidate_because(&format!(
            "session {session}: execution start could not be projected: {error}"
        )),
    }
    if let Some(status) = &location.active.status {
        if let Err(error) = status.publish_current(session, |snapshot| {
            location
                .active
                .status_presentation(session, snapshot)
                .map_err(|_| "session status owner is unconfirmed".to_owned())
        }) {
            location.hub.invalidate_because(&format!(
                "session {session}: status publication unavailable: {error}"
            ));
        }
    }
    Ok(())
}

/// Record the MCP sources this turn could not use, when that changed since
/// the Session's last notice. A notice is advisory: one that cannot be
/// written never costs the user the turn.
pub(super) fn record_mcp_notice(
    location: &LiveLocation,
    session: &SessionId,
    execution: &ExecutionId,
    unavailable: Vec<grapt_session::notice::Unavailable>,
) {
    let mut inbox = match location.inbox.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    let authorities = DeterministicAuthorities::new(&*location.clock, &*location.ids);
    let _ = grapt_session::notice::record_mcp_unavailable(
        inbox.store_mut(),
        &authorities,
        session,
        execution,
        unavailable,
    );
}

/// Name each config key this turn's settings were given that SE does not
/// apply, once per Session. Advisory as an MCP notice is: one that cannot be
/// written never costs the user the turn.
pub(super) fn record_config_notices(
    location: &LiveLocation,
    session: &SessionId,
    execution: &ExecutionId,
    compaction: &crate::dogfood::compaction_config::CompactionSettings,
) {
    let mut inbox = match location.inbox.lock() {
        Ok(held) => held,
        Err(poisoned) => poisoned.into_inner(),
    };
    let authorities = DeterministicAuthorities::new(&*location.clock, &*location.ids);
    for ignored in compaction.ignored() {
        let _ = grapt_session::notice::record_config_ignored(
            inbox.store_mut(),
            &authorities,
            session,
            execution,
            ignored.key,
            ignored.reason,
        );
    }
}

impl ExecutionRefusal {
    /// The product-authored reason, without the refusal's own label.
    pub(super) fn reason(self) -> String {
        match self {
            Self::Compose(reason) | Self::Run(reason) | Self::PublishedFailure(reason) => reason,
        }
    }
}

impl LiveExecutor {
    /// A turn blocked before its acknowledgement: the prompt stays admitted
    /// and queued, the acceptance says why, and the Session's history says so
    /// durably when the notice can be written.
    pub(super) fn not_run_yet(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        input: &grapt_types::MessageId,
        started: &super::super::prompt_driver::Started,
        reason: String,
    ) -> Result<Option<super::WakeOutcome>, ExecutionRefusal> {
        let recorded = if notice_append_faulted() {
            Err("testkit: the notice append was refused".to_owned())
        } else {
            let mut inbox = match self.inbox.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            grapt_session::notice::record_not_run_yet(
                inbox.store_mut(),
                &authorities,
                session,
                execution,
                input,
                &reason,
            )
        };
        // The input is admitted and queued either way, so it is never
        // refused: the acceptance itself carries the
        // reason, and a notice that cannot be written is the server's warning.
        if let Err(error) = recorded {
            crate::diagnostics::report(format!(
                "session {session}: prompt {input} not run yet, and its notice could not be written: {error}"
            ));
        }
        self.active
            .admission_paused(session, execution, input, &reason);
        started.settle_queued(reason.clone());
        self.hub.reconcile_because(
            grapt_api::ReconcileReason::UnpublishedAdvisoryNotice,
            &format!("session {session}: prompt not run yet: {reason}"),
        );
        Ok(None)
    }

    /// Tell the user why an execution stopped on its context, and what to
    /// do next: the model kept repeating what its context already summarized
    /// until the context was full, or its summary of the conversation was
    /// unusable twice. Its failure keeps the standard
    /// context-limit class every build reads; the notice is advisory, as
    /// every notice is.
    pub(super) fn note_context_stop(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        error: &grapt_runtime::CoordinatorError,
    ) {
        let grapt_runtime::CoordinatorError::Context {
            failure, reason, ..
        } = error
        else {
            return;
        };
        let (repeated, what) = match failure {
            grapt_types::ContextFailure::CompactionRepeated => {
                (true, "on a repeating, full context")
            }
            // The coordinator carries the Step's detail after its own
            // "step N failed: <reason>: " label.
            grapt_types::ContextFailure::CompactionNoProgress
                if reason.contains(crate::dogfood::continuation::REFUSED_SUMMARY) =>
            {
                (false, "on an unusable summary")
            }
            _ => return,
        };
        let recorded = {
            let mut inbox = match self.inbox.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            if repeated {
                grapt_session::notice::record_context_repeated(
                    inbox.store_mut(),
                    &authorities,
                    session,
                    execution,
                )
                .map(|_| ())
            } else {
                grapt_session::notice::record_compaction_failed(
                    inbox.store_mut(),
                    &authorities,
                    session,
                    execution,
                )
                .map(|_| ())
            }
        };
        match recorded {
            Ok(()) => self.hub.reconcile_because(
                grapt_api::ReconcileReason::UnpublishedAdvisoryNotice,
                &format!("session {session}: execution stopped {what}"),
            ),
            Err(error) => {
                crate::diagnostics::report(format!(
                "session {session}: execution {execution} stopped {what}, and its notice could not be written: {error}"
                ));
            }
        }
    }

    /// Record that the service itself stopped `execution`'s turn, so a service
    /// that comes back can offer it again. The advisory record never costs the
    /// turn: a write that fails is a server warning, and the interruption
    /// terminal already committed is what clients read.
    ///
    /// The stop cause is read from the live registry, so this must run while the
    /// execution's active entry still exists: settlement removes it.
    pub(super) fn record_interrupted_turn(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        input: &grapt_types::MessageId,
    ) {
        let Some(stopped) = self.active.shutdown_reason(session, Some(execution)) else {
            return;
        };
        // The user's own stop is theirs: it is never recorded as resumable.
        let Some(by) = stopped.resume_cause() else {
            return;
        };
        {
            let mut inbox = match self.inbox.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            if let Err(error) = grapt_session::interrupted_turn::record_interrupted_turn(
                inbox.store_mut(),
                &authorities,
                session,
                execution,
                input,
                by,
                grapt_session::interrupted_turn::Resume::Unsupported,
            ) {
                crate::server_log::warn(format_args!(
                    "session {session}: stopped turn {execution} was not recorded as resumable: {error}"
                ));
                return;
            }
        }
        self.hub.reconcile_because(
            grapt_api::ReconcileReason::UnpublishedAdvisoryNotice,
            &format!("session {session}: the service stopped a running turn"),
        );
    }

    /// Settle this execution's terminal, and record a managed stop as a turn a
    /// later service may offer again.
    ///
    /// The stop cause and the turn's own input are read here, while the active
    /// entry and its principal still exist: [`Self::announce`] settles the entry
    /// away, so a later read could not answer.
    pub(super) fn observe_execution(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        outcome: Option<Outcome>,
        failure: &Failure,
    ) -> Result<ObservationPublication, ExecutionRefusal> {
        let input = self
            .principals
            .get(session)
            .map(|principal| principal.input);
        let durable = {
            let mut inbox = match self.inbox.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            match outcome {
                None => grapt_service_state::lifecycle::begin(
                    inbox.store_mut(),
                    &authorities,
                    session,
                    execution,
                ),
                Some(Outcome::Failed) => grapt_service_state::lifecycle::settle_failed(
                    inbox.store_mut(),
                    &authorities,
                    session,
                    execution,
                    failure.clone(),
                ),
                Some(Outcome::Interrupted) => grapt_service_state::lifecycle::settle_interrupted(
                    inbox.store_mut(),
                    &authorities,
                    session,
                    execution,
                    self.active
                        .shutdown_reason(session, Some(execution))
                        .map_or(
                            grapt_session::execution::InterruptionReason::User,
                            |reason| reason.durable(),
                        ),
                ),
                Some(outcome) => grapt_service_state::lifecycle::settle(
                    inbox.store_mut(),
                    &authorities,
                    session,
                    execution,
                    outcome,
                ),
            }
        };
        if let (Some(input), Some(Outcome::Interrupted)) = (input, outcome) {
            self.record_interrupted_turn(session, execution, &input);
        }
        announce(&self.active, &self.hub, session, execution, durable)
    }

    pub(in crate::dogfood) fn observe_abandoned(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
    ) {
        // No invented Step terminal and no restart authority. A failed store
        // remains a failed store; force clients to reconcile its actual state.
        let _unrecorded =
            self.observe_execution(session, execution, Some(Outcome::Failed), &Failure::Generic);
        self.hub.invalidate_because(&format!(
            "session {session}: execution {execution} abandoned"
        ));
    }
}

/// The public class of a failed wake: a coordinator failure mapped to the
/// closed public vocabulary.
///
/// Typed causes the runtime already knows are named; the rest keep the generic
/// text, so no arbitrary runtime or provider string is published.
///
/// This match is deliberately exhaustive over a closed enum. A `_` arm once
/// sent every cause it did not name to `Failure::Generic`, so a variant added
/// later would silently publish the opaque sentence. Naming each one means a new
/// variant fails to compile until someone decides what the reader is told. There
/// is no runtime witness for that: the guarantee is the compile error itself.
pub(super) fn public_failure(error: &grapt_runtime::CoordinatorError) -> Failure {
    use grapt_runtime::CoordinatorError;
    match error {
        CoordinatorError::ProviderCapacity { .. } => Failure::ProviderCapacity,
        CoordinatorError::ProviderLimit { reset, .. } => Failure::ProviderLimit { reset: *reset },
        CoordinatorError::StepBudgetExhausted {
            policy,
            spent,
            granted,
            ..
        } => Failure::StepBudgetExhausted {
            policy: policy.clone(),
            spent: *spent,
            granted: *granted,
        },
        CoordinatorError::Context {
            failure: grapt_types::ContextFailure::ContinuityUnavailable,
            continuity: Some(failure),
            ..
        } => Failure::Continuity(*failure),
        CoordinatorError::Context { .. } => Failure::ContextLimit,
        // The provider's own refusal, already bounded and redacted at the
        // adapter and carried here as typed text. The row quotes it instead of
        // the generic sentence, which is the ruling: show the cause by default.
        CoordinatorError::ProviderFailed { summary, .. } => {
            Failure::ProviderFailed(summary.clone())
        }
        // The remaining variants describe the coordinator's own inability to
        // serve a known request (an unknown Session, a placement or inbox read
        // that failed, a dropped owner, closed capacity) or a run whose detail
        // was already discarded. None carries a cause the work itself produced,
        // so the fixed sentence is accurate. Listing them means a new variant
        // cannot be added without deciding what the reader is told.
        CoordinatorError::UnknownSession { .. }
        | CoordinatorError::Placement { .. }
        | CoordinatorError::Inbox { .. }
        | CoordinatorError::Run { .. }
        | CoordinatorError::OwnerAbandoned { .. }
        | CoordinatorError::CapacityClosed => Failure::Generic,
    }
}

/// Fault injection for the notice-append-failure witness: the append is
/// skipped as refused. Testkit builds only.
#[cfg(feature = "loopback-provider-testkit")]
fn notice_append_faulted() -> bool {
    std::env::var_os("GRAPT_TESTKIT_FAIL_NOTICE_APPEND").is_some()
}

#[cfg(not(feature = "loopback-provider-testkit"))]
fn notice_append_faulted() -> bool {
    false
}

/// Announce a durable execution observation. Its terminal leaves the active
/// registry before it is published, so a client that sees the terminal never
/// sees the execution active after it, and one that sees it idle can already
/// read the terminal. A terminal that failed to append
/// settles nothing: its entry stays until its guard drops, so the registry
/// never claims idle ahead of a durable failure.
pub(super) fn announce(
    active: &LiveActiveExecutions,
    hub: &DogfoodEventHub,
    session: &SessionId,
    execution: &ExecutionId,
    durable: Result<Observation, String>,
) -> Result<ObservationPublication, ExecutionRefusal> {
    let observation = durable.map_err(ExecutionRefusal::Run)?;
    let terminal = observation.outcome.is_some();
    if terminal {
        active.settle(session, execution);
    }
    // Keep legacy terminal activity last; the revisioned detail is already
    // committed, and a failed live offer cannot swallow its lifecycle frame.
    let status_published = active.status.as_ref().is_none_or(|status| {
        let result = status.publish_current(session, |snapshot| {
            active
                .status_presentation(session, snapshot)
                .map_err(|_| "session status owner is unconfirmed".to_owned())
        });
        if let Err(error) = &result {
            hub.invalidate_because(&format!(
                "session {session}: status publication unavailable: {error}"
            ));
        }
        result.is_ok()
    });
    // A dropped listener cannot turn a committed observation into failure.
    match crate::dogfood::session_status::lifecycle_event(observation) {
        Ok(event) => {
            if let Err(error) = hub.publish(&event) {
                hub.invalidate_because(&format!(
                    "session {session}: execution observation could not be published: {error}"
                ));
                return Ok(ObservationPublication::Unavailable);
            }
        }
        Err(error) => {
            hub.invalidate_because(&format!(
                "session {session}: execution observation could not be projected: {error}"
            ));
            return Ok(ObservationPublication::Unavailable);
        }
    }
    Ok(if status_published {
        ObservationPublication::Published
    } else {
        ObservationPublication::Unavailable
    })
}

/// Keep a compactor's closed cause in the existing execution failure message.
/// No raw provider detail or new durable observation path is introduced.
pub(super) fn public_failure_with_compaction(
    location: &LiveLocation,
    session: &SessionId,
    error: &grapt_runtime::CoordinatorError,
) -> Failure {
    if matches!(
        error,
        grapt_runtime::CoordinatorError::Context {
            failure: grapt_types::ContextFailure::CompactionFailed
                | grapt_types::ContextFailure::CompactionNoProgress
                | grapt_types::ContextFailure::CompactionDidNotFit,
            ..
        }
    ) {
        if let Some(cause) = location.compaction_recovery.take_failure(session) {
            return Failure::CompactionRecovery(cause.public());
        }
        if matches!(
            error,
            grapt_runtime::CoordinatorError::Context {
                failure: grapt_types::ContextFailure::CompactionDidNotFit,
                ..
            }
        ) {
            return Failure::CompactionRecovery(
                location.compaction_recovery.did_not_fit(session).public(),
            );
        }
    }
    public_failure(error)
}
