//! The registry supplies owned observations to the session-status owner.
use super::*;
use crate::dogfood::{
    human_input::Waits,
    session_status::{SessionStatusEmitter, StatusInputs},
};

impl LiveActiveExecutions {
    pub(in crate::dogfood) fn status_emitter(&self) -> Option<&SessionStatusEmitter> {
        self.status.as_deref()
    }

    pub(in crate::dogfood) fn status_inputs(
        &self,
        session: &SessionId,
        expected: Option<&ExecutionId>,
    ) -> Option<StatusInputs> {
        let active = self.lock();
        let entry = active
            .get(session)
            .filter(|entry| expected.is_none_or(|expected| expected == &entry.execution))?;
        let cancelled = *entry.cancel.borrow();
        Some(StatusInputs {
            execution: entry.execution.clone(),
            retry: entry.retry.clone(),
            phase: entry
                .compacting
                .then_some(grapt_api::u3a::SessionStatusPhase::Compacting),
            available: entry.state_available,
            human_input: entry.human_input.as_ref().and_then(|waits| waits.pending()),
            preparing: self
                .lock_preparing()
                .get(session)
                .is_some_and(|entry| !entry.handed_off),
            cancelled,
            shutdown: entry.shutdown,
        })
    }

    pub(in crate::dogfood) fn status_committed(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        available: bool,
    ) {
        if let Some(entry) = self
            .lock()
            .get_mut(session)
            .filter(|entry| &entry.execution == execution)
        {
            entry.state_available = available;
        }
    }

    pub(in crate::dogfood::execution) fn release_state(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
    ) {
        let released = self.status_inputs(session, Some(execution));
        self.settle(session, execution);
        if let Some(released) = released {
            self.observe_release(session, execution, released);
        }
    }

    pub(in crate::dogfood) fn attach_input_waits(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        waits: Arc<Waits>,
    ) {
        if let Some(entry) = self
            .lock()
            .get_mut(session)
            .filter(|entry| &entry.execution == execution)
        {
            entry.human_input = Some(waits);
        }
    }
}
