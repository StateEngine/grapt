// Probe 8, first clause: a capability cannot be constructed from an authored
// string. `CapabilityId` has no public constructor and no `FromStr`; deriving
// from a `ResolvedCapability` is the only route.
//
// The lease clause lives in `08b_lease_consumed_once.rs` because this file
// fails during name resolution, which aborts before borrow checking.
use grapt_types::CapabilityId;

fn main() {
    // A model may request a capability but cannot author the stored key.
    let _authored = CapabilityId::new("fs.read");
}
