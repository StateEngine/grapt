//! Lifecycle cancellation stays on the exact execution entry.
use super::*;
impl LiveActiveExecutions {
    pub(in crate::dogfood::execution) fn shutdown_reason(
        &self,
        session: &SessionId,
        execution: Option<&ExecutionId>,
    ) -> Option<crate::dogfood::lifecycle::StopReason> {
        self.lock()
            .get(session)
            .filter(|entry| execution.is_none_or(|expected| expected == &entry.execution))
            .and_then(|entry| entry.shutdown)
    }
    pub(in crate::dogfood::execution) fn interruption_source(
        &self,
        session: &SessionId,
    ) -> grapt_types::InterruptionSource {
        if self.shutdown_reason(session, None).is_some() {
            grapt_types::InterruptionSource::ManagedShutdown
        } else {
            grapt_types::InterruptionSource::User
        }
    }
    pub(in crate::dogfood::execution) fn cancel_for_user(&self, session: &SessionId) {
        if let Some(entry) = self.lock().get(session) {
            entry.cancel.send_replace(true);
        }
    }
    pub(in crate::dogfood::execution) fn cancel_for_shutdown(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        reason: crate::dogfood::lifecycle::StopReason,
    ) -> Result<(), String> {
        let mut active = self.lock();
        let entry = active
            .get_mut(session)
            .filter(|entry| &entry.execution == execution)
            .ok_or("the active execution identity changed")?;
        if !*entry.cancel.borrow() {
            entry.shutdown = Some(reason);
        }
        entry.cancel.send_replace(true);
        Ok(())
    }
}
