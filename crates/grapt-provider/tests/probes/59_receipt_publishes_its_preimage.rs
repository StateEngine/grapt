// Probe 59: the effective-graph receipt does not publish its own preimage.
//
// `EffectiveGraphReceipt::canonical_bytes` used to return the whole canonical
// stream. That defeated the seal in two independent ways: it was a byte accessor
// for every contribution identity inside it, reachable from a type that refuses
// one directly (probe 57); and it handed a holder the preimage of the digest,
// which is the same act an attacker performs when guessing a value.
//
// The ruling permits the WHOLE `EffectiveGraphId`/digest to stay public once no
// unkeyed secret-derived bytes enter the preimage. `id()` and `digest_hex()`
// therefore remain, and this probe must fail on `canonical_bytes` alone — if it
// ever fails because the receipt itself vanished, it has stopped witnessing this
// law.
fn main() {
    let _ = grapt_policy::EffectiveGraphReceipt::canonical_bytes;
}
