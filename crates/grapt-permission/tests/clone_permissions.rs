//! Acceptance: fold every permission event on the REAL frozen clone and
//! verify the final states row-by-row against `permission_request` — status,
//! reply (stored as JSON `{"reply":"…"}`), and the lingering
//! `suspended_reason` column as historical metadata. Skips loudly when the
//! evidence drive is absent.

use grapt_permission::{PermissionEvent, PermissionFold, PermissionReply};
use grapt_types::BlockerState;
#[path = "../../../test-support/evidence.rs"]
mod evidence;

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run with --include-ignored on the evidence machine"]
fn folds_real_permission_events_and_matches_the_projection_table() {
    let path = evidence::frozen_clone();
    let log = match grapt_store::EventLog::open_read_only(&path) {
        Ok(l) => l,
        Err(e) => panic!("open: {e}"),
    };
    let mut fold = PermissionFold::default();
    let mut decode_errors: Vec<String> = Vec::new();
    let walked = log.for_each_in_replay_order(|e| {
        match PermissionEvent::decode(e.kind.name(), e.kind.version().value(), &e.data) {
            Ok(Some(event)) => fold.apply(event),
            Ok(None) => {}
            Err(err) => decode_errors.push(format!("{}: {err}", e.id)),
        }
    });
    assert!(walked.is_ok(), "walk failed: {walked:?}");
    assert!(
        decode_errors.is_empty(),
        "strict decode rejected real events: {decode_errors:?}"
    );
    assert!(
        fold.violations.is_empty(),
        "lifecycle violations on real data: {:?}",
        fold.violations
    );
    assert!(
        fold.unmatched_references.is_empty(),
        "broken causality on real data: {:?}",
        fold.unmatched_references
    );

    let outcome = fold.outcome();
    assert_eq!(outcome.asked, 43, "clone has 43 permission requests");
    assert_eq!(outcome.settled, 41);
    assert_eq!(outcome.cancelled, 2);
    assert_eq!(outcome.still_suspended, 0);
    assert_eq!(outcome.still_issued, 0);
    assert_eq!(
        outcome.settled_after_suspension, 10,
        "the ten S1-swept rows"
    );

    // Row-by-row against the projection table.
    let conn = match rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => panic!("reopen: {e}"),
    };
    let mut stmt =
        match conn.prepare("SELECT id, status, reply, suspended_reason FROM permission_request") {
            Ok(s) => s,
            Err(e) => panic!("prepare: {e}"),
        };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(e) => panic!("query: {e}"),
    };
    let mut checked = 0u64;
    while let Some(row) = rows.next().unwrap_or(None) {
        let id: String = row.get(0).unwrap_or_default();
        let status: String = row.get(1).unwrap_or_default();
        let reply_json: Option<String> = row.get(2).unwrap_or(None);
        let lingering: Option<String> = row.get(3).unwrap_or(None);

        let record = fold
            .requests
            .get(&id.parse().unwrap_or_else(|e| panic!("bad id {id}: {e}")))
            .unwrap_or_else(|| panic!("{id}: in table but not folded"));

        match (&record.state, status.as_str()) {
            (BlockerState::Settled(reply), "replied") => {
                let expected = format!("{{\"reply\":\"{}\"}}", reply_as_str(*reply));
                assert_eq!(
                    reply_json.as_deref(),
                    Some(expected.as_str()),
                    "{id}: reply column"
                );
            }
            (BlockerState::Cancelled(_), "cancelled") => {}
            (state, s) => panic!("{id}: folded {state:?} but table says {s:?}"),
        }
        let expected_lingering = record.historical_suspension.map(|r| match r {
            grapt_types::PermissionSuspensionReason::ProcessLoss => "process-loss".to_owned(),
            grapt_types::PermissionSuspensionReason::Shutdown => "shutdown".to_owned(),
        });
        // Cancelled rows in the clone carry no lingering reason.
        if status == "replied" {
            assert_eq!(
                lingering, expected_lingering,
                "{id}: suspended_reason column"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 43);
    eprintln!("PERMISSIONS OK: 43 requests, 10 settled-after-suspension, row-by-row match");
}

fn reply_as_str(reply: PermissionReply) -> &'static str {
    grapt_permission::reply_as_str(reply)
}
