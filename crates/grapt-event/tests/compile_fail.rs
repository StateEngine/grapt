//! Trybuild driver. Registration is by glob so no probe file can be
//! silently unregistered.

#[test]
fn envelope_probes_are_rejected_for_their_stated_reason() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/probes/*.rs");
}
