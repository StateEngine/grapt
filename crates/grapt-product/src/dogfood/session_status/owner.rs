//! Bind journaled status observations to the registry's current execution.
use crate::dogfood::execution::LiveActiveExecutions;
use crate::dogfood::human_input;
use grapt_service_state::{Action, Change, Kind, Reason, Snapshot, State};
use grapt_types::{ExecutionId, SessionId};

pub(in crate::dogfood) enum ConfirmationError {
    OwnerChanged,
    OwnerGone,
    CommitUnavailable,
}

/// A single registry observation; never retained as a second activity map.
#[derive(Clone)]
pub(in crate::dogfood) struct StatusInputs {
    pub execution: ExecutionId,
    pub retry: Option<grapt_api::u3a::SessionStatusInfo>,
    pub phase: Option<grapt_api::u3a::SessionStatusPhase>,
    pub available: bool,
    pub human_input: Option<human_input::Kind>,
    pub preparing: bool,
    pub cancelled: bool,
    pub shutdown: Option<crate::dogfood::lifecycle::StopReason>,
}

impl LiveActiveExecutions {
    pub(in crate::dogfood) fn admission_paused(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        input: &grapt_types::MessageId,
        reason: &str,
    ) {
        let Some(emitter) = self.status_emitter() else {
            return;
        };
        let result = emitter.observe_owned(
            session,
            execution,
            Change::Start,
            |inbox| {
                if !inbox
                    .pending_inputs(session)
                    .map_err(|error| error.to_string())?
                    .iter()
                    .any(|pending| &pending.input == input)
                {
                    return Ok(None);
                }
                if self
                    .status_inputs(session, None)
                    .is_some_and(|entry| &entry.execution != execution)
                {
                    return Ok(None);
                }
                Ok(Some(State::described(
                    Kind::Paused,
                    Reason::Preparing,
                    reason,
                    Some(Action::InspectSession),
                )))
            },
            |_| {},
            |snapshot| {
                self.status_presentation(session, snapshot)
                    .map_err(|_| "session status owner is unconfirmed".to_owned())
            },
        );
        if let Err(error) = result {
            emitter.observation_unavailable(session, &error);
        }
    }

    pub(in crate::dogfood) fn observe_release(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        released: StatusInputs,
    ) {
        let Some(emitter) = self.status_emitter() else {
            return;
        };
        let result = emitter.observe_owned(session, execution, Change::Recover,
            |_| {
                if self.status_inputs(session, None).is_some() { return Ok(None); }
                let state = match (released.cancelled, released.shutdown) {
                    (true, shutdown) => {
                        let (reason, message) = match shutdown {
                            Some(crate::dogfood::lifecycle::StopReason::ServeWindowExpired) =>
                                (Reason::WindowClosed, "The serving window closed; the prompt and work so far are kept."),
                            Some(_) => (Reason::Shutdown, "The service stopped this execution; the prompt and work so far are kept."),
                            None => (Reason::User, "The execution was interrupted; the prompt and work so far are kept."),
                        };
                        State::described(Kind::Interrupted, reason, message, Some(Action::FollowUp))
                    }
                    _ => State::described(Kind::Disconnected, Reason::ProcessLoss,
                        "The execution owner stopped without a terminal status; work so far is kept. Send a follow-up prompt to continue.", Some(Action::FollowUp)),
                };
                Ok(Some(state))
            }, |_| {},
            |snapshot| self.status_presentation(session, snapshot).map_err(|_| "session status owner is unconfirmed".to_owned()),
        );
        if let Err(error) = result {
            emitter.observation_unavailable(session, &error);
        }
    }

    pub(in crate::dogfood) fn validate_state(
        &self,
        session: &SessionId,
        snapshot: &Snapshot,
    ) -> Result<(), ConfirmationError> {
        confirm(self.status_inputs(session, None).as_ref(), snapshot)
    }

    pub(in crate::dogfood) fn status_presentation(
        &self,
        session: &SessionId,
        snapshot: &Snapshot,
    ) -> Result<crate::dogfood::session_status::Presentation, ConfirmationError> {
        let inputs = self.status_inputs(session, None);
        let entry = inputs.as_ref();
        confirm(entry, snapshot)?;
        let status = if snapshot.state.active() {
            entry
                .and_then(|entry| entry.retry.clone())
                .unwrap_or_else(grapt_api::u3a::SessionStatusInfo::busy)
        } else {
            grapt_api::u3a::SessionStatusInfo::idle()
        };
        let phase = (snapshot.state.active() && entry.is_some_and(|entry| entry.phase.is_some()))
            .then_some(grapt_api::u3a::SessionStatusPhase::Compacting);
        Ok(crate::dogfood::session_status::Presentation { status, phase })
    }

    /// No registry guard survives the call into the journal; the publisher
    /// captures the current owner only after taking the store's lock.
    pub(in crate::dogfood) fn record_state(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        change: Change,
    ) {
        let Some(emitter) = self.status_emitter() else {
            return;
        };
        let result = emitter.observe_owned(
            session,
            execution,
            change,
            |_| Ok(self.current_state(session, execution)),
            |available| {
                self.status_committed(session, execution, available);
            },
            |snapshot| {
                self.status_presentation(session, snapshot)
                    .map_err(|_| "session status owner is unconfirmed".to_owned())
            },
        );
        if let Err(error) = result {
            emitter.observation_unavailable(session, &error);
        }
    }

    fn current_state(&self, session: &SessionId, execution: &ExecutionId) -> Option<State> {
        let entry = self.status_inputs(session, Some(execution))?;
        if let Some(kind) = entry.human_input {
            return Some(match kind {
                human_input::Kind::Permission => State::described(
                    Kind::Waiting,
                    Reason::Permission,
                    "Waiting for permission; review the open approval request.",
                    Some(Action::ReviewPermission),
                ),
                human_input::Kind::Question => State::described(
                    Kind::Waiting,
                    Reason::Question,
                    "Waiting for your answer to the open question.",
                    Some(Action::AnswerQuestion),
                ),
            });
        }
        if let Some(grapt_api::u3a::SessionStatusInfo::Retry { action, .. }) = &entry.retry {
            let provider_slot = action
                .as_ref()
                .is_some_and(|action| action.reason() == "provider_slot");
            return Some(if provider_slot {
                State::described(
                    Kind::Waiting,
                    Reason::ProviderSlot,
                    "Waiting for a provider slot; queued work will resume automatically.",
                    Some(Action::Wait),
                )
            } else {
                State::described(
                    Kind::Waiting,
                    Reason::ProviderRetry,
                    "The provider is retrying; work so far is kept.",
                    Some(Action::Wait),
                )
            });
        }
        if entry.phase.is_some() {
            return Some(State::described(
                Kind::Running,
                Reason::Compacting,
                "Compacting context before continuing.",
                None,
            ));
        }
        if entry.preparing {
            return Some(State::described(
                Kind::Waiting,
                Reason::Preparing,
                "Preparing this execution; the prompt is admitted.",
                Some(Action::Wait),
            ));
        }
        Some(State::plain(Kind::Running))
    }
}

fn confirm(entry: Option<&StatusInputs>, snapshot: &Snapshot) -> Result<(), ConfirmationError> {
    match entry {
        Some(entry) if snapshot.execution.as_ref() != Some(&entry.execution) => {
            Err(ConfirmationError::OwnerChanged)
        }
        Some(entry) if !entry.available => Err(ConfirmationError::CommitUnavailable),
        None if snapshot.state.active() => Err(ConfirmationError::OwnerGone),
        _ => Ok(()),
    }
}
