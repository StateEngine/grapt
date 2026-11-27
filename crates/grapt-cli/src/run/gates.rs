//! The run's transition guards: the pre-visible retry bound and the
//! terminal/cancel settlement race.

use crate::{CancellationCause, RunError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryGate {
    pub(super) maximum_attempts: u32,
}

impl RetryGate {
    pub fn new(maximum_attempts: u32) -> Result<Self, RunError> {
        if maximum_attempts == 0 || maximum_attempts > crate::bounds::MAX_PRE_VISIBLE_RETRIES {
            return Err(RunError::RetryBound);
        }
        Ok(Self { maximum_attempts })
    }

    pub const fn allows(self, visible: bool, completed_attempts: u32, idempotent: bool) -> bool {
        !visible && idempotent && completed_attempts < self.maximum_attempts
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSettlement {
    Pending,
    Terminal,
    Cancelled(CancellationCause),
}

impl TerminalSettlement {
    pub fn settle_terminal(&mut self) -> Result<(), RunError> {
        if !matches!(self, Self::Pending) {
            return Err(RunError::CancellationRace);
        }
        *self = Self::Terminal;
        Ok(())
    }

    pub fn settle_cancel(&mut self, cause: CancellationCause) -> Result<(), RunError> {
        if !matches!(self, Self::Pending) {
            return Err(RunError::CancellationRace);
        }
        *self = Self::Cancelled(cause);
        Ok(())
    }
}
