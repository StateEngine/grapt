//! Explicit testkit-only startup commit observation, independent of journal/checkpoint state.
use grapt_store::EventStore;
use std::{fs::OpenOptions, io::Write, path::Path};

pub(super) fn observe(root: &Path, store: &mut EventStore) -> Result<(), String> {
    let Some(selected) = std::env::var_os("GRAPT_STARTUP_COMMIT_PROFILE") else {
        return Ok(());
    };
    if std::fs::canonicalize(root).map_err(|e| e.to_string())?
        != std::fs::canonicalize(Path::new(&selected)).map_err(|e| e.to_string())?
    {
        return Ok(());
    }
    let label =
        std::env::var("GRAPT_STARTUP_COMMIT_RUN").map_err(|_| "startup commit run absent")?;
    if !matches!(label.as_str(), "first" | "restart") {
        return Err("invalid startup commit run".into());
    }
    install(store, &root.join(format!("startup-{label}.commits")))
}

fn install(store: &mut EventStore, marker: &Path) -> Result<(), String> {
    // The contract ends at readiness: work after it, such as the deferred
    // scan index build, is not startup. A stale receipt would hide every commit.
    let receipt = marker.with_file_name(crate::ready::READY_RECEIPT_FILE);
    if receipt.exists() {
        return Err("startup commit observation began after readiness".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .map_err(|e| format!("startup commit observation cannot be created: {e}"))?;
    let mut count = 0usize;
    store
        .commit_hook(Some(move || {
            if receipt.exists() {
                return false;
            }
            // Fail the fixture's commit instead of losing evidence on exhaustion or I/O failure.
            if count == 256 {
                return true;
            }
            count += 1;
            file.write_all(b"C").is_err()
        }))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_checkpointed_wal_write_remains_visible_to_the_startup_commit_observer(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("startup-commit-hook-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let database = root.join("store.sqlite3");
        let marker = root.join("commits");
        let mut store = EventStore::open(&database, "startup-commit-observer")?;
        install(&mut store, &marker)?;
        assert_eq!(std::fs::read(&marker)?.len(), 0);
        store.elect_writer(&grapt_store::AggregateId::Session(
            "ses_startup_commit".parse()?,
        ))?;
        store
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        let mut wal = database.as_os_str().to_os_string();
        wal.push("-wal");
        assert_eq!(std::fs::metadata(wal)?.len(), 0);
        let observed = std::fs::read(&marker)?;
        assert_eq!(observed, b"C");
        assert_ne!(
            observed.len(),
            0,
            "a checkpointed startup write must fail the zero-commit contract"
        );
        drop(store);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
