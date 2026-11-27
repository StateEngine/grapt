// Probe 61: a contributor does not hand out the identity it was minted with.
//
// The contributor constructors are the only public route into the mint (probe
// 58), so this is the clause that keeps that route from being an oracle anyway:
// `EnvExposure::new(authority, name, guess)` must not yield a value whose
// identity can be read back out and compared against one obtained elsewhere.
//
// The NAME is public and deliberately so — which variables an attempt runs under
// is authority information a reader legitimately needs — so this probe names the
// value field specifically. A probe that failed on `name` too would be witnessing
// "the struct is opaque", which is not the law and is not true.
fn leak(exposure: &grapt_policy::EnvExposure) -> &grapt_policy::ContributionId {
    &exposure.value
}

fn main() {
    let _ = leak;
}
