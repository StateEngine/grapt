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
async fn a_failing_migration_rolls_back_and_a_full_disk_is_named() -> TestResult {
    const BROKEN: &[MigrationSpec] = &[NOTES, MigrationSpec::new(2, "broken", "CREATE TABLE (")];
    // SQLite reports a page limit exactly as it reports a full disk.
    const FULL: &[MigrationSpec] = &[
        NOTES,
        MigrationSpec::new(
            2,
            "full",
            "PRAGMA max_page_count = 1; CREATE TABLE big(data BLOB);",
        ),
    ];
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V1).await?;
    let before = contents(&path)?;
    assert!(matches!(
        open(&path, BROKEN).await,
        Err(OpenError::Migration { version: 2, .. })
    ));
    assert!(matches!(
        open(&path, FULL).await,
        Err(OpenError::DiskFull(_))
    ));
    assert_eq!(contents(&path)?, before);
    assert_eq!(
        backups(dir.path())?,
        ["run.db.v1-1.backup", "run.db.v1.backup"],
        "each attempt keeps its own backup; none is replaced"
    );
    Ok(())
}

#[tokio::test]
async fn a_backup_that_cannot_be_written_stops_the_migration() -> TestResult {
    let dir = TempDir::new()?;
    let path = dir.path().join("run.db");
    open(&path, V1).await?;
    let before = contents(&path)?;
    // Something that is not a file holds the partial name; it is not removed.
    let blocker = dir.path().join("run.db.backup-partial");
    std::fs::create_dir(&blocker)?;
    assert!(matches!(
        open(&path, V2).await,
        Err(OpenError::BackupFailed(_))
    ));
    assert_eq!(contents(&path)?, before);
    assert!(blocker.is_dir() && backups(dir.path())?.is_empty());
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
