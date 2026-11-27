//! An explicit invocation cap survives steer promotions in the same chain.
//!
//! optional nonzero work bounds with an explicit exhausted outcome (Apache-2.0).
use crate::coordinator::CoordinatorError;
use grapt_types::{PolicyVersion, SessionId};
use std::num::NonZeroU32;

/// The policy an opt-in cap's exhaustion names: `--max-steps` on an input or
/// `steps` in the selected agent's definition.
const POLICY: &str = "max-steps-v1";

#[derive(Debug)]
pub(super) struct RunBudget {
    pub(super) spent: u32,
    pub(super) granted: u32,
}

impl RunBudget {
    pub(super) fn new(maximum: NonZeroU32) -> Self {
        Self {
            spent: 0,
            granted: maximum.get(),
        }
    }

    pub(super) fn constrain(&mut self, maximum: NonZeroU32) {
        self.granted = self.granted.min(self.spent.saturating_add(maximum.get()));
    }

    pub(super) fn take(&mut self) -> Result<(), ()> {
        if self.spent == self.granted {
            return Err(());
        }
        self.spent += 1;
        Ok(())
    }

    /// Spend one Step, or end the chain with the cap's typed failure.
    pub(super) fn spend(&mut self, session: &SessionId) -> Result<(), CoordinatorError> {
        if self.take().is_ok() {
            return Ok(());
        }
        Err(match PolicyVersion::new(POLICY) {
            Some(policy) => CoordinatorError::StepBudgetExhausted {
                session: session.clone(),
                policy,
                spent: self.spent,
                granted: self.granted,
            },
            None => CoordinatorError::Run {
                session: session.as_str().to_owned(),
                reason: "the step cap policy name was refused".to_owned(),
            },
        })
    }
}
