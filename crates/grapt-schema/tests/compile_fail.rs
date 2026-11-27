//! Right-reason compile-fail probes with locked `.stderr`.
//!
//! Each probe file must be REJECTED by the compiler, and its recorded stderr is
//! what proves it was rejected for the law it names rather than incidentally.
//! A probe that starts compiling, or that starts failing for a different
//! reason, is a broken law.

#[test]
fn authority_and_category_probes_are_rejected_for_their_stated_reason() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
