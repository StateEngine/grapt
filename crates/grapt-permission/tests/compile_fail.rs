//! Compile-fail probe (review D): the process-local `SessionGrant` lives in a
//! PRIVATE module of this crate with no public constructor, no `Clone`, and no
//! serialization. External code cannot name, mint, copy, wire, or persist it —
//! the compiler must reject the probe.
#[test]
fn session_grant_is_unreachable_outside_the_policy_boundary() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
