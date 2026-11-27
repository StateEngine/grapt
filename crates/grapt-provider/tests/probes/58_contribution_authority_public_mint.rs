// Probe 58: there is no public caller-mint route.
//
// §8.4 as ruled names this separately from construction, and it is separate: a
// public `mint` would hand any holder of the authority an ONLINE oracle — mint a
// candidate, compare with a contributor they already hold — without ever
// constructing an identity from bytes or reading one out. Probes 56 and 57 both
// pass against a crate with a public mint.
//
// The public routes into the mint are the contributor constructors, and each of
// them keeps its result private, which is what probe 61 witnesses.
fn main() {
    let _ = grapt_policy::ContributionAuthority::mint;
}
