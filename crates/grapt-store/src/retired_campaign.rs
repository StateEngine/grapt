//! Tables kept for databases written by the retired campaign draft feature.
//!
//! Earlier releases could record campaign drafts. Their rows stay in place and
//! nothing reads or writes them any more, but the open path still creates and
//! verifies these tables so every database keeps one shape. The statements are
//! frozen: changing one is a schema migration, not a cleanup.

pub(crate) fn ensure_phase_table(tx: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS campaign_phase (
            campaign_id TEXT PRIMARY KEY,
            phase TEXT NOT NULL
        );",
    )
}

pub(crate) fn ensure_plan_artifact_table(
    tx: &rusqlite::Transaction<'_>,
) -> Result<(), rusqlite::Error> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS campaign_plan_artifact (
            campaign_id TEXT NOT NULL,
            plan_version TEXT NOT NULL,
            artifact_id TEXT NOT NULL,
            artifact_length INTEGER NOT NULL CHECK (artifact_length > 0),
            artifact_digest TEXT NOT NULL,
            graph_digest TEXT NOT NULL,
            versioned_sequence INTEGER NOT NULL CHECK (versioned_sequence >= 0),
            PRIMARY KEY (campaign_id, plan_version)
        );",
    )
}

pub(crate) fn ensure_plan_read_table(
    tx: &rusqlite::Transaction<'_>,
) -> Result<(), rusqlite::Error> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS plan_read_receipt (
            receipt_id TEXT PRIMARY KEY,
            campaign_id TEXT NOT NULL,
            recorded INTEGER NOT NULL,
            record_bytes BLOB NOT NULL
        );",
    )
}

/// Runs only after the open path has verified the retained tables' columns:
/// on a wrong-shaped namesake an index would otherwise be refused first, and
/// the failure would name the index instead of the missing column.
pub(crate) fn ensure_indexes(tx: &rusqlite::Transaction<'_>) -> Result<(), rusqlite::Error> {
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS plan_read_receipt_campaign
            ON plan_read_receipt (campaign_id, receipt_id);",
    )?;
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS campaign_plan_artifact_identity
         ON campaign_plan_artifact (campaign_id, plan_version);",
    )
}
