//! `/reconcile`: what process loss left running is recorded as unknown.
//!
//! A nested start with no terminal blocks every prompt, rewind and undo, and
//! names this route as the way out. The Session must be idle, so nothing can
//! still be running it; each orphan then gains exactly an unknown-effect
//! terminal through the runtime's reconciliation owner, and the model is warned
//! about it by the ordinary unknown-effect notice.

use super::*;
use grapt_api::u3a::{SessionReconcileApplicationError as Error, SessionReconcileResponseDto};

impl DogfoodApplication {
    pub(super) fn reconcile(
        &self,
        session: SessionId,
    ) -> Result<SessionReconcileResponseDto, Error> {
        context::require_session(self, &session).map_err(|error| match error {
            SessionLookupApplicationError::Unavailable => Error::InvalidRequest(invalid(
                "History is temporarily unavailable; retry the request",
            )),
            SessionLookupApplicationError::SessionNotFound(value) => Error::SessionNotFound(value),
            SessionLookupApplicationError::InvalidRequest(value) => Error::InvalidRequest(value),
        })?;
        // The same slot excludes admitted and active owners in both backends.
        // It is acquired before the inbox, and no guard crosses an await.
        let _idle = self
            .wake_slots
            .reserve_idle(&session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        self.ensure_session_writer(&session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let mut inbox = self.lock_inbox();
        let orphans = continuation::orphaned_nested(inbox.store(), &session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        grapt_runtime::reconcile_orphaned_nested(
            inbox.store_mut(),
            &session,
            &orphans,
            &execution::RuntimeEventIds(Arc::clone(&self.ids)),
            &execution::RuntimeClock(Arc::clone(&self.clock)),
        )
        .map_err(|error| Error::Unknown(unknown(error)))?;
        drop(inbox);
        if !orphans.is_empty() {
            self.hub.invalidate_session_because(
                &session,
                "lost tool runs reconciled; reload durable history",
            );
        }
        decode(json!({"data":{"sessionID":session.as_str(),"reconciled":orphans.len()}}))
            .map_err(|error| Error::Unknown(unknown(error)))
    }
}
