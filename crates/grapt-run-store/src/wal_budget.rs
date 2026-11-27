//! Admission headroom for writes to ordinary rowid tables.

use crate::command::MAX_COMMAND_BYTES;
use crate::error::{OpenError, WriteError};
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

pub(crate) const WAL_CAP: u64 = 64 * 1024 * 1024;
pub(crate) const CACHE_KIB: i32 = 4096;
const REQUEST_WINDOW: u64 = 32 * 1024 * 1024;

// SQLite B-trees have a 20-level depth limit. A rebalance can dirty
// neighboring pages and parents. Cover a delete and an insert on each tree.
const REBALANCE_PAGES: u64 = 2 * 20 * 6;
const COMMIT_PADDING: u64 = 64 * 1024;
const FRAME_HEADER: u64 = 24;

pub(crate) struct WalBudget {
    file: PathBuf,
    stop_at: u64,
    rows: HashMap<String, u64>,
    request: Mutex<RequestBudget>,
}

#[derive(Default)]
struct RequestBudget {
    remaining: u64,
    refusal: Option<WriteError>,
}

impl WalBudget {
    pub(crate) fn new(
        connection: &Connection,
        file: PathBuf,
        internal: &[&str],
    ) -> Result<Self, OpenError> {
        let page: u32 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
        let frame = u64::from(page) + FRAME_HEADER;
        let tables = connection
            .prepare(
                "SELECT name FROM pragma_table_list
             WHERE schema='main' AND type='table' AND wr=0",
            )?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut rows = HashMap::new();
        for table in tables {
            if table.starts_with("sqlite_")
                || internal.iter().any(|name| table.eq_ignore_ascii_case(name))
            {
                continue;
            }
            let (indexes, unique): (u32, u32) = connection.query_row(
                "SELECT count(*), coalesce(sum(\"unique\"),0) FROM pragma_index_list(?1)",
                [&table],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            rows.insert(table, row_budget(u64::from(page), indexes, unique));
        }
        let per_row = rows
            .values()
            .copied()
            .max()
            .unwrap_or(frame * REBALANCE_PAGES);
        // The callback observes a completed row. Leave room for an in-flight
        // row, any final mutation before SQLite next checks interrupt, the
        // resident dirty cache, and the final WAL commit frame and padding.
        let reserved = 2 * per_row + 2 * (CACHE_KIB as u64) * 1024 + 4 * frame + COMMIT_PADDING;
        if reserved >= WAL_CAP || per_row > REQUEST_WINDOW {
            return Err(OpenError::WalLayout);
        }
        Ok(Self {
            file,
            stop_at: WAL_CAP - reserved,
            rows,
            request: Mutex::new(RequestBudget::default()),
        })
    }

    pub(crate) fn can_write(&self, table: &str) -> bool {
        self.rows.contains_key(table)
    }

    pub(crate) fn start_request(&self) {
        *self
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = RequestBudget {
            remaining: REQUEST_WINDOW,
            refusal: None,
        };
    }

    pub(crate) fn changed(&self, table: &str) -> bool {
        let mut request = self
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(cost) = self.rows.get(table) else {
            request.refusal = Some(WriteError::WalPressure);
            return true;
        };
        if let Some(remaining) = request.remaining.checked_sub(*cost) {
            request.remaining = remaining;
        } else {
            request.refusal = Some(WriteError::WalPressure);
        }
        drop(request);
        self.interrupted()
    }

    pub(crate) fn interrupted(&self) -> bool {
        let mut request = self
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if request.refusal.is_none() {
            request.refusal = self.check().err();
        }
        request.refusal.is_some()
    }

    pub(crate) fn take_refusal(&self) -> Option<WriteError> {
        self.request
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .refusal
            .take()
    }

    pub(crate) fn check(&self) -> Result<(), WriteError> {
        if self.at_limit(self.size()?) {
            return Err(WriteError::WalPressure);
        }
        Ok(())
    }

    pub(crate) fn at_limit(&self, bytes: u64) -> bool {
        bytes >= self.stop_at
    }

    pub(crate) fn size(&self) -> Result<u64, WriteError> {
        match fs::metadata(&self.file) {
            Ok(metadata) => Ok(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(WriteError::WalIo(error)),
        }
    }
}

fn row_budget(page: u64, indexes: u32, unique: u32) -> u64 {
    let trees = u64::from(indexes) + 1;
    let frame = page + FRAME_HEADER;
    // Overflow pages carry a next-page pointer. A replacement can implicitly
    // remove one conflicting row per unique index plus the rowid conflict.
    // Removing overflow payload frees pages rather than rewriting the payload.
    let record = (MAX_COMMAND_BYTES as u64).div_ceil(page - 4) * frame;
    let replacements = u64::from(unique) + 2;
    trees * (record + replacements * REBALANCE_PAGES * frame)
}
