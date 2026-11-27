// Probe 49: an authority snapshot cannot be duplicated.
//
// External on purpose: `grapt-provider` is the consumer that fuses a graph to
// a request, so it is the crate where the law "one immutable receipted
// authority graph per physical attempt" (root correction #4) is actually
// spendable.
//
// The consumer takes a snapshot BY VALUE, and by-value reads as
// single-use. It is not enough on its own: `PhysicalAttemptAuthoritySnapshot`
// derived `Clone`, so a caller built one graph, cloned it, and could spend the
// same resolved authority twice — each copy yielding a receipt that honestly
// described the same graph. Sealing a consumer while its INPUT can be copied at
// will guards the wrong end of the chain, which is why this probe is about the
// snapshot rather than about anything downstream of it.
fn requires_clone<T: Clone>() {}

fn main() {
    requires_clone::<grapt_policy::PhysicalAttemptAuthoritySnapshot>();
}
