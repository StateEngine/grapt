// Probe 70 (R2A ruling R1): the attempt vocabulary and the Step vocabulary are
// DIFFERENT TYPES, so an attempt's terminal reason cannot be written into a
// Step terminal.
//
// Before R2A both levels shared one `StepFailure { reason: &'static str }`, and
// this substitution was not merely legal, it was invisible: `aborted` written
// by the supervisor and `aborted` written by the driver were the same value in
// the same field. Nothing could distinguish "this physical call was cancelled"
// from "this whole logical Step ended".
//
// This probe carries exactly one error phase. The reason value is a real,
// valid `AttemptTerminalReason`; the only fault is where it is being put.

use grapt_runtime::StepFailure;
use grapt_types::{AttemptTerminalReason, InterruptionSource};

fn main() {
    let attempt_reason = AttemptTerminalReason::Interrupted {
        by: InterruptionSource::User,
    };

    // A Step terminal is being built out of an attempt's reason.
    let _ = StepFailure::new(attempt_reason, "interrupted");
}
