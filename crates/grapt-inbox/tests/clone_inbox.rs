//! Acceptance: fold all 1,772 real admission events and reproduce the
//! `session_pending` table EXACTLY — which independently rediscovers the S4
//! orphaned-steer defect (two admitted inputs never promoted and never
//! disposed) from pure event data. Skips loudly when the clone is absent.

use grapt_inbox::{InboxEvent, InboxFold};
#[path = "../../../test-support/evidence.rs"]
mod evidence;

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run with --include-ignored on the evidence machine"]
fn folds_real_admissions_and_reproduces_session_pending() {
    let path = evidence::frozen_clone();
    let log = match grapt_store::EventLog::open_read_only(&path) {
        Ok(l) => l,
        Err(e) => panic!("open: {e}"),
    };
    let mut fold = InboxFold::default();
    let mut decode_errors: Vec<String> = Vec::new();
    let walked = log.for_each_in_replay_order(|e| {
        match InboxEvent::decode(e.kind.name(), e.kind.version().value(), &e.data) {
            Ok(Some(event)) => fold.apply(event, e.seq.value()),
            Ok(None) => {}
            Err(err) => decode_errors.push(format!("{}: {err}", e.id)),
        }
    });
    assert!(walked.is_ok(), "walk failed: {walked:?}");
    assert!(
        decode_errors.is_empty(),
        "strict decode rejected real events: {decode_errors:?}"
    );
    assert!(fold.violations.is_empty(), "{:?}", fold.violations);
    assert!(
        fold.unmatched_promotions.is_empty(),
        "{:?}",
        fold.unmatched_promotions
    );
    assert_eq!(fold.admitted, 887);
    assert_eq!(fold.promoted, 885);
    assert_eq!(fold.pending.len(), 2, "the two S4 orphaned steers");

    let conn = match rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => panic!("reopen: {e}"),
    };
    let mut stmt =
        match conn.prepare("SELECT id, session_id, delivery, admitted_seq FROM session_pending") {
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
        let session: String = row.get(1).unwrap_or_default();
        let delivery: String = row.get(2).unwrap_or_default();
        let admitted_seq: i64 = row.get(3).unwrap_or(-1);

        let folded = fold
            .pending
            .get(&id.parse().unwrap_or_else(|e| panic!("bad id {id}: {e}")))
            .unwrap_or_else(|| panic!("{id}: in session_pending but not folded pending"));
        assert_eq!(folded.session.as_str(), session, "{id}: session");
        let folded_delivery = match folded.delivery {
            grapt_types::DeliveryMode::Steer => "steer",
            grapt_types::DeliveryMode::Queue => "queue",
        };
        assert_eq!(folded_delivery, delivery, "{id}: delivery");
        assert_eq!(
            folded.admitted_seq,
            grapt_types::AdmissionSeq::new(u64::try_from(admitted_seq).unwrap_or(u64::MAX)),
            "{id}: admitted_seq"
        );
        checked += 1;
    }
    assert_eq!(checked, 2);
    eprintln!("INBOX OK: 887 admitted / 885 promoted; the 2 S4 orphans reproduced exactly");
}
