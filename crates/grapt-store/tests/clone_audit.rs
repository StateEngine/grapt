//! Acceptance: audit the REAL frozen clone and reproduce D1's numbers with an
//! independent implementation — 69,740 events, all strictly decoded, zero
//! explicit nulls — plus a NEW empiric D1 did not measure: every aggregate's
//! sequence is zero-based and contiguous (the replay-fold precondition).
//! Ignored in ordinary runs (visible as `ignored`, never a vacuous pass);
//! under `cargo test -p grapt-store -- --include-ignored` an absent clone FAILS.

#[path = "../../../test-support/evidence.rs"]
mod evidence;

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run `cargo test -p grapt-store -- --include-ignored` on the evidence machine"]
fn audits_the_frozen_clone_and_reproduces_d1() {
    let path = evidence::frozen_clone();
    let report = match grapt_store::audit(&path) {
        Ok(r) => r,
        Err(e) => panic!("strict audit of the frozen clone failed: {e}"),
    };
    assert_eq!(report.events, 69_740, "D1 counted 69,740 events");
    assert_eq!(
        report.explicit_null_events, 0,
        "D1 found zero explicit nulls"
    );
    assert!(
        report.replay_preconditions_hold(),
        "monotonicity violations: {:?}",
        report.monotonicity_violations
    );
    assert!(report.aggregates > 0 && report.distinct_kinds > 0);
    eprintln!(
        "AUDIT OK: {} events, {} aggregates, {} distinct kind.version; gaps: {} aggregates carry {} missing seq numbers, {} start nonzero",
        report.events,
        report.aggregates,
        report.distinct_kinds,
        report.aggregates_with_gaps,
        report.missing_sequence_numbers,
        report.aggregates_starting_nonzero
    );
}
