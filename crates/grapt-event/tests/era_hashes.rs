//! Acceptance test: reproduce the six per-era sample hashes from
//! `DB_EVIDENCE_D1_2026-08-08.md` §4 against the real frozen clone.
//! Ignored in ordinary runs (visible as `ignored` in every summary, never a
//! vacuous pass); under the evidence command — `cargo test -p grapt-event
//! -- --include-ignored` — it FAILS when the clone is absent.

use serde_json::Value;
#[path = "../../../test-support/evidence.rs"]
mod evidence;

const ERA_SAMPLES: [(&str, &str); 6] = [
    ("evt_f64188413001TPhWNnUGx5y0Ac", "aeb7b5f00d47a305"),
    ("evt_f7174464c001Odd696DhpyEyko", "7e141677bb30b338"),
    ("evt_fba3836940016Ovltq9yLIU3cP", "ec47a8962606eeb0"),
    ("evt_fc654578e001Ox7lxaHoTIyFeE", "f7347f08396d7830"),
    ("evt_fcdc8b3c3001AlUu2nHqJ0PGHb", "f4e73135183f4470"),
    ("evt_fde910350001ClEP8FJU2Mh1SU", "96061dfb8d21150f"),
];

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run `cargo test -p grapt-event -- --include-ignored` on the evidence machine"]
fn reproduces_the_six_era_sample_hashes() {
    let path = evidence::frozen_clone();
    let conn = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => panic!("clone exists but cannot open read-only: {e}"),
    };
    for (id, expected_prefix) in ERA_SAMPLES {
        let data: String =
            match conn.query_row("SELECT data FROM event WHERE id = ?1", [id], |row| {
                row.get(0)
            }) {
                Ok(d) => d,
                Err(e) => panic!("event {id} missing from clone: {e}"),
            };
        let value: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => panic!("event {id} data does not parse: {e}"),
        };
        let hash = grapt_event::canonical_hash_hex(&value);
        assert_eq!(
            &hash[..16],
            expected_prefix,
            "era sample {id}: canonical hash diverged from the D1-validated spec"
        );
    }
}
