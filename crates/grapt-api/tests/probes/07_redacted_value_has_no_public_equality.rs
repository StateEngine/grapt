// Probe 07 (R4H-A, R1 finding H2): redaction exposes no equality oracle.
//
// `Redacted::new` is public because refusals need a typed observation carrier.
// If that carrier implemented `PartialEq`, a holder could construct guesses and
// learn whether each guess matched the hidden material without ever rendering
// it. The absence is asserted through a trait bound so the compiler, rather
// than a runtime convention, owns the seal.
use grapt_api::Redacted;

fn requires_partial_eq<T: PartialEq>() {}

fn main() {
    requires_partial_eq::<Redacted<String>>();
}
