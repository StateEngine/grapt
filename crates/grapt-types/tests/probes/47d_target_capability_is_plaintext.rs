// Probe 47d: the capability derived from a resolved target cannot be read out.
//
// This is the leak the accessor audit missed twice, and it is the worst of the
// set because it is not an oracle at all. A `ModelTargetCapabilityId` is NOT
// a digest — `canonical_target_form` builds
//
//     model-target|provider=…|base=…|family=…|variant_present=…|route=…|…
//
// and `as_str` hands that string back unchanged — `Display` is absent on this
// type for the same reason. A public `capability()` published `base_model` and
// `family` in plaintext, on a type whose whole point is that neither has a
// public read path. Deleting the `base_model()`/`family()` accessors while
// leaving this one was sealing the front door beside an open window.
//
// The target is taken as a FUNCTION ARGUMENT. Building one in-probe is
// impossible — `ResolvedModelTarget::resolve` is crate-private — and attempting
// it would fail on the mint and witness the wrong law entirely.
use grapt_types::{ModelTargetCapabilityId, ResolvedModelTarget};

fn leak(target: &ResolvedModelTarget) -> &ModelTargetCapabilityId {
    target.capability()
}

fn main() {
    let _ = leak;
}
