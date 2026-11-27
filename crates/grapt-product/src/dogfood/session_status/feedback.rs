//! Read-only backoff feedback tied to the active execution identity.
use crate::dogfood::execution::LiveActiveExecutions;
use grapt_api::u3a::SessionStatusInfo;
use grapt_runtime::step::RetryNotice;
use grapt_types::{ExecutionId, ProviderId, SessionId};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(in crate::dogfood) struct RetryFeedback {
    active: Arc<LiveActiveExecutions>,
    session: SessionId,
    execution: ExecutionId,
    provider: ProviderId,
}
/// Advisory ownership follows the compaction future, including cancellation.
pub(in crate::dogfood) struct CompactingGuard(RetryFeedback);
impl Drop for CompactingGuard {
    fn drop(&mut self) {
        self.0
            .active
            .compacting(&self.0.session, &self.0.execution, false);
    }
}
impl RetryFeedback {
    pub(in crate::dogfood) fn attach_input_waits(
        &self,
        waits: Arc<crate::dogfood::human_input::Waits>,
    ) {
        self.active
            .attach_input_waits(&self.session, &self.execution, waits);
    }

    pub(in crate::dogfood) fn input_changed(&self) {
        self.active.record_state(
            &self.session,
            &self.execution,
            grapt_service_state::Change::Update,
        );
    }

    pub(in crate::dogfood) fn compacting(&self) -> CompactingGuard {
        self.active.compacting(&self.session, &self.execution, true);
        CompactingGuard(self.clone())
    }

    pub(in crate::dogfood) fn new(
        active: Arc<LiveActiveExecutions>,
        session: SessionId,
        execution: ExecutionId,
        provider: ProviderId,
    ) -> Self {
        Self {
            active,
            session,
            execution,
            provider,
        }
    }
    pub(in crate::dogfood) fn provider_slot(&self, position: Option<usize>) {
        let Some(emitter) = self.active.status_emitter() else {
            return;
        };
        let status = if let Some(position) = position {
            let Some(status) = emitter.provider_slot_info(&self.provider, position) else {
                return;
            };
            status
        } else {
            SessionStatusInfo::busy()
        };
        self.active.retrying(&self.session, &self.execution, status);
    }

    pub(in crate::dogfood) fn observe(&self, notice: Option<RetryNotice>) {
        let Some(emitter) = self.active.status_emitter() else {
            return;
        };
        let status = match notice {
            Some(notice) => {
                let Some(status) = emitter.waiting_info(&self.provider, notice) else {
                    return;
                };
                status
            }
            None => SessionStatusInfo::busy(),
        };
        self.active.retrying(&self.session, &self.execution, status);
    }
}
