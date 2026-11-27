// Probe 43b (amendment §8.5): a rescue cannot be asserted by the caller that
// benefits from it.
//
// "Provider-supported rescue occurs before the first destructive write and is
// included in the receipt."
//
// `SettledRestorePlan::apply` takes a `RestoreRescue` by value, so rescue is an
// input to the destructive write rather than a promise made after it. That is
// only worth anything if the value is unforgeable: `RestoreRescue::captured` is
// crate-private and owned by the checkpoint provider adapter, which is the only
// component that actually performed a capture.
//
// The one public route, `RestoreRescue::waived_as_unsupported`, is sealed by its
// input in the same way as the entry itself — it returns `Some` only for a
// settled plan that already records
// `UnusualRestoreCondition::ProviderRescueUnsupported`, so a caller cannot waive
// rescue for a plan whose authority never saw that condition.
//
// Error phase: privacy, resolved before type checking.
use grapt_types::{RecoveryAnchor, RestoreRescue, SettledRestorePlan};

fn claim_a_rescue(plan: &SettledRestorePlan, anchor: &RecoveryAnchor) -> Option<RestoreRescue> {
    RestoreRescue::captured(plan, anchor)
}

fn main() {
    let _ = claim_a_rescue;
}
