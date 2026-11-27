//! Shared best-effort output for product lifecycle and timing diagnostics.
pub(crate) use grapt_diagnostics::{LossyOutput, Offer};

pub(crate) fn report(message: String) -> (Offer, u64) {
    grapt_diagnostics::report(message)
}
