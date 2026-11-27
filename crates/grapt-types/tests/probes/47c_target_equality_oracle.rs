// Probe 47c: two `ResolvedModelTarget`s cannot be compared.
//
// Derived equality over the resolved tuple compared the contained `ModelRef`'s
// complete catalog row AND the derived `CapabilityId`, both of which carry
// `base_model` and `family`. That is the guessed-row oracle one level up: once
// the registry lane can issue targets, a caller holding one genuine target and
// able to produce candidates compares them and reads back the sealed routing
// facts one bit at a time.
//
// The two questions a consumer genuinely asks are answered by named predicates
// instead — `describes_same_target` over the inert coordinates, and
// `is_same_issuance` for exact provenance — neither of which can be assembled
// into "may `agent default` cross to this target".
fn requires_equality<T: PartialEq>() {}

fn main() {
    requires_equality::<grapt_types::ResolvedModelTarget>();
}
