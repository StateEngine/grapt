//! Owned descriptor opens must remain writable and use the real file's sidecars.
use super::*;
use rusqlite::{Connection, OpenFlags, MAIN_DB};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, std::io::Error> {
        let path =
            std::env::temp_dir().join(format!("grapt-owned-sidecars-{}", std::process::id()));
        if path.try_exists()? {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn owned_sqlite_is_writable_and_sidecars_belong_to_the_real_database() -> TestResult {
    let scratch = Scratch::new()?;
    let database = scratch.0.join("real.db");
    let alias = scratch.0.join("alias.db");
    Connection::open(&database)?.execute_batch("CREATE TABLE witness (n INTEGER)")?;
    std::os::unix::fs::symlink(&database, &alias)?;
    // Ordinary retained identities are read-only handles. SQLite must reopen
    // the physical file read-write, rather than duplicate that access mode.
    let handle = database_file_handle(&alias)?;
    let conn = handle.open_sqlite(&alias, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    assert!(!conn.is_readonly(MAIN_DB)?);
    let actual: String = conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name='main'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(PathBuf::from(actual), database.canonicalize()?);
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE; BEGIN IMMEDIATE; INSERT INTO witness VALUES (1)",
    )?;
    assert!(scratch.0.join("real.db-journal").try_exists()?);
    assert!(!scratch.0.join("alias.db-journal").try_exists()?);
    conn.execute_batch("COMMIT; PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; INSERT INTO witness VALUES (2)")?;
    assert!(scratch.0.join("real.db-wal").try_exists()?);
    assert!(scratch.0.join("real.db-shm").try_exists()?);
    assert!(!scratch.0.join("alias.db-wal").try_exists()?);
    assert!(!scratch.0.join("alias.db-shm").try_exists()?);
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM witness", [], |row| row.get(0))?;
    assert_eq!(count, 2);
    handle.revalidate()?;
    validate_bound_path(&alias, &handle)?;
    Ok(())
}
