//! Read-only event log over an oracle-era database file.

use crate::decode::{decode_row, DecodeError, DecodedEvent};
use rusqlite::{Connection, OpenFlags};
use std::cell::{Ref, RefCell};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// A read-only view of the durable event log. Ordering law: aggregates fold
/// independently; within an aggregate, `seq` is the replay order.
#[derive(Debug)]
pub struct EventLog {
    pub(crate) conn: RefCell<Connection>,
}

impl EventLog {
    /// Open through the real file behind a final file link, so the view
    /// reads the same `-wal` the writer commits to. A link that cannot be
    /// resolved is opened as given and SQLite refuses it.
    pub fn open_read_only(path: &Path) -> Result<Self, LogError> {
        let real = crate::migration::authority::real_database_path(path)
            .unwrap_or_else(|_| path.to_path_buf());
        let conn = Connection::open_with_flags(real, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self {
            conn: RefCell::new(conn),
        })
    }

    /// Crate-private immutable access for bounded SQLite read probes.
    pub(crate) fn read_only_connection(&self) -> rusqlite::Result<Ref<'_, Connection>> {
        self.conn.try_borrow().map_err(|_| {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                Some(
                    "event log is borrowed by an active snapshot; finish it before another read"
                        .into(),
                ),
            )
        })
    }

    pub fn event_count(&self) -> Result<u64, LogError> {
        let n: i64 =
            self.read_only_connection()?
                .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Visit every event in replay order (aggregate, then seq). Strict decode:
    /// the first contract-violating row aborts the walk with its reason.
    pub fn for_each_in_replay_order(
        &self,
        mut visit: impl FnMut(&DecodedEvent),
    ) -> Result<u64, LogError> {
        let conn = self.read_only_connection()?;
        let mut stmt = conn.prepare(
            "SELECT id, aggregate_id, seq, created, type, data
             FROM event ORDER BY aggregate_id, seq",
        )?;
        let mut rows = stmt.query([])?;
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let aggregate: String = row.get(1)?;
            let seq: i64 = row.get(2)?;
            let created: i64 = row.get(3)?;
            let kind: String = row.get(4)?;
            let data: String = row.get(5)?;
            let event = decode_row(&id, &aggregate, seq, created, &kind, &data)?;
            visit(&event);
            count += 1;
        }
        Ok(count)
    }
}

/// Links are made with Unix symlinks; Windows link creation needs a privilege.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A read through a link sees commits still held in the real file's WAL.
    #[test]
    fn a_linked_log_reads_commits_still_in_the_wal() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("grapt-log-link-{}", std::process::id()));
        let _stale = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("real"))?;
        let (real, link) = (root.join("real").join("store.db"), root.join("link.db"));
        let writer = Connection::open(&real)?;
        writer.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE event (id TEXT PRIMARY KEY);
             INSERT INTO event VALUES ('evt_in_wal');",
        )?;
        std::os::unix::fs::symlink(&real, &link)?;
        assert!(root.join("real").join("store.db-wal").metadata()?.len() > 0);
        assert_eq!(EventLog::open_read_only(&link)?.event_count()?, 1);
        assert!(
            !root.join("link.db-wal").exists(),
            "a sidecar was made beside the link"
        );
        drop(writer);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
