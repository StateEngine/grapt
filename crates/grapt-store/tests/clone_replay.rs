//! Acceptance: fold the REAL frozen clone's 69,740 events and prove the
//! append+project law against its own projection tables — 305 aggregates,
//! event_sequence parity with exact last-seq equality, session-table parity
//! in both directions. Skips loudly when the evidence drive is absent.

#[path = "../../../test-support/evidence.rs"]
mod evidence;

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run `cargo test -p grapt-store -- --include-ignored` on the evidence machine"]
fn folds_the_frozen_clone_and_proves_append_project_law() {
    let path = evidence::frozen_clone();
    let log = match grapt_store::EventLog::open_read_only(&path) {
        Ok(l) => l,
        Err(e) => panic!("open: {e}"),
    };
    let fold = match grapt_store::replay::ReplayFold::from_log(&log) {
        Ok(f) => f,
        Err(e) => panic!("fold: {e}"),
    };
    assert_eq!(
        fold.aggregates.len(),
        305,
        "clone has 305 session aggregates"
    );

    let conn = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => panic!("reopen: {e}"),
    };
    let report = match grapt_store::replay::verify_against_projections(&fold, &conn) {
        Ok(r) => r,
        Err(e) => panic!("verify: {e}"),
    };
    assert!(
        report.append_project_law_holds(),
        "append+project law violated: seq mismatches {:?}, orphans {:?}, session mismatches {:?}",
        report.sequence_table_mismatches,
        report.orphan_sequence_rows,
        report.session_table_mismatches
    );
    eprintln!(
        "REPLAY OK: {} aggregates, {} not starting with session.created (measured fact)",
        report.aggregates, report.aggregates_not_starting_with_created
    );
}
