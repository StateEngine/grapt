// Probe 71 (R2A ruling R1): no string can name a terminal reason.
//
// This is a DIFFERENT law from probe 70, and neither subsumes the other. A
// crate that kept two distinct string newtypes — one for attempts, one for
// Steps — would pass 70 and fail here; a crate with one shared enum would pass
// here and fail 70.
//
// The defect this closes is the one that reached production: `application.rs`
// compared a Step's reason against `const ABORTED: &str = "aborted"` and routed
// live control flow on the result. A typo, a rename, or a driver that spelled
// the same event differently would have silently stopped routing interrupts,
// with no compiler objection and no failing test.
//
// One error phase: the detail argument is a perfectly good `&str`, so the only
// fault is the reason.

use grapt_runtime::StepFailure;

fn main() {
    let _ = StepFailure::new("aborted", "interrupted");
}
