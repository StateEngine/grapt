//! Which Sessions the coordinator reported as interrupted, recorded under the
//! lock terminal publication takes.

use std::sync::Mutex;

use grapt_types::SessionId;

use super::ExecutionRefusal;

#[derive(Debug, Default)]
pub(super) struct InterruptTruth {
    sessions: Mutex<Vec<SessionId>>,
}

impl InterruptTruth {
    /// Observe the coordinator decision while holding the same lock terminal
    /// publication takes. The ordering matters: cancellation may release the
    /// drain immediately after `interrupt` returns, so recording after this
    /// critical section would let success publication race ahead of the exact
    /// `Interrupted` outcome.
    pub(super) fn observe<F>(
        &self,
        session: &SessionId,
        interrupt: F,
    ) -> Result<grapt_runtime::InterruptOutcome, ExecutionRefusal>
    where
        F: FnOnce() -> Result<grapt_runtime::InterruptOutcome, ExecutionRefusal>,
    {
        let mut sessions = match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let outcome = interrupt()?;
        if matches!(outcome, grapt_runtime::InterruptOutcome::Interrupted)
            && !sessions.contains(session)
        {
            sessions.push(session.clone());
        }
        Ok(outcome)
    }

    pub(super) fn take(&self, session: &SessionId) -> bool {
        let mut sessions = match self.sessions.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions
            .iter()
            .position(|held| held == session)
            .map(|position| sessions.remove(position))
            .is_some()
    }

    #[cfg(test)]
    pub(super) fn take_after_observed_attempt(
        &self,
        session: &SessionId,
        attempted: &std::sync::Barrier,
    ) -> (bool, bool) {
        match self.sessions.try_lock() {
            Ok(mut sessions) => {
                let _ = attempted.wait();
                let took = sessions
                    .iter()
                    .position(|held| held == session)
                    .map(|position| sessions.remove(position))
                    .is_some();
                (false, took)
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                let _ = attempted.wait();
                (true, self.take(session))
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let mut sessions = poisoned.into_inner();
                let _ = attempted.wait();
                let took = sessions
                    .iter()
                    .position(|held| held == session)
                    .map(|position| sessions.remove(position))
                    .is_some();
                (false, took)
            }
        }
    }
}
