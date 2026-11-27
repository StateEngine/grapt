//! This service creates its schema in ONE durable
//! commit, and a current schema costs none.
//!
//! SQLite increments the database header's file change counter (bytes 24..28)
//! once per committed write transaction in the explicit rollback-journal fixture,
//! so the counter counts the commits,
//! and with them the journal and database syncs, this initialization pays.

use grapt_store::EventStore;
use std::path::Path;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn change_counter(database: &Path) -> TestResult<u32> {
    let bytes = std::fs::read(database)?;
    let field: [u8; 4] = bytes
        .get(24..28)
        .ok_or("the database is shorter than its header")?
        .try_into()?;
    Ok(u32::from_be_bytes(field))
}

#[test]
fn the_form_schema_is_created_in_one_commit_and_kept_in_none() -> TestResult {
    let root =
        std::env::temp_dir().join(format!("grapt-form-schema-commit-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let database = root.join("store.db");
    let mut store = EventStore::open(&database, "0.1.0")?;
    // This counter measures rollback-journal commits, not writes retained in WAL.
    // Production's WAL policy has separate witnesses; keep this schema-atomicity probe explicit.
    store
        .connection()
        .execute_batch("PRAGMA journal_mode = DELETE")?;
    let fresh = change_counter(&database)?;
    grapt_form::FormService::initialize(&mut store)?;
    assert_eq!(
        change_counter(&database)?,
        fresh + 1,
        "PSF104 the form service created its schema in more than one durable commit"
    );
    grapt_form::FormService::initialize(&mut store)?;
    assert_eq!(
        change_counter(&database)?,
        fresh + 1,
        "a current form schema committed again"
    );
    drop(store);
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
