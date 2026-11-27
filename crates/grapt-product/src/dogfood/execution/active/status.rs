//! Execution-owned advisory status, ordered with hydration and settlement.
use super::*;
impl LiveActiveExecutions {
    pub(in crate::dogfood) fn offer_status_hydration(
        &self,
        emitter: &crate::dogfood::session_status::SessionStatusEmitter,
        frames: &mut Vec<DogfoodEvent>,
    ) -> Vec<SessionId> {
        let active = self.lock();
        let preparing = self.lock_preparing();
        let sessions = Self::listed_sessions(&active, &preparing);
        for session in &sessions {
            let entry = active.get(session);
            let phase = entry
                .filter(|entry| entry.compacting)
                .map(|_| grapt_api::u3a::SessionStatusPhase::Compacting);
            let offered = match entry.and_then(|entry| entry.retry.as_ref()) {
                Some(status) => emitter.offer_active_phase(session, status.clone(), phase, frames),
                None if phase.is_some() => emitter.offer_active_phase(
                    session,
                    grapt_api::u3a::SessionStatusInfo::busy(),
                    phase,
                    frames,
                ),
                None => emitter.offer_active(session, frames),
            };
            if !offered {
                break;
            }
        }
        sessions
    }

    /// Subscriber -> registry is also hydration's lock order. A late owner can
    /// neither publish for a successor nor restore Retry after settlement.
    pub(in crate::dogfood) fn retrying(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        status: grapt_api::u3a::SessionStatusInfo,
    ) {
        let Some(emitter) = &self.status else {
            return;
        };
        let Some(compacting) = self
            .lock()
            .get(session)
            .filter(|entry| &entry.execution == execution)
            .map(|entry| entry.compacting)
        else {
            return;
        };
        let phase = compacting.then_some(grapt_api::u3a::SessionStatusPhase::Compacting);
        emitter.publish_status_if_phase(session, status.clone(), phase, || {
            let mut active = self.lock();
            let Some(entry) = active
                .get_mut(session)
                .filter(|entry| &entry.execution == execution && entry.compacting == compacting)
            else {
                return false;
            };
            entry.retry =
                matches!(status, grapt_api::u3a::SessionStatusInfo::Retry { .. }).then_some(status);
            emitter.note_active(session);
            true
        });
        self.record_state(session, execution, grapt_service_state::Change::Update);
    }

    pub(in crate::dogfood) fn compacting(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        compacting: bool,
    ) {
        let Some(emitter) = &self.status else {
            return;
        };
        let phase = compacting.then_some(grapt_api::u3a::SessionStatusPhase::Compacting);
        emitter.publish_status_if_phase(
            session,
            grapt_api::u3a::SessionStatusInfo::busy(),
            phase,
            || {
                let mut active = self.lock();
                let Some(entry) = active.get_mut(session).filter(|entry| {
                    &entry.execution == execution && entry.compacting != compacting
                }) else {
                    return false;
                };
                entry.compacting = compacting;
                // Entering and leaving a compaction retires its prior wait notice.
                entry.retry = None;
                emitter.note_active(session);
                true
            },
        );
        self.record_state(session, execution, grapt_service_state::Change::Update);
    }
}
