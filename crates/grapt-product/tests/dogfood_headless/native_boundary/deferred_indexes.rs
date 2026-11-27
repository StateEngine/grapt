//! A store from before the mailbox scan indexes starts within the launch
//! window and gains both indexes after readiness, not at open.
#![cfg(target_os = "linux")]
use super::*;

/// The deferred mailbox indexes present on the store, read beside the service.
fn scan_indexes(root: &Path) -> i64 {
    rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .and_then(|store| {
        store.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('mailbox_message_open_idx','mailbox_message_debt_idx')",
            [],
            |row| row.get(0),
        )
    })
    .unwrap_or_else(|error| panic!("index listing unreadable: {error}"))
}

#[test]
fn a_store_from_before_the_scan_indexes_gains_them_after_readiness() -> TestResult {
    let _journey = process_journey_custody();
    let root = root("deferred-indexes");
    let _root_cleanup = super::super::terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 90_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut first = process_fixture::spawn_controlled(&root, provider.address);
    let _address = await_ready(&root, &mut first);
    stop_cleanly(first, &root)?;
    super::idle_cpu::seed_history(&root, 10_000, super::idle_cpu::MailAttentionKind::Consumed)?;
    // The shape a build from before the scan indexes left behind.
    rusqlite::Connection::open(root.join("database.sqlite3"))?.execute_batch(
        "DROP INDEX IF EXISTS mailbox_message_open_idx; DROP INDEX IF EXISTS mailbox_message_debt_idx;",
    )?;
    assert_eq!(scan_indexes(&root), 0);
    // await_ready's bound is the launcher's own 30 s window.
    let mut second = process_fixture::spawn_controlled(&root, provider.address);
    let _address = await_ready(&root, &mut second);
    let built = PollDeadline::within(Duration::from_secs(30));
    while scan_indexes(&root) < 2 {
        assert!(
            built.pending(),
            "the scan indexes were not built after readiness"
        );
        thread::sleep(Duration::from_millis(50));
    }
    stop_cleanly(second, &root)
}
