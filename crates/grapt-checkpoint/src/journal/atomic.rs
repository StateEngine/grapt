//! Publish complete journal records while the directory's exclusive owner is held.
use super::*;
use std::io::Write;

struct Pending(PathBuf);

impl Drop for Pending {
    fn drop(&mut self) {
        let _removed = fs::remove_file(&self.0);
    }
}

impl FileRestoreJournal {
    pub(super) fn publish(&self, path: &Path, bytes: &[u8]) -> Result<(), JournalError> {
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(JournalError::OverBound);
        }
        let pending = Pending(path.with_extension("pending"));
        // Only unpublished staging bytes use this reserved sibling name.
        // The directory owner lock prevents another writer using it.
        match fs::remove_file(&pending.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(JournalError::Unavailable),
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&pending.0)
            .map_err(|_| JournalError::Unavailable)?;
        file.write_all(bytes)
            .map_err(|_| JournalError::Unavailable)?;
        file.sync_all().map_err(|_| JournalError::Unavailable)?;
        drop(file);
        fs::rename(&pending.0, path).map_err(|_| JournalError::Unavailable)?;
        self.durability.sync_parent(&self.root)
    }

    pub(super) fn preserve_incomplete(&self, operation: &str) -> Result<(), JournalError> {
        let bytes = self.raw(operation)?.ok_or(JournalError::Missing)?;
        let archive = self
            .path(operation)
            .with_extension(format!("damaged-{:x}", Sha256::digest(&bytes)));
        self.publish(&archive, &bytes)
    }
}
