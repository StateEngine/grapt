//! FI: atomic fresh creation, fault rollback, and process-crash witnesses.
use super::*;
use grapt_types::{IdToken, Timestamp};
use rusqlite::Connection;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const FIXED_TIME: i64 = 1_786_060_800_123;

#[derive(Debug)]
struct Authorities;
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(FIXED_TIME)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        let Some(id) = IdToken::new("fresh-init-fixed") else {
            panic!("fixture identity is invalid")
        };
        id
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!("grapt-fi-{label}-{}", std::process::id()));
        if path.exists() {
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

// Always reap the witness process, including a failed evidence assertion.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _kill = self.0.kill();
        let _reap = self.0.wait();
    }
}

enum Driver {
    Fault,
    Populate,
    Swap(OpenHook),
    Crash(OpenHook, PathBuf),
}
impl MigrationDriver for Driver {
    fn migrate(
        &self,
        path: &Path,
        plan: &MigrationPlan,
        handle: &DatabaseFileHandle,
        version: &str,
        clock: &dyn ClockAuthority,
    ) -> Result<MigrationOutcome, MigrationError> {
        ProductionMigrationDriver.migrate(path, plan, handle, version, clock)
    }
    fn hook(&self, point: OpenHook, path: &Path) -> Result<(), MigrationError> {
        match self {
            Self::Fault if point == OpenHook::FreshMigrationRecorded(1) => {
                return Err(MigrationError::Interrupted("fresh-after-second-up"))
            }
            Self::Populate if point == OpenHook::FreshCreated => {
                Connection::open(path)?.execute_batch("CREATE TABLE stranger (n INTEGER); INSERT INTO stranger VALUES (7); PRAGMA user_version=7")?;
            }
            Self::Swap(cut) if point == *cut => {
                std::fs::rename(path, path.with_extension("held"))?;
                std::fs::write(path, b"FI_REPLACEMENT_MUST_NOT_BE_WRITTEN")?;
            }
            // A later identity refusal and rollback cannot prove that the
            // post-open check prevented schema work from starting.
            Self::Swap(OpenHook::FreshSqliteOpened)
                if matches!(point, OpenHook::FreshMigrationRecorded(_)) =>
            {
                return Err(MigrationError::Interrupted(
                    "fresh-schema-started-after-open-swap",
                ));
            }
            Self::Crash(cut, marker) if point == *cut => {
                publish_crash_marker(marker, |path| std::fs::write(path, b"FI_CUT_REACHED"))?;
                for _ in 0..1000 {
                    std::thread::sleep(Duration::from_millis(25));
                }
                return Err(MigrationError::Interrupted(
                    "fresh-crash-parent-did-not-kill",
                ));
            }
            _ => {}
        }
        Ok(())
    }
}

fn publish_crash_marker(
    marker: &Path,
    write: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    // The parent may read as soon as the marker pathname exists.
    let pending = marker.with_extension("pending");
    write(&pending)?;
    std::fs::rename(&pending, marker)
}

#[test]
fn fi_crash_marker_is_absent_until_its_payload_is_complete() -> TestResult {
    use std::io::Write;
    use std::sync::mpsc::sync_channel;

    let scratch = Scratch::new("marker-publication")?;
    let marker = scratch.0.join("cut-reached");
    let destination = marker.clone();
    let (opened, observe) = sync_channel(1);
    let (release, finish) = sync_channel(1);
    let publisher = std::thread::spawn(move || {
        publish_crash_marker(&destination, |path| {
            let mut file = std::fs::File::create(path)?;
            file.write_all(b"FI_")?;
            opened.send(()).map_err(std::io::Error::other)?;
            finish
                .recv_timeout(Duration::from_secs(5))
                .map_err(std::io::Error::other)?;
            file.write_all(b"CUT_REACHED")
        })
    });
    let opened = observe.recv_timeout(Duration::from_secs(5));
    let premature = std::fs::read(&marker);
    let released = release.send(());
    let published = publisher.join().map_err(|_| "marker publisher panicked")?;
    opened?;
    released?;
    published?;
    assert!(
        matches!(&premature, Err(error) if error.kind() == std::io::ErrorKind::NotFound),
        "FI: parent observed the marker before its payload was complete: {premature:?}"
    );
    assert_eq!(std::fs::read(marker)?, b"FI_CUT_REACHED");
    Ok(())
}

fn initialize(
    path: &Path,
    driver: &dyn MigrationDriver,
) -> Result<OwnedEventStore, OwnedOpenError> {
    EventStore::open_owned_with_driver(
        path,
        "fi-witness",
        OwnedOpenAuthority::Initialize,
        &Authorities,
        &Authorities,
        driver,
    )
}

fn assert_uncommitted(path: &Path) -> TestResult {
    // A writable connection performs SQLite's hot-journal recovery. Its
    // recovered state must contain none of the atomic initialization facts.
    let conn = Connection::open(path)?;
    let epoch: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let objects: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))?;
    assert_eq!(
        epoch, 0,
        "FI: ownership escaped an uncommitted initialization"
    );
    assert_eq!(
        objects, 0,
        "FI: schema or migration journal escaped rollback"
    );
    Ok(())
}

fn assert_complete(path: &Path) -> TestResult {
    assert!(matches!(preflight_read_only(path), StoreReadiness::Ready));
    let conn = Connection::open(path)?;
    let epoch: i64 = conn.query_row("SELECT time FROM engine_epoch", [], |r| r.get(0))?;
    assert_eq!(epoch, FIXED_TIME);
    let migrations = ProductionMigrations::load()?;
    let mut statement = conn.prepare("SELECT id,time_completed FROM rust_migration ORDER BY id")?;
    let actual = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut expected = migrations
        .as_slice()
        .iter()
        .map(|m| (m.id().as_str().to_owned(), FIXED_TIME))
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(
        actual, expected,
        "FI: all six migration facts must commit together"
    );
    let fk: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
        [],
        |r| r.get(0),
    )?;
    assert!(!fk);
    Ok(())
}

#[test]
fn fi_fault_after_migration_rolls_back_epoch_schema_and_journal() -> TestResult {
    let scratch = Scratch::new("fault")?;
    let path = scratch.0.join("store.db");
    assert!(matches!(
        initialize(&path, &Driver::Fault),
        Err(OwnedOpenError::Migration(MigrationError::Interrupted(
            "fresh-after-second-up"
        )))
    ));
    assert!(path.exists(), "partial profile data must be preserved");
    assert_uncommitted(&path)?;
    assert!(!matches!(preflight_read_only(&path), StoreReadiness::Ready));
    // Existing empty files are not eligible for the no-backup route.
    assert!(matches!(
        initialize(&path, &ProductionMigrationDriver),
        Err(OwnedOpenError::QuarantinedOrCorrupt(ref obstruction))
        if matches!(obstruction.as_ref(), Obstruction::Integrity(detail)
            if detail == "the durable event table is missing")
    ));
    assert_uncommitted(&path)?;
    Ok(())
}

#[test]
fn fi_path_swap_before_open_and_commit_refuses_and_preserves_stranger() -> TestResult {
    for (label, cut) in [
        ("created", OpenHook::FreshCreated),
        ("opened", OpenHook::FreshSqliteOpened),
        ("commit", OpenHook::FreshBeforeCommit),
    ] {
        let scratch = Scratch::new(label)?;
        let path = scratch.0.join("store.db");
        let result = initialize(&path, &Driver::Swap(cut));
        #[cfg(unix)]
        {
            assert!(matches!(result, Err(OwnedOpenError::PathIdentityChanged)));
            assert_eq!(std::fs::read(&path)?, b"FI_REPLACEMENT_MUST_NOT_BE_WRITTEN");
            assert_uncommitted(&path.with_extension("held"))?;
        }
        #[cfg(windows)]
        {
            assert!(
                matches!(result, Err(OwnedOpenError::Migration(MigrationError::Backup(ref error)))
                if error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(32))
            );
            assert!(
                !path.with_extension("held").exists(),
                "FI retained handle allowed rename"
            );
            assert_uncommitted(&path)?;
        }
    }
    Ok(())
}

#[test]
fn fi_new_file_populated_by_another_writer_is_never_overwritten() -> TestResult {
    let scratch = Scratch::new("populated")?;
    let path = scratch.0.join("store.db");
    assert!(matches!(
        initialize(&path, &Driver::Populate),
        Err(OwnedOpenError::PostActionNotReady(_))
    ));
    let conn = Connection::open(&path)?;
    let value: i64 = conn.query_row("SELECT n FROM stranger", [], |r| r.get(0))?;
    let epoch: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(value, 7);
    assert_eq!(epoch, 7);
    let objects: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))?;
    assert_eq!(
        objects, 1,
        "FI refused creation changed another writer's schema"
    );
    Ok(())
}

#[test]
#[ignore = "FI subprocess witness; parent supplies exact path and cut"]
fn fi_crash_child() -> TestResult {
    let path = PathBuf::from(std::env::var_os("GRAPT_FI_DB").ok_or("FI child database missing")?);
    let marker =
        PathBuf::from(std::env::var_os("GRAPT_FI_MARKER").ok_or("FI child marker missing")?);
    let cut = match std::env::var("GRAPT_FI_CUT")?.as_str() {
        "before" => OpenHook::FreshBeforeCommit,
        "after" => OpenHook::FreshAfterCommit,
        _ => return Err("FI child cut is invalid".into()),
    };
    let result = initialize(&path, &Driver::Crash(cut, marker));
    panic!("FI crash parent failed to terminate child at the cut: {result:?}");
}

#[test]
fn fi_crash_before_and_after_commit_has_only_empty_or_complete_state() -> TestResult {
    for cut in ["before", "after"] {
        let scratch = Scratch::new(cut)?;
        let path = scratch.0.join("store.db");
        let marker = scratch.0.join("cut-reached");
        let mut child = ChildGuard(
            Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "owned_open::fresh::tests::fi_crash_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("GRAPT_FI_DB", &path)
                .env("GRAPT_FI_MARKER", &marker)
                .env("GRAPT_FI_CUT", cut)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let mut reached = false;
        for _ in 0..1000 {
            if marker.exists() {
                reached = true;
                break;
            }
            if let Some(status) = child.0.try_wait()? {
                panic!("FI child exited before named cut: {status}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(reached, "FI child never produced the required cut witness");
        assert_eq!(std::fs::read(&marker)?, b"FI_CUT_REACHED");
        child.0.kill()?;
        let status = child.0.wait()?;
        assert!(!status.success(), "FI child was not crashed");
        assert!(path.exists(), "crash cleanup deleted partial data");
        if cut == "before" {
            assert_uncommitted(&path)?;
        } else {
            assert_complete(&path)?;
            let ready = EventStore::open_owned_with_authorities(
                &path,
                "fi-witness",
                OwnedOpenAuthority::OpenReady,
                &Authorities,
                &Authorities,
            )?;
            assert!(matches!(ready.receipt(), OwnedOpenReceipt::OpenedReady));
        }
    }
    Ok(())
}

fn schema(path: &Path) -> TestResult<Vec<(String, String, String, String)>> {
    let conn = Connection::open(path)?;
    let mut query = conn.prepare(
        "SELECT type,name,tbl_name,coalesce(sql,'') FROM sqlite_master ORDER BY type,name",
    )?;
    let rows = query
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, String>(3)?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[test]
fn fi_current_schema_indexes_and_foreign_keys_match_existing_migration() -> TestResult {
    let scratch = Scratch::new("parity")?;
    let path = scratch.0.join("fresh.db");
    let opened = initialize(&path, &ProductionMigrationDriver)?;
    assert!(matches!(
        opened.receipt(),
        OwnedOpenReceipt::Initialized(MigrationOutcome::AlreadyCurrent)
    ));
    let fk: i64 = opened
        .store()
        .connection()
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    assert_eq!(fk, 1);
    // Canonical writes still require the governed store after fresh creation.
    assert!(opened
        .store()
        .connection()
        .execute("DELETE FROM event", [])
        .is_err());
    drop(opened);
    assert_complete(&path)?;
    let existing = scratch.0.join("existing.db");
    drop(EventStore::open_with_ids(
        &existing,
        "fi-witness",
        &Authorities,
    )?);
    let migrations = ProductionMigrations::load()?;
    crate::apply_migrations_with_clock(
        &existing,
        &scratch.0,
        "fi-witness",
        &migrations.as_slice(),
        &Authorities,
    )?;
    drop(EventStore::open_owned_with_authorities(
        &existing,
        "fi-witness",
        OwnedOpenAuthority::OpenReady,
        &Authorities,
        &Authorities,
    )?);
    assert_eq!(
        schema(&path)?,
        schema(&existing)?,
        "FI: reuse the current schema and migration owners"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
    }
    Ok(())
}

#[test]
fn a_fresh_owned_store_uses_the_live_wal_policy_before_reopen() -> TestResult {
    let scratch = Scratch::new("wal-policy")?;
    let path = scratch.0.join("store.db");
    let opened = initialize(&path, &Driver::Populate);
    assert!(opened.is_err(), "populated foreign files remain refused");
    let refused = Connection::open(&path)?;
    let mode: String = refused.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    assert_eq!(
        mode, "delete",
        "a refused initialization cannot change journal policy"
    );
    drop(refused);
    let path = scratch.0.join("fresh.db");
    let opened = EventStore::open_owned_with_authorities(
        &path,
        "fresh-wal",
        OwnedOpenAuthority::Initialize,
        &Authorities,
        &Authorities,
    )?;
    let conn = opened.store().connection();
    let journal: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let synchronous: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    assert_eq!(journal, "wal");
    assert_eq!(synchronous, 1);
    Ok(())
}
