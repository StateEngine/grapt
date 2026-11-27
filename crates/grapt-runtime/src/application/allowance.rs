use grapt_types::PolicyVersion;

/// How many Steps ONE promoted user input may chain through.
///
/// Checked construction with **no default**. A tool-calling chain is otherwise
/// unbounded, and the oracle's own ceiling is not among the evidence this slice
/// was built from — so the allowance is a required, visible input rather than a
/// number invented here. Exhausting it is a typed failure, never a silent stop
/// that would look like the model finished.
///
/// It is an allowance per *input*, not per chain: the delivery law resets it
/// whenever a new user input is promoted, because the user asking for something
/// else is not the model looping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAllowance {
    policy: PolicyVersion,
    steps: u32,
}

impl StepAllowance {
    pub fn new(steps: u32) -> Option<Self> {
        Self::named(PolicyVersion::new("step-allowance-v1")?, steps)
    }

    pub fn named(policy: PolicyVersion, steps: u32) -> Option<Self> {
        if steps == 0 {
            None
        } else {
            Some(Self { policy, steps })
        }
    }

    pub fn policy(&self) -> &PolicyVersion {
        &self.policy
    }

    pub fn value(&self) -> u32 {
        self.steps
    }
}

/// The allowance as a chain actually spends it.
///
/// The law is "one grant per promoted *batch*", and the batch is a structural
/// boundary rather than a convention: it opens when an input is admitted and
/// closes when a Step is taken against it, so every input promoted between two
/// Steps belongs to exactly one batch by construction.
///
/// Tying the grant to a batch identity — rather than to whether the counter
/// happens to read zero — is what makes the law checkable. Zeroing a counter
/// that already reads zero is indistinguishable from not zeroing it, so a
/// ledger that reset unconditionally could be doing it per steer or per batch
/// and no observation of remaining capacity could tell the two apart.
#[derive(Debug, Clone)]
pub(super) struct AllowanceLedger {
    pub(super) allowance: StepAllowance,
    pub(super) spent: u32,
    /// The batch now being promoted into. Monotonic, and never reused: a `u64`
    /// of Steps is not reachable, so the wrap is a formality rather than a
    /// case.
    batch: u64,
    /// The batch this allowance was granted for.
    granted_for: u64,
    /// How many grants actually happened. Diagnostics, and the observation a
    /// witness needs: it counts the reset itself instead of guessing at it
    /// from capacity.
    pub(super) grants: u32,
}

/// The allowance is exhausted: this chain has spent every Step its current
/// input was allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AllowanceExhausted;

impl AllowanceLedger {
    /// Open a chain. The promotion that opened it IS the first batch's input,
    /// so the allowance is granted here and a steer joining that same batch
    /// before the first Step finds it already granted.
    pub(super) fn new(allowance: StepAllowance) -> Self {
        Self {
            allowance,
            spent: 0,
            batch: 0,
            granted_for: 0,
            grants: 1,
        }
    }

    /// Admit a newly promoted user input into the current batch.
    ///
    /// Grants the allowance once per batch. The second steer of a batch is
    /// refused a grant *because it is the same batch*, not because the grant
    /// would have been a no-op.
    pub(super) fn admit_input(&mut self) {
        if self.granted_for == self.batch {
            return;
        }
        self.spent = 0;
        self.granted_for = self.batch;
        self.grants = self.grants.saturating_add(1);
    }

    pub(super) fn take(&mut self) -> Result<(), AllowanceExhausted> {
        if self.spent >= self.allowance.value() {
            return Err(AllowanceExhausted);
        }
        self.spent += 1;
        // This Step answered the batch, and closes it. Whatever is promoted
        // next is a new user turn rather than more of the turn just run — which
        // is the whole distinction between a user steering and a model looping.
        self.batch = self.batch.wrapping_add(1);
        Ok(())
    }
}
