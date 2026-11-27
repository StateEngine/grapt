// Probe 57: a contribution identity has no byte accessor.
//
// §8.4 as ruled: "no public from-bytes, digest, hex, byte accessor or
// caller-mint route". Probe 56 closes the from-bytes direction; this closes the
// read direction, and they are genuinely different laws — an identity that could
// be read out but not constructed would still let a holder of the graph publish
// the identity of a value, and an identity that could be constructed but not
// read would still let one be forged.
//
// The function is named as a PATH rather than called, because calling it needs a
// value and obtaining a value needs the mint. A path reference fails in name
// resolution alone, so this probe witnesses exactly one law.
fn main() {
    let _ = grapt_policy::ContributionId::as_bytes;
}
