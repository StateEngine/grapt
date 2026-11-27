//! W2 on real rows: the legacy decode path attaches the D1-pinned identity
//! hash byte for byte, via the v1 kernel and never the v2 encoder. Runs the
//! six per-era samples from `DB_EVIDENCE_D1_2026-08-08.md` §4 through the
//! FULL `decode_for_replay` path (the kernel-only half lives in
//! `era_hashes.rs`). Ignored in ordinary runs (visible as `ignored`, never a
//! vacuous pass); under `cargo test -p grapt-event -- --include-ignored`
//! it FAILS when the frozen clone is absent.

use grapt_event::{decode_for_replay, LegacyDataLimit};
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
fn the_decode_path_reproduces_the_six_era_hashes() {
    let path = evidence::frozen_clone();
    let conn = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => panic!("clone exists but cannot open read-only: {e}"),
    };
    for (id, expected_prefix) in ERA_SAMPLES {
        let (aggregate_id, seq, created, kind, data): (String, i64, i64, String, String) =
            match conn.query_row(
                "SELECT aggregate_id, seq, created, type, data FROM event WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            ) {
                Ok(columns) => columns,
                Err(e) => panic!("event {id} missing from clone: {e}"),
            };
        let record = match decode_for_replay(
            id,
            &aggregate_id,
            seq,
            created,
            &kind,
            &data,
            LegacyDataLimit::MEASURED_DEFAULT,
        ) {
            Ok(record) => record,
            Err(e) => panic!("era sample {id} refused to decode: {e}"),
        };
        assert_eq!(
            &record.derived_identity_hash()[..16],
            expected_prefix,
            "era sample {id}: decode-path hash diverged from the D1 pin"
        );
    }
}
