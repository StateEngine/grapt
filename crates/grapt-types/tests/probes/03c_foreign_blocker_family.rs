// Probe 3c: a downstream type cannot declare itself a blocker family.
//
// `BlockerFamily` is sealed, so the canonical Permission and Form families are
// the only ones that exist. Without the seal a consumer could introduce a
// fourth family whose payloads no lifecycle owner exhaustively matches, which
// is exactly the third owner the shared `SuspensionReason` used to be.
use grapt_types::{BlockerFamily, FormAnswer, PermissionSuspensionReason};

struct MyBlocker;

impl BlockerFamily for MyBlocker {
    type Reply = FormAnswer;
    type Reason = PermissionSuspensionReason;
}

fn main() {}
