// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: settle the existing generation-bound call lease on drop.
// (InitializeDeadlineGuard, ElicitationPauseGuard), commit

//! Synchronous terminal custody for the adapter's exclusively held session.
use grapt_mcp::session::{CallLease, CallOutcome, McpSession, SessionError};

pub(super) struct CallLeaseGuard<'a> {
    session: &'a mut McpSession,
    lease: Option<CallLease>,
}

impl<'a> CallLeaseGuard<'a> {
    pub(super) fn new(session: &'a mut McpSession, lease: CallLease) -> Self {
        Self {
            session,
            lease: Some(lease),
        }
    }

    pub(super) fn finish(mut self, outcome: CallOutcome) -> Result<(), SessionError> {
        match self.lease.take() {
            Some(lease) => self.session.settle(lease, outcome),
            None => Ok(()),
        }
    }
}

impl Drop for CallLeaseGuard<'_> {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            // Dropping the waiter does not prove that the server did not act.
            // Keep duplicate detection, release only this reservation, and let
            // the existing generation check refuse a retired lease. No task or
            // lock acquisition is needed to complete this synchronous cleanup.
            let _ = self.session.settle(lease, CallOutcome::Disconnected);
        }
    }
}

#[cfg(test)]
#[path = "call_lease_tests.rs"]
mod tests;
