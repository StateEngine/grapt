//! Compile-time probes for laws a runtime assertion cannot hold.
//!
//! `01_bounds_have_no_default` — no bound type implements `Default`, so a bound
//! is a number somebody decided rather than one nobody did. The runtime witness
//! exercises `try_new(0)` and `try_new(CEILING + 1)` and proves nothing about
//! whether `MaxValueDepth::default()` COMPILES; only the compiler holds that.
//!
//! Unlike most compile-fail seams this one IS scoreable: adding a `Default`
//! derive to the `bound!` macro makes the probe compile, trybuild reports an
//! expected-failure that succeeded, and this test turns red. IGC27 is that row.

#[test]
fn config_probes_are_rejected_by_the_compiler() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
