// Probe 56: a contribution identity cannot be constructed from bytes.
//
// §8.4 as ruled: only NON-CONSTRUCTIBLE opaque contribution identities cross the
// seam. If a caller could wrap bytes of their choosing in a `ContributionId`,
// the keyed mint would be decoration: they would compute an unkeyed digest of a
// candidate value, wrap it, and compare — the exact offline oracle the keyed
// mint exists to remove, reassembled one layer up.
//
// Cross-crate on purpose. Inside `grapt-policy` the tuple constructor is
// reachable and must be; the law is about what a CONSUMER can do.
fn main() {
    let _ = grapt_policy::ContributionId([0u8; 32]);
}
