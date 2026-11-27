// Probe 47: a `ResolvedModelTarget` cannot be printed.
//
// Probe 45 deleted `base_model()`/`family()` from this type, and the round that
// did it audited ACCESSORS — by name. `#[derive(Debug)]` is an accessor too, and
// it was still there, printing the whole resolved tuple including the derived
// `ModelTargetCapabilityId`. That id is not a digest: its canonical form is the
// literal `model-target|provider=…|base=…|family=…|variant_present=…|…`, so one
// `println!("{target:?}")` dumped both sealed catalog facts verbatim. No
// guessing, no oracle, no enumeration — the values themselves.
//
// `Debug` is ABSENT rather than redacted here, deliberately. A redacted impl
// has to be re-audited every time a field is added to an authority proof, and
// this type is exactly where that audit was already missed once. The inert
// endpoint coordinates stay printable through `model_ref()`, whose own `Debug`
// is hand-written and redacts `base_model` and `family`.
fn requires_debug<T: core::fmt::Debug>() {}

fn main() {
    requires_debug::<grapt_types::ResolvedModelTarget>();
}
