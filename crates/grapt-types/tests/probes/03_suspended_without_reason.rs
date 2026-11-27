// Probe 3: `Suspended` cannot exist without its reason, cannot carry a reply,
// and a settled blocker cannot carry a suspension reason instead of its reply.
//
// Both payloads now travel with the sealed family parameter, so each of these
// is a type error against the family's own associated types rather than
// against two independently chosen ones.
use grapt_types::{BlockerState, PermissionBlocker, PermissionReply, PermissionSuspensionReason};

fn takes_blocker(_: BlockerState<PermissionBlocker>) {}

fn main() {
    // Suspended used as a value: it is a constructor needing a reason.
    let bare: BlockerState<PermissionBlocker> = BlockerState::Suspended;
    takes_blocker(bare);

    // Suspended carrying a reply payload instead of a reason.
    takes_blocker(BlockerState::Suspended(PermissionReply::Once));

    // Settled carrying a suspension reason instead of its reply.
    takes_blocker(BlockerState::Settled(PermissionSuspensionReason::Shutdown));
}
