//! Acceptance: run the canonical exporter over BOTH state classes on the REAL
//! frozen clone and reproduce the D1 evidence report's row counts
//! (fixtures/rust-reconstruction/DB_EVIDENCE_D1_2026-08-08.md §2/§5).
//! The clone is opened READ-ONLY — the exporter must never stamp or mutate
//! evidence. Skips loudly when the evidence drive is absent.

use rusqlite::{Connection, OpenFlags};
#[path = "../../../test-support/evidence.rs"]
mod evidence;

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run `cargo test -p grapt-store -- --include-ignored` on the evidence machine"]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
fn exports_both_state_classes_and_reproduces_d1_row_counts() {
    let path = evidence::frozen_clone();
    let conn = match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => panic!("read-only open of the frozen clone failed: {e}"),
    };
    let export = match grapt_store::export_canonical(&conn) {
        Ok(e) => e,
        Err(e) => panic!("canonical export failed: {e}"),
    };

    let rows = |class: &[grapt_store::TableDigest], table: &str| {
        class
            .iter()
            .find(|d| d.table == table)
            .unwrap_or_else(|| panic!("digest missing for {table}"))
            .rows
    };

    // Event-replayable class — D1 §2 counts.
    let er = &export.event_replayable;
    assert_eq!(rows(er, "session"), 305);
    assert_eq!(rows(er, "session_message"), 5_844);
    assert_eq!(rows(er, "session_pending"), 2);
    assert_eq!(rows(er, "permission_request"), 43);
    assert_eq!(rows(er, "form_request"), 2);
    assert_eq!(rows(er, "instruction_blob"), 309);
    assert_eq!(rows(er, "instruction_entry"), 82);
    assert_eq!(rows(er, "instruction_state"), 253);

    // Mutable-only class — the state replay can NEVER rebuild (D1 §5).
    let mo = &export.mutable_only;
    assert_eq!(rows(mo, "subagent_operation"), 11);
    assert_eq!(rows(mo, "permission"), 45);
    assert_eq!(rows(mo, "kv"), 1);
    assert_eq!(rows(mo, "credential"), 4);
    assert_eq!(rows(mo, "project"), 3);
    assert_eq!(rows(mo, "project_directory"), 77);
    assert_eq!(rows(mo, "effective_graph"), 22);
    assert_eq!(rows(mo, "workspace"), 0);
    // V1 legacy tables are EMPTY on this database (D1 finding).
    assert_eq!(rows(mo, "message"), 0);
    assert_eq!(rows(mo, "part"), 0);

    // The event log digest covers every event and is non-empty/deterministic.
    assert_eq!(export.event_log.rows, 69_740);
    assert!(!export.event_log.digest_hex.is_empty());
    let again = match grapt_store::export_canonical(&conn) {
        Ok(e) => e,
        Err(e) => panic!("second export failed: {e}"),
    };
    assert_eq!(
        again.event_log.digest_hex, export.event_log.digest_hex,
        "deterministic digest"
    );

    // Evidence hygiene: the read-only open cannot have stamped the clone.
    let user_version: u32 = match conn.query_row("PRAGMA user_version", [], |r| r.get(0)) {
        Ok(v) => v,
        Err(e) => panic!("user_version read failed: {e}"),
    };
    assert_eq!(
        user_version, 0,
        "the frozen clone remains TS-era (never stamped)"
    );
}
