//! Acceptance: project all 305 real sessions from the event log and verify
//! field-by-field against the session table — slug, project_id, title,
//! agent, model (JSON object equality), preset, time_created. Seedless
//! aggregates (created event compacted away) are measured and their
//! seed-only fields skipped. Skips loudly when the clone is absent.

use grapt_session::{SessionEvent, SessionProjection};
use serde_json::Value;
#[path = "../../../test-support/evidence.rs"]
mod evidence;

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run with --include-ignored on the evidence machine"]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn projects_all_305_sessions_and_matches_the_table() {
    let path = evidence::frozen_clone();
    let log = match grapt_store::EventLog::open_read_only(&path) {
        Ok(l) => l,
        Err(e) => panic!("open: {e}"),
    };
    let mut projection = SessionProjection::default();
    let mut decode_errors: Vec<String> = Vec::new();
    let walked = log.for_each_in_replay_order(|e| {
        match SessionEvent::decode(e.kind.name(), e.kind.version().value(), &e.data) {
            Ok(Some(event)) => projection.apply(event),
            Ok(None) => {}
            Err(err) => decode_errors.push(format!("{}: {err}", e.id)),
        }
    });
    assert!(walked.is_ok(), "walk failed: {walked:?}");
    assert!(
        decode_errors.is_empty(),
        "strict decode rejected real events: {decode_errors:?}"
    );

    let conn = match rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => panic!("reopen: {e}"),
    };
    let mut stmt = match conn.prepare(
        "SELECT id, slug, project_id, title, agent, model, preset, time_created FROM session",
    ) {
        Ok(s) => s,
        Err(e) => panic!("prepare: {e}"),
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(e) => panic!("query: {e}"),
    };
    let mut checked = 0u64;
    let mut mismatches: Vec<String> = Vec::new();
    while let Some(row) = rows.next().unwrap_or(None) {
        let id: String = row.get(0).unwrap_or_default();
        let slug: Option<String> = row.get(1).unwrap_or(None);
        let project_id: Option<String> = row.get(2).unwrap_or(None);
        let title: Option<String> = row.get(3).unwrap_or(None);
        let agent: Option<String> = row.get(4).unwrap_or(None);
        let model_json: Option<String> = row.get(5).unwrap_or(None);
        let preset: Option<String> = row.get(6).unwrap_or(None);
        let time_created: Option<i64> = row.get(7).unwrap_or(None);

        let projected = projection
            .rows
            .get(&id.parse().unwrap_or_else(|e| panic!("bad id {id}: {e}")))
            .unwrap_or_else(|| panic!("{id}: in table but never projected"));

        let mut diff = |field: &str, table: String, folded: String| {
            if table != folded {
                mismatches.push(format!(
                    "{id}.{field}: table {table:?} vs folded {folded:?}"
                ));
            }
        };
        if projected.seeded {
            diff("slug", format!("{slug:?}"), format!("{:?}", projected.slug));
            diff(
                "project_id",
                format!("{project_id:?}"),
                format!("{:?}", projected.project_id),
            );
            diff(
                "time_created",
                format!("{time_created:?}"),
                format!("{:?}", projected.time_created_ms),
            );
        }
        diff(
            "title",
            format!("{title:?}"),
            format!("{:?}", projected.title),
        );
        diff(
            "agent",
            format!("{agent:?}"),
            format!("{:?}", projected.agent),
        );
        diff(
            "preset",
            format!("{preset:?}"),
            format!("{:?}", projected.preset),
        );

        let table_model: Option<Value> = model_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        if table_model != projected.model {
            mismatches.push(format!(
                "{id}.model: table {table_model:?} vs folded {:?}",
                projected.model
            ));
        }
        checked += 1;
    }
    assert_eq!(checked, 305);
    assert!(
        mismatches.len() <= 12,
        "too many field mismatches ({}): first: {:?}",
        mismatches.len(),
        &mismatches[..mismatches.len().min(6)]
    );
    if mismatches.is_empty() {
        eprintln!(
            "SESSION OK: 305 rows field-by-field, {} seedless (measured)",
            projection.seedless
        );
    } else {
        // Divergences under the threshold are printed for law refinement,
        // not swallowed: they become corrected-expectation rows or projector
        // law fixes on the next pass.
        panic!("session projection near-miss — refine the law: {mismatches:?}");
    }
}
