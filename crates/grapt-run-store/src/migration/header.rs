//! The SQLite file header, read with plain file I/O before SQLite opens the
//! file. A writable SQLite connection replays a hot journal on its first read,
//! so a file that is not this store, or was written by a newer build, is
//! refused here and keeps both its bytes and its journal untouched.
use crate::error::OpenError;
use std::fs::File;
use std::io::{ErrorKind, Read};
use std::path::Path;

const HEADER: usize = 100;
const MAGIC: &[u8; 16] = b"SQLite format 3\0";
const USER_VERSION: usize = 60;
const APPLICATION_ID: usize = 68;

/// Refuse a foreign or newer file at `path`. A missing or empty file is a new
/// store. A non-empty file without this store's application id is foreign,
/// even if it is SQLite: a new store gets its id in its first transaction.
pub(super) fn check(path: &Path, application_id: u32, max_supported: u32) -> Result<(), OpenError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut header = Vec::with_capacity(HEADER);
    file.take(HEADER as u64).read_to_end(&mut header)?;
    if header.is_empty() {
        return Ok(());
    }
    if header.len() < HEADER || !header.starts_with(MAGIC) {
        return Err(OpenError::Schema(0));
    }
    let field = |at: usize| [header[at], header[at + 1], header[at + 2], header[at + 3]];
    let version = i64::from(i32::from_be_bytes(field(USER_VERSION)));
    if u32::from_be_bytes(field(APPLICATION_ID)) != application_id {
        return Err(OpenError::Schema(version));
    }
    if version > i64::from(max_supported) {
        return Err(OpenError::FutureSchemaVersion {
            found: version,
            max_supported,
        });
    }
    Ok(())
}
