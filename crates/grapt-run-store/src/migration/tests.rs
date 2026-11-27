//! A full disk during a migration, raised by SQLite itself on the real open
//! path, is named and leaves the database file as it was.
use crate::sqlite::page_limit::PageLimit;
use crate::{MigrationSpec, OpenError, RunStore};
use tempfile::TempDir;

const NOTES: MigrationSpec = MigrationSpec::new(
    1,
    "notes",
    "CREATE TABLE note(id INTEGER PRIMARY KEY, body TEXT NOT NULL);
     INSERT INTO note VALUES (1, 'kept');",
);
const V1: &[MigrationSpec] = &[NOTES];
const V2: &[MigrationSpec] = &[
    NOTES,
    MigrationSpec::new(2, "grows", "CREATE TABLE grown(id INTEGER);"),
];

#[tokio::test]
async fn a_full_disk_during_migration_is_named_and_changes_nothing(
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    RunStore::open(&path, V1).await?.shutdown().await?;
    let before = std::fs::read(&path)?;
    // A cap below the file's size holds it at its size: the next page is full.
    let result = {
        let _full = PageLimit::set(&path, 1)?;
        RunStore::open(&path, V2).await
    };
    assert!(matches!(result, Err(OpenError::DiskFull(_))), "{result:?}");
    assert_eq!(std::fs::read(&path)?, before);
    Ok(())
}
