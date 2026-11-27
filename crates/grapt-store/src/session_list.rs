//! Raw persistence and SQLite resource controls for the session-list projection.
//!
//! `grapt-session` owns list meaning. This module owns only the schema that a
//! production migration must create/verify and the deterministic SQLite VM-step
//! guard used by synchronous readers. Keeping this module free of
//! `grapt-session` preserves the ruled dependency direction.

use grapt_types::{LocationRef, Platform, ProjectId, SessionId, WorkspaceId};
use rusqlite::{Connection, Transaction};
use sha2::{Digest, Sha256};
use std::num::NonZeroU64;

pub const SESSION_LIST_PROJECTION: crate::ProjectionId =
    crate::ProjectionId::new("session-list-v1");
pub const MAX_IMPORTED_ROWS: usize = 1_000_000;
pub const IMPORT_BATCH_ROWS: usize = 512;
pub const MAX_ROW_TEXT_BYTES: usize = 262_144;
pub const MAX_SQLITE_VM_STEPS: u64 = 1_000_000;
/// Step allowance the open/restart integrity scan may spend per imported row.
///
/// It is a declared allowance, not a measured constant: it is chosen far above
/// the opcode cost of one row of the joined scan so that every database inside
/// the declared import maximum verifies, while a pathological or adversarial
/// database still stops instead of scanning without end.
pub const OPEN_VERIFY_STEPS_PER_ROW: u64 = 512;
/// Total bound for the open/restart integrity scan.
///
/// The scan moved OFF the per-request path, so it needs a bound of its own —
/// a database is untrusted input, and "bounded on the request path, unbounded
/// at open" would simply relocate the unbounded work rather than remove it.
pub const MAX_OPEN_VERIFY_VM_STEPS: u64 = MAX_IMPORTED_ROWS as u64 * OPEN_VERIFY_STEPS_PER_ROW;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const LEGACY_SOURCE: &str = "typescript-session-v1@42e4888cc0";

include!("session_list/budget.rs");
include!("session_list/schema.rs");
include!("session_list/migration.rs");

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Builds a projection holding exactly the given rows, using the same
    /// column set the import writes.
    fn projection_with(rows: &[(&str, i64)]) -> Result<Connection, Box<dyn std::error::Error>> {
        let mut conn = Connection::open_in_memory()?;
        let tx = conn.transaction()?;
        create_schema(&tx)?;
        for (id, updated_ms) in rows {
            tx.execute(
                "INSERT INTO session_list_projection
                    (session_id, project_id, workspace_id, parent_id, directory, title,
                     updated_ms, provenance_kind, provenance_digest, cutover_seq)
                 VALUES (?1, 'prj_a', NULL, NULL, '/work/a', 'title', ?2,
                         'legacy-baseline', 'digest', -1)",
                rusqlite::params![id, updated_ms],
            )?;
        }
        tx.commit()?;
        Ok(conn)
    }

    /// The projected domain is what the source domain is compared against, so
    /// it must be computed with the SAME encoding — otherwise the comparison
    /// could only ever fail.
    #[test]
    fn the_projected_domain_reproduces_the_source_encoding_exactly() -> TestResult {
        let conn = projection_with(&[("ses_a", 5), ("ses_b", 6)])?;
        let (count, digest) = projected_domain(&conn)?;
        assert_eq!(count, 2);

        let mut hasher = Sha256::new();
        for (id, updated_ms) in [("ses_a", 5i64), ("ses_b", 6i64)] {
            feed_row(
                &mut hasher,
                &LegacyRow {
                    id: id.to_owned(),
                    project_id: "prj_a".to_owned(),
                    workspace_id: None,
                    parent_id: None,
                    directory: "/work/a".to_owned(),
                    title: "title".to_owned(),
                    updated_ms,
                },
            );
        }
        assert_eq!(
            digest,
            format!("{:x}", hasher.finalize()),
            "the projection digest must use the source row encoding"
        );
        Ok(())
    }

    /// A projection holding FEWER rows than the source recorded is an honest
    /// atomic failure. This is the shape a silently-truncating batch reader
    /// produces, and it is the reason the postcondition exists.
    #[test]
    fn a_short_projection_fails_the_import_postcondition() -> TestResult {
        let conn = projection_with(&[("ses_a", 5)])?;
        let (_, digest) = projected_domain(&conn)?;
        let Err(message) = verify_import_postcondition(&conn, 2, &digest) else {
            panic!("a one-row projection cannot satisfy a two-row source domain");
        };
        assert!(
            message.contains("postcondition failed") && message.contains("2"),
            "the failure must name the recorded source domain: {message}"
        );
        Ok(())
    }

    /// Same count, different content. A count-only postcondition would pass
    /// here, so the digest half is proven separately from the count half.
    #[test]
    fn a_content_divergent_projection_fails_the_import_postcondition() -> TestResult {
        let conn = projection_with(&[("ses_a", 5), ("ses_b", 6)])?;
        let (count, _) = projected_domain(&conn)?;
        let divergent = projection_with(&[("ses_a", 5), ("ses_b", 7)])?;
        let (_, divergent_digest) = projected_domain(&divergent)?;
        let verdict = verify_import_postcondition(&conn, count, &divergent_digest);
        assert!(
            verdict.is_err(),
            "an equal-count, unequal-content domain must still fail"
        );
        Ok(())
    }

    /// The control arm: an exactly-matching domain passes. Without this the
    /// two failure witnesses above could both be satisfied by a postcondition
    /// that refuses everything.
    #[test]
    fn an_exact_projection_satisfies_the_import_postcondition() -> TestResult {
        let conn = projection_with(&[("ses_a", 5), ("ses_b", 6)])?;
        let (count, digest) = projected_domain(&conn)?;
        verify_import_postcondition(&conn, count, &digest)?;
        Ok(())
    }
}
