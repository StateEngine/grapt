//! An open of an already-current store commits
//! nothing, and a crash inside that open's preparation still recovers.
//!
//! A long-lived read-only connection's data_version detects writes by the
//! opened connection in either journal mode, including checkpointed WAL writes.
//! The main-header counter is used only by the explicit rollback-journal tear
//! witness below, where the one subsequent WAL-mode transition is named.

use grapt_store::{
    preflight_read_only, EventStore, OwnedOpenAuthority, OwnedOpenReceipt, StoreReadiness,
};
use grapt_types::{ClockAuthority, IdAuthority, IdToken, Timestamp};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ENGINE_VERSION: &str = "0.1.0";
const SLEEP_SECONDS: u64 = 120;
const TORN_ROWS: usize = 400;

#[derive(Debug)]
struct FixedAuthorities;

impl ClockAuthority for FixedAuthorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_786_060_800_123)
    }
}

impl IdAuthority for FixedAuthorities {
    fn mint(&self) -> IdToken {
        let Some(token) = IdToken::new("startup-sync") else {
            panic!("the fixed startup-sync identity must be non-empty");
        };
        token
    }
}

fn scratch(label: &str) -> TestResult<PathBuf> {
    let root =
        std::env::temp_dir().join(format!("grapt-startup-sync-{label}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

fn ready_store(root: &Path) -> TestResult<PathBuf> {
    let database = root.join("store.db");
    drop(EventStore::open_owned_with_authorities(
        &database,
        ENGINE_VERSION,
        OwnedOpenAuthority::Initialize,
        &FixedAuthorities,
        &FixedAuthorities,
    )?);
    assert!(matches!(
        preflight_read_only(&database),
        StoreReadiness::Ready
    ));
    Ok(database)
}

/// The database header's file change counter.
fn change_counter(database: &Path) -> TestResult<u32> {
    let bytes = std::fs::read(database)?;
    let field: [u8; 4] = bytes
        .get(24..28)
        .ok_or("the database is shorter than its header")?
        .try_into()?;
    Ok(u32::from_be_bytes(field))
}

fn data_version(observer: &rusqlite::Connection) -> TestResult<i64> {
    Ok(observer.pragma_query_value(None, "data_version", |row| row.get(0))?)
}
fn observe(database: &Path) -> TestResult<rusqlite::Connection> {
    Ok(rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

#[test]
fn an_already_current_open_commits_nothing() -> TestResult {
    let root = scratch("current")?;
    let database = ready_store(&root)?;
    let observer = observe(&database)?;
    let before = data_version(&observer)?;
    for _open in 0..3 {
        drop(EventStore::open(&database, ENGINE_VERSION)?);
    }
    assert_eq!(
        data_version(&observer)?,
        before,
        "PSF101 an already-current open committed (and synced) a transaction with no durable effect"
    );
    drop(observer);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn a_fresh_open_still_commits_its_schema() -> TestResult {
    let root = scratch("fresh")?;
    let database = root.join("store.db");
    drop(EventStore::open(&database, ENGINE_VERSION)?);
    let reopened = rusqlite::Connection::open(&database)?;
    let tables: i64 = reopened.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('event', 'engine_epoch')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(tables, 2, "the first open's schema did not persist");
    drop(reopened);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn an_open_that_repairs_its_schema_commits_the_repair() -> TestResult {
    let root = scratch("repair")?;
    let database = ready_store(&root)?;
    rusqlite::Connection::open(&database)?.execute_batch("DROP INDEX event_type_id_idx")?;
    let observer = observe(&database)?;
    let before = data_version(&observer)?;
    drop(EventStore::open(&database, ENGINE_VERSION)?);
    assert!(
        data_version(&observer)? != before,
        "the schema repair was not committed"
    );
    let restored: i64 = rusqlite::Connection::open(&database)?.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'event_type_id_idx'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(restored, 1, "the repaired index did not persist");
    drop(observer);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// A crash inside an already-current open's preparation, with its probe-shaped
/// rows written and spilled to the database file, leaves a hot journal whose
/// recovery restores the committed state byte for byte.
#[test]
fn a_crash_inside_a_no_op_preparation_recovers_to_the_committed_state() -> TestResult {
    let root = scratch("torn")?;
    let database = ready_store(&root)?;
    // This witness intentionally constructs the legacy rollback-journal mode;
    // ordinary production stores now use WAL.
    rusqlite::Connection::open(&database)?.pragma_update(None, "journal_mode", "DELETE")?;
    let pre_tear = std::fs::read(&database)?;
    let marker = root.join("tear.marker");
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("psf_torn_preparation_writer")
        .env("PSF_TORN_DB", &database)
        .env("PSF_TORN_MARKER", &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut observed = false;
    for _ in 0..1000 {
        if marker.exists() {
            observed = true;
            break;
        }
        if let Some(status) = child.try_wait()? {
            panic!("the torn preparation writer exited before its marker: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !observed {
        let _ = child.kill();
        let _ = child.wait();
        panic!("the torn preparation writer never wrote its marker");
    }
    child.kill()?;
    child.wait()?;
    let journal = root.join("store.db-journal");
    assert!(journal.is_file(), "the kill left no hot journal");
    assert_ne!(
        std::fs::read(&database)?,
        pre_tear,
        "the torn preparation never reached the database file, so this is not a tear"
    );

    let opened = EventStore::open_owned_with_authorities(
        &database,
        ENGINE_VERSION,
        OwnedOpenAuthority::OpenReady,
        &FixedAuthorities,
        &FixedAuthorities,
    )?;
    assert!(
        matches!(
            opened.receipt(),
            OwnedOpenReceipt::RecoveredHotJournal { slow: false }
        ),
        "the hot journal was not recovered"
    );
    drop(opened);
    assert!(!journal.exists(), "recovery left the journal behind");
    assert_eq!(
        change_counter(&database)?,
        u32::from_be_bytes(pre_tear[24..28].try_into()?) + 1,
        "recovery must restore the committed state before the one WAL mode transition"
    );
    let epochs: i64 = rusqlite::Connection::open(&database)?.query_row(
        "SELECT COUNT(*) FROM engine_epoch",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(epochs, 1, "a torn preparation row survived recovery");
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// The child half of the torn witness. A plain suite run carries no tear
/// coordinates and does nothing.
#[test]
fn psf_torn_preparation_writer() {
    let Some(database) = std::env::var_os("PSF_TORN_DB") else {
        return;
    };
    let Some(marker) = std::env::var_os("PSF_TORN_MARKER") else {
        panic!("PSF_TORN_DB is set but PSF_TORN_MARKER is not");
    };
    let mut connection = match rusqlite::Connection::open(&database) {
        Ok(connection) => connection,
        Err(error) => panic!("torn writer open failed: {error}"),
    };
    // A tiny page cache spills the probe-shaped pages into the database file
    // before the crash, which is what makes the journal hot.
    if let Err(error) =
        connection.execute_batch("PRAGMA cache_size = -10; PRAGMA busy_timeout = 10000;")
    {
        panic!("torn writer pragmas failed: {error}");
    }
    let marker = PathBuf::from(marker);
    let outcome = grapt_store::stamp_and_prepare(&mut connection, ENGINE_VERSION, |tx| {
        // The same shape as the schema probe: rows in a savepoint that is
        // never released, on an already-current store. The process is killed
        // here, before the savepoint or the transaction can end.
        tx.execute_batch("SAVEPOINT psf_probe")?;
        for index in 0..TORN_ROWS {
            tx.execute(
                "INSERT INTO engine_epoch (epoch, engine, version, time) VALUES (?1, 'psf-torn', ?2, 0)",
                rusqlite::params![-1 - index as i64, "x".repeat(512)],
            )?;
        }
        if std::fs::write(&marker, "torn\n").is_err() {
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_secs(SLEEP_SECONDS));
        Ok(0)
    });
    // Reaching here means the parent never killed the writer: the tear did
    // not happen, and the run must not pass as one.
    panic!("the torn preparation writer was not killed: {outcome:?}");
}

#[test]
fn the_no_op_observer_detects_a_checkpointed_wal_write() -> TestResult {
    let root = scratch("observer-checkpoint")?;
    let database = ready_store(&root)?;
    let observer = observe(&database)?;
    let before = data_version(&observer)?;
    let header = change_counter(&database)?;
    let mut writer = EventStore::open(&database, ENGINE_VERSION)?;
    writer.elect_writer(&grapt_store::AggregateId::Session("ses_observer".parse()?))?;
    writer
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    assert_eq!(std::fs::metadata(root.join("store.db-wal"))?.len(), 0);
    drop(writer);
    assert_eq!(
        change_counter(&database)?,
        header,
        "this witness must evade the old header probe"
    );
    assert_ne!(
        data_version(&observer)?,
        before,
        "a checkpointed WAL write passed the no-op observer"
    );
    drop(observer);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
