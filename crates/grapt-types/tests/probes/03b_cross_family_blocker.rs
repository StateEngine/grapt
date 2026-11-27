// Probe 3b: a blocker state cannot mix one family's reply with another
// family's suspension reason.
//
// The earlier shape was `BlockerState<Reply, Reason = SuspensionReason>`: two
// independently substitutable parameters with a default. That made
// `BlockerState<FormAnswer, PermissionSuspensionReason>` — a Form suspended for
// a Permission's reason — a perfectly legal type, and it made the shared
// default apply silently whenever a caller wrote the one-parameter form.
//
// The state now takes ONE sealed family that chooses both payloads together,
// so the mixed pair has nowhere to be written.
use grapt_types::{BlockerState, FormAnswer, PermissionSuspensionReason};

fn main() {
    let _mixed: BlockerState<FormAnswer, PermissionSuspensionReason> = BlockerState::Issued;
}
