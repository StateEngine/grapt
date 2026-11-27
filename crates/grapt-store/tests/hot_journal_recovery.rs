//! Witnesses for the hot rollback-journal state: the classification that
//! keeps a torn-but-recoverable database out of the corrupt bucket, the
//! owned-open recovery that rolls the torn transaction back with the
//! committed facts intact, and the
//! counterexamples that prove the recovery door admits only that one
//! state.

use grapt_store::{
    apply_migrations, preflight_read_only, preflight_with_walk_budget, EpochError, EventStore,
    MigrationOutcome, Obstruction, OwnedOpenAuthority, OwnedOpenError, OwnedOpenReceipt,
    ProductionMigrations, StoreReadiness,
};
use grapt_types::{ClockAuthority, IdAuthority, IdToken, Timestamp};
use rusqlite::{Connection, OpenFlags};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ENGINE_VERSION: &str = "0.1.0";

#[derive(Debug)]
struct FixedAuthorities;

impl ClockAuthority for FixedAuthorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_786_060_800_123)
    }
}

impl IdAuthority for FixedAuthorities {
    fn mint(&self) -> IdToken {
        let Some(token) = IdToken::new("hot-journal-integration") else {
            panic!("the fixed hot-journal identity must be non-empty");
        };
        token
    }
}

fn scratch(label: &str) -> TestResult<PathBuf> {
    let name = format!("grapt-hot-journal-{label}-{}", std::process::id());
    let root = std::env::temp_dir().join(name);
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// Opens (and closes) a fresh store, applies the production migrations
/// behind a verified backup, then refuses to proceed unless the
/// classification actually says Ready.
fn ready_store(root: &Path) -> TestResult<PathBuf> {
    let database = root.join("store.db");
    let store = EventStore::open(&database, ENGINE_VERSION)?;
    drop(store);
    let migrations = ProductionMigrations::load()?;
    match apply_migrations(&database, root, ENGINE_VERSION, &migrations.as_slice())? {
        MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent => {}
    }
    // These tests cover recovery of legacy rollback-journal stores.
    Connection::open(&database)?.pragma_update(None, "journal_mode", "DELETE")?;
    match preflight_read_only(&database) {
        StoreReadiness::Ready => Ok(database),
        other => panic!("expected a ready store after a fresh open, found {other:?}"),
    }
}

fn journal_path(database: &Path) -> TestResult<PathBuf> {
    let name = database
        .file_name()
        .ok_or_else(|| std::io::Error::other("database path has no file name"))?;
    Ok(database.with_file_name(format!("{}-journal", name.to_string_lossy())))
}

/// Tears the database: snapshots the pre-tear bytes, re-executes this test
/// binary as the in-graph torn writer, waits (bounded) for its marker, kills
/// the writer, and proves the torn transaction modified the database pages.
/// The kill is the crash; the returned bytes are the recovery ground truth.
fn tear(database: &Path, root: &Path) -> TestResult<Vec<u8>> {
    let pre_tear = std::fs::read(database)?;
    let marker = root.join("tear.marker");
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("crf_torn_writer")
        .env("CRF_TORN_DB", database)
        .env("CRF_TORN_MARKER", &marker)
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
            panic!("the torn-journal writer exited before writing its marker: {status}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    if !observed {
        let _ = child.kill();
        let _ = child.wait();
        panic!("the torn-journal writer never wrote its marker within the bounded wait");
    }
    child.kill()?;
    child.wait()?;
    let journal = journal_path(database)?;
    if !journal.is_file() {
        panic!(
            "the kill did not leave a hot journal at {}",
            journal.display()
        );
    }
    let post_tear = std::fs::read(database)?;
    assert_ne!(
        pre_tear, post_tear,
        "the torn transaction must have modified the database: the pre-tear and post-tear bytes are identical, so the tear is not what the witness claims"
    );
    Ok(pre_tear)
}

fn open_owned(
    database: &Path,
    authority: OwnedOpenAuthority,
    authorities: &FixedAuthorities,
) -> Result<grapt_store::OwnedEventStore, OwnedOpenError> {
    EventStore::open_owned_with_authorities(
        database,
        ENGINE_VERSION,
        authority,
        authorities,
        authorities,
    )
}

#[test]
fn hot_journal_classifies_recoverable_not_corrupt() -> TestResult {
    let root = scratch("classification")?;
    let database = ready_store(&root)?;
    let _pre_tear = tear(&database, &root)?;
    match preflight_read_only(&database) {
        StoreReadiness::HotJournal => {}
        other => panic!("expected HotJournal after the kill, found {other:?}"),
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// The committed facts of a Ready store: table, event and sequence counts,
/// read only. The rollback witness compares the facts before the tear with
/// the facts the recovery leaves behind.
fn committed_facts(database: &Path) -> TestResult<(i64, i64, i64)> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
        [],
        |row| row.get(0),
    )?;
    let events: i64 = connection.query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    let sequences: i64 =
        connection.query_row("SELECT COUNT(*) FROM event_sequence", [], |row| row.get(0))?;
    Ok((tables, events, sequences))
}

#[test]
fn owned_open_ready_rolls_back_and_reopens_intact() -> TestResult {
    let root = scratch("recovery")?;
    let database = ready_store(&root)?;
    let facts_before = committed_facts(&database)?;
    tear(&database, &root)?;
    let authorities = FixedAuthorities;
    let opened = match open_owned(&database, OwnedOpenAuthority::OpenReady, &authorities) {
        Ok(opened) => opened,
        Err(error) => panic!("the hot-journal recovery was refused: {error}"),
    };
    let OwnedOpenReceipt::RecoveredHotJournal { slow: false } = opened.receipt() else {
        panic!(
            "expected the recovered-hot-journal receipt, found {:?}",
            opened.receipt()
        );
    };
    drop(opened);
    let journal = journal_path(&database)?;
    if journal.exists() {
        panic!(
            "the hot journal must be consumed by the rollback, found at {}",
            journal.display()
        );
    }
    match preflight_read_only(&database) {
        StoreReadiness::Ready => {}
        other => panic!("the recovered store must stand Ready, found {other:?}"),
    }
    let facts_after = committed_facts(&database)?;
    assert_eq!(
        facts_before, facts_after,
        "the committed facts must stand untouched by the rollback",
    );
    let torn: i64 = {
        let connection = Connection::open(&database)?;
        connection.query_row(
            "SELECT COUNT(*) FROM event WHERE id LIKE ?",
            ["crf-torn-%"],
            |row| row.get(0),
        )?
    };
    assert_eq!(torn, 0, "the torn rows must not survive the rollback");
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn corrupt_header_stays_integrity_not_hot_journal() -> TestResult {
    let root = scratch("corrupt-header")?;
    let database = ready_store(&root)?;
    let mut bytes = std::fs::read(&database)?;
    // Clobber the SQLite header magic: the file stops being a database
    // entirely, which must land in Integrity, never in the recovery door.
    bytes[0] = 0;
    std::fs::write(&database, &bytes)?;
    match preflight_read_only(&database) {
        StoreReadiness::QuarantinedOrCorrupt(Obstruction::Integrity(_)) => {}
        other => panic!("expected the integrity obstruction, found {other:?}"),
    }
    let authorities = FixedAuthorities;
    match open_owned(&database, OwnedOpenAuthority::OpenReady, &authorities) {
        Err(OwnedOpenError::QuarantinedOrCorrupt(obstruction)) => {
            assert!(
                matches!(&*obstruction, Obstruction::Integrity(_)),
                "the owned open must refuse as corrupt, found {obstruction:?}",
            );
        }
        other => panic!("expected the corrupt refusal from the owned open, found {other:?}"),
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn foreign_epoch_stays_epoch_not_hot_journal() -> TestResult {
    let root = scratch("foreign-epoch")?;
    let database = ready_store(&root)?;
    let connection = Connection::open(&database)?;
    connection.execute_batch("PRAGMA user_version = 7")?;
    drop(connection);
    match preflight_read_only(&database) {
        StoreReadiness::QuarantinedOrCorrupt(Obstruction::Epoch(EpochError::ForeignEpoch {
            found: 7,
        })) => {}
        other => panic!("expected the foreign-epoch obstruction, found {other:?}"),
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn quarantine_marker_stays_quarantine_not_hot_journal() -> TestResult {
    let root = scratch("quarantine")?;
    let database = ready_store(&root)?;
    let connection = Connection::open(&database)?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS rust_migration_run (id INTEGER PRIMARY KEY CHECK (id = 1), started_ms INTEGER NOT NULL, pending TEXT NOT NULL, backup TEXT NOT NULL); INSERT OR REPLACE INTO rust_migration_run (id, started_ms, pending, backup) VALUES (1, 1786060800123, '20260811_rebuild', 'crf-backup-1');"
    )?;
    drop(connection);
    match preflight_read_only(&database) {
        StoreReadiness::QuarantinedOrCorrupt(Obstruction::Quarantine(_)) => {}
        other => panic!("expected the quarantine obstruction, found {other:?}"),
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn torn_over_corrupted_page_refuses_typed_after_recovery() -> TestResult {
    let root = scratch("unrecoverable")?;
    let database = ready_store(&root)?;
    // Zero the mailbox table root page BEFORE the tear. The torn
    // transaction never touches it, so the rollback cannot restore it:
    // the recovery must succeed in rolling back and then refuse, typed,
    // against the database the rollback left.
    let (page, page_size) = {
        let connection = Connection::open(&database)?;
        let page: i64 = connection.query_row(
            "SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = 'mailbox_message'",
            [],
            |row| row.get(0),
        )?;
        let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        (page, page_size)
    };
    let mut bytes = std::fs::read(&database)?;
    let start = ((page - 1) * page_size) as usize;
    let end = start + page_size as usize;
    for byte in bytes
        .get_mut(start..end)
        .ok_or_else(|| std::io::Error::other("the database shrank below the zeroed page"))?
    {
        *byte = 0;
    }
    std::fs::write(&database, bytes)?;
    let _pre_tear = tear(&database, &root)?;
    match preflight_read_only(&database) {
        StoreReadiness::HotJournal => {}
        other => {
            panic!("expected the hot journal to mask the corruption, found {other:?}")
        }
    }
    let authorities = FixedAuthorities;
    match open_owned(&database, OwnedOpenAuthority::OpenReady, &authorities) {
        Err(OwnedOpenError::PostActionNotReady(readiness)) => {
            let StoreReadiness::QuarantinedOrCorrupt(Obstruction::Integrity(detail)) = &*readiness
            else {
                panic!("expected the malformed-database integrity refusal, found {readiness:?}")
            };
            let _ = detail;
        }
        other => {
            panic!("expected the typed post-recovery refusal, found {other:?}")
        }
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn hot_journal_refuses_initialize_authority() -> TestResult {
    let root = scratch("authority")?;
    let database = ready_store(&root)?;
    let _pre_tear = tear(&database, &root)?;
    let authorities = FixedAuthorities;
    match open_owned(&database, OwnedOpenAuthority::Initialize, &authorities) {
        Err(OwnedOpenError::AuthorityMismatch {
            observed,
            authority,
        }) => {
            let StoreReadiness::HotJournal = &*observed else {
                panic!("expected the observed readiness to be HotJournal, found {observed:?}")
            };
            let OwnedOpenAuthority::Initialize = &*authority else {
                panic!("expected the refusing authority to be Initialize, found {authority:?}")
            };
        }
        other => panic!("expected the authority mismatch, found {other:?}"),
    }
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

/// A recovery the database file itself refuses: with the store file
/// read-only the owned open cannot roll the journal back. The refusal must
/// be the typed hot-journal-recovery error and must leave BOTH the database
/// and the journal byte-identical — a failed recovery has still not failed
/// over the data.
#[test]
fn hot_journal_recovery_refused_by_a_read_only_database_preserves_torn_bytes() -> TestResult {
    let root = scratch("recovery-refusal")?;
    let database = ready_store(&root)?;
    let _pre_tear = tear(&database, &root)?;
    let journal = journal_path(&database)?;
    let torn_db = std::fs::read(&database)?;
    let torn_journal = std::fs::read(&journal)?;
    make_read_only(&database)?;
    let authorities = FixedAuthorities;
    match open_owned(&database, OwnedOpenAuthority::OpenReady, &authorities) {
        Err(OwnedOpenError::HotJournalRecovery(source)) => {
            let _ = source;
        }
        other => panic!("expected the typed hot-journal recovery refusal, found {other:?}"),
    }
    assert!(
        journal.is_file(),
        "the refused recovery must leave the hot journal at {}",
        journal.display()
    );
    assert_eq!(
        std::fs::read(&database)?,
        torn_db,
        "the refused recovery must not modify the database bytes"
    );
    assert_eq!(
        std::fs::read(&journal)?,
        torn_journal,
        "the refused recovery must not modify the journal bytes"
    );
    make_writable(&database)?;
    std::fs::remove_dir_all(&root)?;
    Ok(())
}

#[cfg(unix)]
fn make_read_only(path: &Path) -> TestResult<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))?;
    Ok(())
}

#[cfg(unix)]
fn make_writable(path: &Path) -> TestResult<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    Ok(())
}

#[cfg(windows)]
fn make_read_only(path: &Path) -> TestResult<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(windows)]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "Windows-only helper: clears the read-only file attribute; the lint's Unix world-writable concern does not apply to an NTFS attribute"
)]
fn make_writable(path: &Path) -> TestResult<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The in-graph torn writer.
//
// The writer used to live in a second binary under src/bin that the harness
// had to pre-build. It now re-executes THIS test binary as the single test
// `crf_torn_writer`, which cargo builds in the same invocation as its
// parent witness, so the writer can never drift from the witness that
// consumes it.

const AGGREGATE_FAMILY: &str = "crf_torn";
const AGGREGATE_ID: &str = "crf-torn";
const EVENT_TYPE: &str = "session.content.appended.1";
const CREATED_INSTANT: i64 = 1_786_060_800_123;
const WRITER_EPOCH: i64 = 1;
const ROW_COUNT: u32 = 256;
const SLEEP_SECONDS: u64 = 120;

#[test]
fn crf_torn_writer() {
    // A plain suite run carries no tear coordinates and tears nothing. The
    // parent witness re-executes this binary with `--exact` and the pair of
    // environment variables that make this process the writer.
    let Some(database) = std::env::var_os("CRF_TORN_DB") else {
        return;
    };
    let Some(marker) = std::env::var_os("CRF_TORN_MARKER") else {
        panic!("CRF_TORN_DB is set but CRF_TORN_MARKER is not");
    };
    let code = torn_writer_run(Path::new(&database), Path::new(&marker));
    if code != 0 {
        std::process::exit(code);
    }
}

/// Opens the READY database, begins a write transaction, and inserts
/// deterministic `crf-torn-*` rows. In rollback-journal mode the original
/// pages are written to the `-journal` file as they are modified, so the
/// writer proves the journal exists and is non-empty BEFORE it writes the
/// marker file and sleeps. The parent kills the process at the marker: the
/// kill is the crash, and the hot journal is left exactly as a torn writer
/// would leave it. If the parent never kills it, dropping the connection
/// rolls the transaction back cleanly, so an un-killed run can never fake a
/// hot journal.
fn torn_writer_run(database: &Path, marker: &Path) -> i32 {
    let Some(name) = database.file_name() else {
        eprintln!("crf torn writer: the database path has no file name");
        return 1;
    };
    let journal = database.with_file_name(format!("{}-journal", name.to_string_lossy()));

    let mut connection = match rusqlite::Connection::open(database) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("crf torn writer: opening the database was refused: {error}");
            return 1;
        }
    };
    if let Err(error) =
        connection.execute_batch("PRAGMA cache_size = -10; PRAGMA busy_timeout = 10000;")
    {
        eprintln!("crf torn writer: pragma batch was refused: {error}");
        return 1;
    }

    let transaction = match connection.transaction() {
        Ok(transaction) => transaction,
        Err(error) => {
            eprintln!("crf torn writer: the write transaction was refused: {error}");
            return 1;
        }
    };

    // The event table carries a foreign key into event_sequence by
    // (family, aggregate); the parent row must exist before the first
    // torn event row, whatever the connection-level foreign-key setting.
    let sequence_sql =
        "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id) VALUES (?1, ?2, 0, NULL)";
    if let Err(error) = transaction.execute(
        sequence_sql,
        rusqlite::params![AGGREGATE_FAMILY, AGGREGATE_ID],
    ) {
        eprintln!("crf torn writer: the event_sequence row was refused: {error}");
        return 1;
    }

    for index in 0..ROW_COUNT {
        let id = format!("crf-torn-{index:04}");
        let data = torn_row_data(index, &id);
        let event_sql = "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
        if let Err(error) = transaction.execute(
            event_sql,
            rusqlite::params![
                id,
                AGGREGATE_FAMILY,
                AGGREGATE_ID,
                index as i64,
                CREATED_INSTANT,
                EVENT_TYPE,
                data,
                WRITER_EPOCH,
            ],
        ) {
            eprintln!("crf torn writer: insert of {id} was refused: {error}");
            return 1;
        }
    }

    // The journal is hot on disk from the first page modification. Prove
    // the observable fact BEFORE the marker: a zero-byte journal is not a
    // tear the recovery witness can use.
    let journal_size = match std::fs::metadata(&journal) {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            eprintln!("crf torn writer: the rollback journal was not observable yet: {error}");
            return 1;
        }
    };
    if journal_size == 0 {
        eprintln!("crf torn writer: the rollback journal is empty; the tear is not observable");
        return 1;
    }

    if let Err(error) = std::fs::write(marker, format!("journal={journal_size}\n")) {
        eprintln!("crf torn writer: the marker write was refused: {error}");
        return 1;
    }

    // Sleep with the transaction OPEN. A kill inside the window leaves the
    // journal hot; surviving the window drops the transaction, which rolls
    // back and removes the journal, so only a kill can produce the state
    // the recovery witness consumes.
    std::thread::sleep(std::time::Duration::from_secs(SLEEP_SECONDS));
    drop(transaction);
    0
}

/// One torn row: a 4096-character deterministic filler derived from the
/// row index only, wrapped in the session content append envelope.
fn torn_row_data(index: u32, id: &str) -> String {
    let mut filler = String::with_capacity(4096);
    for block in 0..256u32 {
        let value = u64::from(index)
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add(u64::from(block));
        filler.push_str(&format!("{value:016x}"));
    }
    format!(
        "{{\"message\":\"{id}\",\"item\":{{\"ordinal\":{index},\"kind\":\"crf-torn\",\"payload\":{{\"id\":\"{id}\",\"length\":4096,\"digest\":\"{filler}\"}}}}}}",
    )
}

/// A store whose page walk cannot finish inside its bound is slow, not corrupt.
///
/// The real box store needs about nineteen million VM steps for `quick_check`,
/// twice the launch budget, and used to be reported as corrupt for it. The
/// classification must say so without calling the data damaged, and the same
/// store must still pass an unbounded check.
#[test]
fn a_walk_that_exhausts_its_budget_is_slow_and_never_corrupt() -> TestResult {
    let root = scratch("walk-budget")?;
    let database = ready_store(&root)?;
    // One page-walk step cannot finish on any non-empty store, so this is the
    // exhaustion the real store hits without needing a two hundred megabyte
    // fixture to reach it.
    match preflight_with_walk_budget(&database, 1) {
        StoreReadiness::Slow => {}
        other => panic!("a spent walk budget must classify Slow, found {other:?}"),
    }
    // The same store, walked to completion, is genuinely clean: the slow verdict
    // was about the check's bound, not the data.
    match preflight_read_only(&database) {
        StoreReadiness::Ready => {}
        other => panic!("the store is healthy; the unbounded walk found {other:?}"),
    }
    Ok(())
}

/// The bound must not swallow real damage: a corrupt store still reports it.
#[test]
fn a_corrupt_store_is_still_corrupt_when_the_walk_is_bounded() -> TestResult {
    let root = scratch("walk-budget-corrupt")?;
    let database = ready_store(&root)?;
    // Zero a table's root page, the corruption the recovery witness above
    // already proves SQLite reports structurally.
    let (page, page_size) = {
        let connection = Connection::open(&database)?;
        let page: i64 = connection.query_row(
            "SELECT rootpage FROM sqlite_master WHERE type = 'table' AND name = 'mailbox_message'",
            [],
            |row| row.get(0),
        )?;
        let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
        (page, page_size)
    };
    let mut bytes = std::fs::read(&database)?;
    let start = ((page - 1) * page_size) as usize;
    let end = start + page_size as usize;
    for byte in bytes
        .get_mut(start..end)
        .ok_or_else(|| std::io::Error::other("the database shrank below the zeroed page"))?
    {
        *byte = 0;
    }
    std::fs::write(&database, bytes)?;
    // A bound far above this fixture's size, so the walk reaches the damage
    // rather than stopping early and calling it Slow.
    match preflight_with_walk_budget(&database, 100_000_000) {
        StoreReadiness::QuarantinedOrCorrupt(Obstruction::Integrity(_)) => Ok(()),
        other => panic!("a bounded walk accepted a damaged store: {other:?}"),
    }
}

/// A hot-journal recovery whose post-recovery walk spends its budget must still
/// open, and must say the check was slow.
///
/// This drives the real recovery path, not the classifier alone. Before the fix
/// a Slow classification fell into the refusal arm, so the launch failed with
/// `PostActionNotReady(Slow)` on any store too large to finish the walk.
#[test]
fn a_recovery_whose_walk_spends_its_budget_opens_ready_and_says_slow() -> TestResult {
    let root = scratch("recovery-slow")?;
    let database = ready_store(&root)?;
    let _pre_tear = tear(&database, &root)?;
    let authorities = FixedAuthorities;
    // A one-step bound cannot finish on any non-empty store.
    let opened = grapt_store::EventStore::recover_hot_journal_with_bound(
        &database,
        ENGINE_VERSION,
        &authorities,
        1,
    )
    .map_err(|error| format!("the recovery refused a healthy store: {error}"))?;
    let OwnedOpenReceipt::RecoveredHotJournal { slow } = opened.receipt() else {
        panic!(
            "expected the recovered-hot-journal receipt, found {:?}",
            opened.receipt()
        )
    };
    assert!(
        slow,
        "the recovery reported a finished check it could not have made"
    );
    drop(opened);
    let journal = journal_path(&database)?;
    assert!(!journal.exists(), "the hot journal survived the rollback");
    Ok(())
}

/// The launch walk must stay bounded and finite.
///
/// Nothing else pins this: the recovery witness injects its own one-step bound,
/// so making the production budget unbounded would silently put a whole-database
/// audit back on the launcher's readiness and pass every other test.
#[test]
fn the_launch_walk_stays_bounded() {
    let operations = grapt_store::migration::preflight::launch_walk_operations();
    assert!(
        matches!(operations, Some(budget) if budget > 0 && budget < i64::MAX),
        "the launch walk must be bounded and finite, found {operations:?}"
    );
}
