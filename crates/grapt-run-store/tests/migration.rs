//! Opening applies a build's migration ledger: a new file is created, an
//! existing one is backed up and verified before it changes, and a newer,
//! changed or failing ledger leaves the database as it was.
use grapt_run_store::{LedgerError, Migrated, MigrationSpec, OpenError, RunStore};
use rusqlite::Connection;
use std::error::Error;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const NOTES: MigrationSpec = MigrationSpec::new(
    1,
    "notes",
    "CREATE TABLE note(id INTEGER PRIMARY KEY, body TEXT NOT NULL);
     INSERT INTO note VALUES (1, 'kept');",
);
const TAGS: MigrationSpec = MigrationSpec::new(2, "tags", "CREATE TABLE tag(id INTEGER);");
const LINKS: MigrationSpec = MigrationSpec::new(3, "links", "CREATE TABLE link(id INTEGER);");
const V1: &[MigrationSpec] = &[NOTES];
const V2: &[MigrationSpec] = &[NOTES, TAGS];
const V3: &[MigrationSpec] = &[NOTES, TAGS, LINKS];

/// Schema version, table names and note bodies, read outside the writer.
fn contents(path: &Path) -> TestResult<(i64, Vec<String>, Vec<String>)> {
    let db = Connection::open(path)?;
    let version = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let column = |sql: &str| -> rusqlite::Result<Vec<String>> {
        db.prepare(sql)?.query_map([], |row| row.get(0))?.collect()
    };
    let tables = column("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")?;
    Ok((
        version,
        tables,
        column("SELECT body FROM note ORDER BY id")?,
    ))
}

async fn open(path: &Path, ledger: &'static [MigrationSpec]) -> Result<Migrated, OpenError> {
    let store = RunStore::open(path, ledger).await?;
    let migrated = store.migrated().clone();
    store.shutdown().await?;
    Ok(migrated)
}

fn backups(dir: &Path) -> TestResult<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".backup") {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn migrated(from: u32, to: u32, backup: Option<PathBuf>) -> Migrated {
    Migrated { from, to, backup }
}

#[tokio::test]
async fn a_new_store_is_created_and_reopening_it_changes_nothing() -> TestResult {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    assert_eq!(open(&path, V2).await?, migrated(0, 2, None));
    assert_eq!(open(&path, V2).await?, migrated(2, 2, None));
    assert_eq!(contents(&path)?.0, 2);
    assert!(
        backups(dir.path())?.is_empty(),
        "a new or current store needs no backup"
    );
    Ok(())
}

#[tokio::test]
async fn an_upgrade_backs_up_the_old_schema_first_and_replaces_a_stale_partial_copy() -> TestResult
{
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V1).await?;
    let partial = dir.path().join("run.db.backup-partial");
    std::fs::write(&partial, b"left by a crash")?;
    let backup = std::fs::canonicalize(dir.path())?.join("run.db.v1.backup");
    assert_eq!(open(&path, V3).await?, migrated(1, 3, Some(backup.clone())));
    assert!(!partial.exists());
    let (version, tables, notes) = contents(&backup)?;
    assert_eq!((version, notes), (1, vec!["kept".to_owned()]));
    assert!(!tables.contains(&"tag".to_owned()));
    let (version, tables, notes) = contents(&path)?;
    assert_eq!((version, notes), (3, vec!["kept".to_owned()]));
    assert!(tables.contains(&"tag".to_owned()) && tables.contains(&"link".to_owned()));
    Ok(())
}

#[tokio::test]
async fn a_newer_or_changed_ledger_is_refused_without_touching_the_file() -> TestResult {
    const CHANGED: &[MigrationSpec] = &[
        NOTES,
        MigrationSpec::new(2, "tags", "CREATE TABLE tag(id TEXT);"),
    ];
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V2).await?;
    let before = std::fs::read(&path)?;
    assert!(matches!(
        open(&path, V1).await,
        Err(OpenError::FutureSchemaVersion {
            found: 2,
            max_supported: 1
        })
    ));
    assert!(matches!(
        open(&path, CHANGED).await,
        Err(OpenError::Ledger(LedgerError::Changed { version: 2 }))
    ));
    assert_eq!(std::fs::read(&path)?, before);
    assert!(backups(dir.path())?.is_empty());
    Ok(())
}

#[tokio::test]
async fn failing_or_escaping_migration_sql_rolls_back_with_nothing_applied() -> TestResult {
    const BROKEN: &[MigrationSpec] = &[NOTES, MigrationSpec::new(2, "broken", "CREATE TABLE (")];
    const ESCAPES: &[MigrationSpec] = &[
        NOTES,
        MigrationSpec::new(2, "escapes", "CREATE TABLE early(id INTEGER); COMMIT;"),
    ];
    const TAMPERS: &[MigrationSpec] = &[
        NOTES,
        MigrationSpec::new(2, "tampers", "DELETE FROM rust_migration;"),
    ];
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V1).await?;
    let before = contents(&path)?;
    for ledger in [BROKEN, ESCAPES, TAMPERS] {
        assert!(matches!(
            open(&path, ledger).await,
            Err(OpenError::Migration { version: 2, .. })
        ));
        assert_eq!(contents(&path)?, before, "no part of migration 2 stays");
    }
    assert_eq!(
        backups(dir.path())?.len(),
        3,
        "each attempt keeps its own backup"
    );
    Ok(())
}

#[tokio::test]
async fn a_backup_that_cannot_be_written_leaves_the_file_as_found() -> TestResult {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V1).await?;
    // A restored backup is in rollback-journal mode; that must survive too.
    let mode: String =
        Connection::open(&path)?.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    assert_eq!(mode, "delete");
    let before = std::fs::read(&path)?;
    // Something that is not a file holds the partial name; it is not removed.
    let blocker = dir.path().join("run.db.backup-partial");
    std::fs::create_dir(&blocker)?;
    assert!(matches!(
        open(&path, V2).await,
        Err(OpenError::BackupFailed(_))
    ));
    assert_eq!(std::fs::read(&path)?, before);
    assert!(blocker.is_dir() && backups(dir.path())?.is_empty());
    Ok(())
}

#[tokio::test]
async fn another_writer_holding_the_lock_stalls_the_open_without_a_change() -> TestResult {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V1).await?;
    let holder = Connection::open(&path)?;
    holder.execute_batch("BEGIN IMMEDIATE")?;
    assert!(matches!(
        open(&path, V2).await,
        Err(OpenError::StorageStalled(_))
    ));
    holder.execute_batch("ROLLBACK")?;
    drop(holder);
    assert_eq!(contents(&path)?.0, 1);
    Ok(())
}

/// A crash mid-transaction in rollback-journal mode leaves a hot journal.
/// Opening rolls it back to the last commit, then migrates as usual.
#[tokio::test]
async fn a_hot_journal_left_by_a_crash_is_rolled_back_and_the_store_opens() -> TestResult {
    let dir = TempDir::new()?;
    let (live, crashed) = (dir.path().join("live.db"), dir.path().join("run.db"));
    open(&live, V1).await?;
    let writer = Connection::open(&live)?;
    writer.execute_batch(
        "PRAGMA journal_mode=DELETE; PRAGMA cache_size=1; BEGIN;
         CREATE TABLE uncommitted(data BLOB);
         WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<64)
             INSERT INTO uncommitted SELECT zeroblob(4096) FROM n;",
    )?;
    // Copying mid-transaction captures what a crash leaves: a changed file
    // and the journal that undoes it, with no process holding a lock.
    std::fs::copy(&live, &crashed)?;
    std::fs::copy(
        dir.path().join("live.db-journal"),
        dir.path().join("run.db-journal"),
    )?;
    writer.execute_batch("ROLLBACK")?;
    assert_eq!(open(&crashed, V2).await?.from, 1);
    let (version, tables, notes) = contents(&crashed)?;
    assert_eq!((version, notes), (2, vec!["kept".to_owned()]));
    assert!(!tables.contains(&"uncommitted".to_owned()));
    Ok(())
}

#[tokio::test]
async fn readers_cannot_see_the_ledger() -> TestResult {
    let dir = TempDir::new()?;
    let store = RunStore::open(dir.path().join("run.db"), V1).await?;
    let deadline = || tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let readers = store.readers();
    let read =
        |sql: &'static str| readers.snapshot(deadline(), move |view| view.prepare(sql).map(drop));
    read("SELECT body FROM note").await?;
    assert!(read("SELECT * FROM rust_migration").await.is_err());
    store.shutdown().await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_backup_is_no_more_readable_than_its_database() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V1).await?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let backup = open(&path, V2).await?.backup.ok_or("no backup")?;
    assert_eq!(
        std::fs::metadata(backup)?.permissions().mode() & 0o777,
        0o600
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_path_backs_up_beside_the_real_file() -> TestResult {
    let dir = TempDir::new()?;
    let (real, links) = (dir.path().join("real"), dir.path().join("links"));
    std::fs::create_dir(&real)?;
    std::fs::create_dir(&links)?;
    let link = links.join("run.db");
    open(&real.join("run.db"), V1).await?;
    std::os::unix::fs::symlink(real.join("run.db"), &link)?;
    let backup = std::fs::canonicalize(&real)?.join("run.db.v1.backup");
    assert_eq!(open(&link, V2).await?, migrated(1, 2, Some(backup)));
    assert!(backups(&links)?.is_empty());
    assert_eq!(contents(&link)?.0, 2);
    Ok(())
}
