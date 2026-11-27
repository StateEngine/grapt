//! PRODUCT-STARTUP-FSYNC-01: a steady restart of the real product reaches
//! readiness without committing anything to its store.
//!
//! An explicit testkit commit hook starts after the core store opens and before
//! owner-schema installation. Checkpointing cannot hide a commit from this
//! observation; missing or malformed fixture output fails the witness.

use super::*;

#[cfg(windows)]
#[test]
fn a_ready_service_releases_its_profile_protection_process(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("startup-protection-release");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut product = process_fixture::spawn_controlled(&root, provider.address);
    let _address = await_ready(&root, &mut product);
    let system = std::env::var_os("SystemRoot").ok_or("SystemRoot is absent")?;
    let output = root.join("protection-process-count");
    // Inspect only the disposable service's direct children. Never read argv,
    // environment, file paths or credential material from another process.
    let mut observer = Command::new(
        Path::new(&system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    )
    .env_clear()
    .env("SystemRoot", &system)
    .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
    .arg(format!(
        "$ErrorActionPreference = 'Stop'; @(Get-CimInstance Win32_Process -Filter 'ParentProcessId = {}' | Where-Object Name -eq 'powershell.exe').Count",
        product.id()
    ))
    .stdin(Stdio::null())
    .stdout(fs::File::create(&output)?)
    .stderr(Stdio::null())
    .spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        match observer.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if tokio::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => break Err(std::io::Error::other("process observer timed out")),
            Err(error) => break Err(error),
        }
    };
    let _killed = observer.kill();
    observer.wait()?;
    assert!(status?.success(), "process observer failed");
    let mut count = String::new();
    fs::File::open(output)?
        .take(17)
        .read_to_string(&mut count)?;
    assert!(count.len() <= 16, "process count exceeded its byte bound");
    assert_eq!(
        count.trim(),
        "0",
        "ready service retained its protection helper"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    product.stop_after_witness(&root);
    drop(product);
    process_fixture::cleanup(&root);
    Ok(())
}

fn spawn_observed(root: &Path, provider: SocketAddr, label: &str) -> process_fixture::ManagedChild {
    spawn_with(
        root,
        provider,
        &[
            (
                "GRAPT_STARTUP_COMMIT_PROFILE",
                root.to_str()
                    .unwrap_or_else(|| panic!("fixture root is not UTF-8")),
            ),
            ("GRAPT_STARTUP_COMMIT_RUN", label),
        ],
    )
}
fn assert_commits(root: &Path, label: &str, expected: usize) {
    let path = root.join(format!("startup-{label}.commits"));
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("startup commit observation missing: {error}"));
    assert!(
        bytes.len() <= 256 && bytes.iter().all(|byte| *byte == b'C'),
        "startup commit observation is malformed"
    );
    assert_eq!(
        bytes.len(),
        expected,
        "startup committed an unexpected number of owner transactions"
    );
}

#[test]
fn a_steady_restart_commits_nothing_to_the_store_before_readiness() {
    let _journey = process_journey_custody();
    let root = root("psf-steady-restart");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    // The first serve owns, migrates and creates every schema: it commits.
    let mut first = spawn_observed(&root, provider.address, "first");
    let _address = await_ready(&root, &mut first);
    assert_commits(&root, "first", 1);
    first.stop_after_witness(&root);
    drop(first);

    let mut restarted = spawn_observed(&root, provider.address, "restart");
    let _address = await_ready(&root, &mut restarted);
    assert_commits(&root, "restart", 0);
    restarted.stop_after_witness(&root);
    drop(restarted);

    drop(provider);
    process_fixture::cleanup(&root);
}

/// Every table composition creates on a fresh store, across its owners.
const OWNER_TABLES: &[&str] = &[
    "instruction_blob",
    "instruction_entry",
    "instruction_state",
    "form_request",
    "shell_operation",
    "shell_artifact",
    "product_shell_binding",
    "session_pending",
    "session_message",
    "inbox_legacy_synthetic_audit",
    "session_operation_delivery_pending",
    "subagent_operation",
];

/// How many of [`OWNER_TABLES`] the store holds, read without writing.
fn owner_tables(database: &Path) -> usize {
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|error| {
                panic!("{} read-only open failed: {error}", database.display())
            });
    OWNER_TABLES
        .iter()
        .filter(|table| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or_else(|error| panic!("schema probe failed: {error}"))
                == 1
        })
        .count()
}

/// Kill the product as a crash would, then clear only the stale registration
/// files a supervisor would; the database is left exactly as the crash left it.
fn crash(child: &mut process_fixture::ManagedChild, root: &Path) {
    let _ = child.kill();
    child
        .wait()
        .unwrap_or_else(|error| panic!("crashed product reap failed: {error}"));
    for name in [
        "registration",
        "registration.elected",
        "registration.lock",
        "serve-ready.json",
    ] {
        match fs::remove_file(root.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("stale {name} cleanup failed: {error}"),
        }
    }
}

#[test]
fn a_fresh_start_installs_every_owner_schema_in_one_commit_before_readiness() {
    let _journey = process_journey_custody();
    let root = root("psf-fresh-start");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    let database = root.join("database.sqlite3");
    assert_eq!(
        owner_tables(&database),
        0,
        "the prepared store is not fresh"
    );

    let mut first = spawn_observed(&root, provider.address, "first");
    let _address = await_ready(&root, &mut first);
    assert_commits(&root, "first", 1);
    first.stop_after_witness(&root);
    drop(first);

    assert_eq!(owner_tables(&database), OWNER_TABLES.len());
    drop(provider);
    process_fixture::cleanup(&root);
}

/// A first start killed at any point leaves either every owner schema or
/// none, never some, and the next start serves with all of them.
#[test]
fn a_first_start_killed_at_any_point_leaves_all_or_no_owner_schemas() {
    let _journey = process_journey_custody();
    let provider = ProviderFixture::start(Vec::new());
    let mut observed = Vec::new();
    for delay_ms in [0, 2, 5, 10, 20, 30, 45, 60, 80, 110, 150, 200, 300] {
        let root = root(&format!("psf-crash-{delay_ms}"));
        prepare(&root, 120_000);
        let database = root.join("database.sqlite3");
        let mut first = spawn(&root, provider.address);
        thread::sleep(Duration::from_millis(delay_ms));
        crash(&mut first, &root);
        drop(first);
        let present = owner_tables(&database);
        observed.push((delay_ms, present));
        assert!(
            present == 0 || present == OWNER_TABLES.len(),
            "a crash {delay_ms} ms into the first start left {present} of {} owner tables",
            OWNER_TABLES.len()
        );
        let mut restarted = spawn(&root, provider.address);
        let _address = await_ready(&root, &mut restarted);
        restarted.stop_after_witness(&root);
        drop(restarted);
        assert_eq!(
            owner_tables(&database),
            OWNER_TABLES.len(),
            "the restart after a {delay_ms} ms crash served without every owner schema"
        );
        process_fixture::cleanup(&root);
    }
    eprintln!("owner tables present after each crash (delay ms, tables): {observed:?}");
    drop(provider);
}

#[test]
fn a_checkpointed_write_is_rejected_by_the_zero_startup_commit_assertion() {
    let _journey = process_journey_custody();
    let root = root("psf-checkpoint-observation");
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("fixture root: {error}"));
    let database = root.join("database.sqlite3");
    let mut store = grapt_store::EventStore::open(&database, "startup-commit-fixture")
        .unwrap_or_else(|error| panic!("fixture store: {error}"));
    let aggregate = grapt_store::AggregateId::Session(
        "ses_startup_commit"
            .parse()
            .unwrap_or_else(|error| panic!("fixture session: {error}")),
    );
    store
        .elect_writer(&aggregate)
        .unwrap_or_else(|error| panic!("initial writer: {error}"));
    store
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap_or_else(|error| panic!("initial checkpoint: {error}"));
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("startup-restart.commits"))
        .unwrap_or_else(|error| panic!("commit marker: {error}"));
    store
        .commit_hook(Some(move || marker.write_all(b"C").is_err()))
        .unwrap_or_else(|error| panic!("commit observer: {error}"));
    store
        .elect_writer(&aggregate)
        .unwrap_or_else(|error| panic!("observed writer: {error}"));
    store
        .connection()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap_or_else(|error| panic!("observed checkpoint: {error}"));
    assert_eq!(
        fs::metadata(root.join("database.sqlite3-wal"))
            .unwrap_or_else(|error| panic!("WAL metadata: {error}"))
            .len(),
        0
    );
    assert_commits(&root, "restart", 1);
    assert!(
        std::panic::catch_unwind(|| assert_commits(&root, "restart", 0)).is_err(),
        "a checkpointed startup write passed the zero-commit assertion"
    );
    drop(store);
    process_fixture::cleanup(&root);
}
