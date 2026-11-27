//! Carry each Session's token calibration through its durable log, so a
//! restarted process estimates from it instead of the byte bound.
use super::*;

impl LiveLocation {
    /// Seed the Session's calibration from its log before its first plan.
    pub(super) fn restore_calibration(&self, session: &SessionId, target: &ResolvedModelTarget) {
        self.calibrations.restore(session, target, || {
            let inbox = self
                .inbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            grapt_session::notice::latest_token_calibration(inbox.store().connection(), session)
        });
    }

    /// Record the Session's calibration once its execution's requests settled,
    /// its final measured call folded in first. A failed write costs only the
    /// next process's first estimate.
    pub(super) fn persist_calibration(&self, session: &SessionId) {
        self.calibrations.fold_last(session, |attempt| {
            let inbox = self.inbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match crate::dogfood::continuation::admission::confirmed_usage_in(&inbox, session, attempt) {
                Ok(usage) => usage.input,
                Err(reason) => {
                    crate::diagnostics::report(format!(
                        "session {session}: final call usage is unreadable; calibration keeps its earlier sample: {reason}"
                    ));
                    None
                }
            }
        });
        let written = self.calibrations.persist(session, |calibration| {
            let mut inbox = self
                .inbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            grapt_session::notice::record_token_calibration(
                inbox.store_mut(),
                &authorities,
                session,
                calibration,
            )
            .map(drop)
        });
        if let Err(reason) = written {
            crate::diagnostics::report(format!(
                "session {session}: token calibration was not recorded: {reason}"
            ));
        }
    }
}
