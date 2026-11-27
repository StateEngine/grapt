// An unresolved approval can never cross the governed lease/effect entry.
//
// This is the shortest public chain an attacker in a consumer crate has, and
// every link is now broken:
//
//   * an action string is not a `CapabilityId` — the type has no `FromStr` and
//     no public constructor, only `derive` from a `ResolvedCapability`;
//   * `CapabilityGrant` has no public `new`, so a self-derived capability
//     cannot be wrapped in a grant;
//   * the only routes to a grant or a lease consume a
//     `LivePermissionDecision`, whose mint is crate-private to `grapt-types`;
//   * and this crate's own approval record is not a `SessionGrant` and does not
//     produce one.
//
// Attribution note: this probe used to reach `CapabilityGrant::new`, which
// still existed at the time. Now that the constructor is gone the FIRST law the
// compiler reaches is the capability-identity one, so the remaining links are
// witnessed by their own probes in `grapt-types`
// (`31_authored_capability_grant`, `32_mint_a_live_decision`) rather than being
// silently swallowed here.
use grapt_types::CapabilityId;

fn main() {
    // An action string is not a capability.
    let _capability: CapabilityId = "fs.read";
}
