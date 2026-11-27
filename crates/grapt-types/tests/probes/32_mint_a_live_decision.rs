// Probe 32: live authority cannot be minted from outside the crate at all.
//
// `SessionGrant::from_live`, `CapabilityGrant::from_live` and
// `AuthorityLease::once_from_live` are public routes sealed by their INPUT: they
// each require a `&LivePermissionDecision`, and its mint is crate-private. There
// is no public constructor, no `Clone`, no deserialization and no public
// function anywhere in the crate that returns one, so a downstream caller
// cannot obtain the value those routes demand.
//
// This is the honest boundary. A types crate has no authentication primitive —
// nothing in it can tell the real answering transport apart from any caller in
// the same process — so F1A ships the sealed seam and names the permission/policy
// application owner as the later lane that will hold the live route.
use grapt_types::LivePermissionDecision;

fn main() {
    let _decision = LivePermissionDecision::mint(todo!(), todo!());
}
