//! Compile-time proof that cache eligibility is issued only by the prefix
//! identity authority. Registration is globbed so the probe cannot be added
//! without being executed.

#[test]
fn prefix_comparison_authority_cannot_be_forged() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/probes/*.rs");
}
