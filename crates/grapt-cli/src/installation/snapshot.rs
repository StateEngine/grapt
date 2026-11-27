//! Retained verified file identities, under the trusted per-user prefix model.

use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use super::io;

#[derive(Debug)]
pub(super) struct Stamp {
    // Retention prevents an unlinked inode from being recycled into its name.
    pub(super) file: File,
    metadata: Metadata,
}

impl Stamp {
    /// A rename changes ctime on Linux. Retain the same open inode and require
    /// every other content and permission attribute to survive publication.
    pub(super) fn after_rename(&self, path: &Path) -> Result<Self, String> {
        let file = self.file.try_clone().map_err(|error| {
            format!(
                "published launcher handle is unavailable: {:?}",
                error.kind()
            )
        })?;
        let published = Self::capture(file)?;
        let before = &self.metadata;
        let after = &published.metadata;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.mode() != after.mode()
        {
            return Err("verified launcher changed during publication".into());
        }
        published.recheck(path)?;
        Ok(published)
    }

    pub(super) fn capture(file: File) -> Result<Self, String> {
        let metadata = file
            .metadata()
            .map_err(|error| format!("verified input cannot be identified: {:?}", error.kind()))?;
        Ok(Self { file, metadata })
    }

    pub(super) fn directory(path: &Path) -> Result<Self, String> {
        io::directory(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("verified directory cannot be opened: {:?}", error.kind()))?;
        let stamp = Self::capture(file)?;
        stamp.recheck(path)?;
        Ok(stamp)
    }

    pub(super) fn recheck(&self, path: &Path) -> Result<(), String> {
        let named = if self.metadata.is_dir() {
            io::directory(path)?;
            std::fs::symlink_metadata(path)
                .map_err(|error| format!("verified directory is unreadable: {:?}", error.kind()))?
        } else {
            io::regular(path)?
                .metadata()
                .map_err(|error| format!("verified input is unreadable: {:?}", error.kind()))?
        };
        let expected = &self.metadata;
        if expected.dev() != named.dev()
            || expected.ino() != named.ino()
            || expected.is_dir() != named.is_dir()
            || (!expected.is_dir()
                && (expected.len() != named.len()
                    || expected.mtime() != named.mtime()
                    || expected.mtime_nsec() != named.mtime_nsec()
                    || expected.ctime() != named.ctime()
                    || expected.ctime_nsec() != named.ctime_nsec()
                    || expected.mode() != named.mode()))
        {
            return Err("verified installation input changed before activation".into());
        }
        Ok(())
    }
}

pub(super) fn read(path: &Path, cap: u64) -> Result<(Vec<u8>, Stamp), String> {
    let stamp = Stamp::capture(io::regular(path)?)?;
    let mut bytes = Vec::new();
    (&stamp.file)
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("package record cannot be read: {:?}", error.kind()))?;
    if bytes.len() as u64 > cap {
        return Err("package record exceeds its byte bound".into());
    }
    stamp.recheck(path)?;
    Ok((bytes, stamp))
}
