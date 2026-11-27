//! Compile-time probes for the `remote-seat-mail-transport/v1` port.
//!
//! The compiler must REJECT each probe file; a probe that starts compiling is a
//! broken law. Following the `grapt-types` harness convention, probes are split
//! one **error phase** per file — rustc stops after a name-resolution or privacy
//! failure and never reaches trait selection, so two laws in one file silently
//! witness only the first.
//!
//! | Probe | Law |
//! | --- | --- |
//! | `01_delivery_ack_is_not_a_read_claim` | delivery acknowledgement cannot express, set or read attention. This is the structural half of the law that ruling J1's "two doors, one floor" depends on: `msg_ack` means READ, and a transport that could reach a read accessor from a delivery ack could settle attention nobody paid. There is deliberately no runtime test for this, because the code that would witness it does not compile |
//! | `02_binding_is_not_clonable` | a `MailBinding` is a private handle with no second copy. Ruling J2 removed leases and fences, so nothing REJECTS a stale handle's use — which makes it more important, not less, that a handle cannot be duplicated and stashed past its close. `close` takes the binding by value; a `Clone` would defeat that outright |
//! | `03_binding_cannot_be_constructed_externally` | a `MailBinding` cannot be FORGED. It is this crate's proof that an authenticated open happened, and R3b-2B/2C read `receipt().verified_principal()` as authority. The R1 review compiled this exact shape against the previous candidate and it passed, minting a binding for an arbitrary principal with no adapter at all |
//! | `04_port_cannot_be_implemented_externally` | the port is SEALED. Sealing construction alone would leave the other door open: a downstream crate could implement the trait and return a binding from its own `open`. Under J7 the deterministic fake is the only v1 implementation |
//!
//! Probes 03 and 04 are two files because they are two laws AND two error
//! phases — privacy, then trait selection. Closing either alone leaves a route
//! to a forged binding, so neither is redundant.
//!
//! Note what probe 04 had to do to be honest: the one-line
//! `impl RemoteSeatMailTransportV1 for ForeignAdapter {}` fails with E0046
//! (missing items) BEFORE it ever reaches the seal, and E0046 fires just as
//! loudly with the seal deleted — a probe indistinguishable from the unsealed
//! case. It therefore implements all seven methods, leaving the unsatisfiable
//! `Sealed` bound as the only possible error.
//!
//! The behavioural halves live in the crate's unit tests: the refusal variant
//! `DeliveryAckFailure::ReadAckSubstitutionRefused` covers a provider that
//! OFFERS its read endpoint, and the takeover receipts cover displacement.

// One test per probe, not one test for the glob.
//
// A single test over `tests/probes/*.rs` gives every probe the same test
// identity, and trybuild's panic says only `N of M tests failed` without naming
// which probe failed. Two governed mutation rows pointed here — one for external
// construction, one for the port seal — and their evidence was then identical
// but for the process id, so two laws shared one record and the runner's
// duplicate-evidence guard could not see it. Splitting the test gives each law
// its own test name, which is the part of the evidence key that actually
// distinguishes them.
//
// These stay in ONE test binary on purpose: the governed ten-step gate selects
// `--test compile_fail`, so a new target would silently drop its probe out of
// that step even though `workspace-test` still ran it.

#[test]
fn a_delivery_ack_cannot_reach_a_read_claim() {
    trybuild::TestCases::new().compile_fail("tests/probes/01_delivery_ack_is_not_a_read_claim.rs");
}

#[test]
fn a_binding_cannot_be_cloned() {
    trybuild::TestCases::new().compile_fail("tests/probes/02_binding_is_not_clonable.rs");
}

#[test]
fn a_binding_cannot_be_constructed_outside_this_crate() {
    trybuild::TestCases::new()
        .compile_fail("tests/probes/03_binding_cannot_be_constructed_externally.rs");
}

#[test]
fn the_transport_port_cannot_be_implemented_outside_this_crate() {
    trybuild::TestCases::new()
        .compile_fail("tests/probes/04_port_cannot_be_implemented_externally.rs");
}
