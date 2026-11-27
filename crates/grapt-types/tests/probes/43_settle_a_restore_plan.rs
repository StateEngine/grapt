// Probe 43 (row 12, second clause): a settled restore plan cannot be minted
// from outside the crate.
//
// `SettledRestorePlan::apply` is a public route sealed by its INPUT, exactly
// like `SessionGrant::from_live` and `AuthorityLease::once_from_live`: it
// demands a `SettledRestorePlan`, and the only mint is crate-private. Without
// this seal the entry would still take a plan by value — and a caller would
// simply settle a plan of its own choosing, declaring one write of zero bytes
// over any root it liked, and then apply it.
//
// This is the honest boundary. A types crate cannot enumerate a provider's real
// consequences or negotiate its lanes, so F1A ships the sealed seam and names
// the external checkpoint provider / restore-planning owner as the later lane
// that will hold the live route.
//
// Error phase: privacy, resolved before type checking.
use grapt_types::SettledRestorePlan;

fn main() {
    let _plan = SettledRestorePlan::settle(todo!(), todo!());
}
