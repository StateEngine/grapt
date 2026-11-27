//! Retained Windows identities and no-delete leases, under the per-user ACL
//! trust model. This module does not inspect or modify an ACL or install state.

use std::fs::{File, Metadata, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf, Prefix};

use super::{io, PrefixError};

const REPARSE_POINT: u32 = 0x0000_0400;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const SHARE_READ_WRITE: u32 = 0x0000_0003;
const MAX_COMPONENTS: usize = 64;

pub(super) fn reparse(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink() || metadata.file_attributes() & REPARSE_POINT != 0
}

pub(super) fn lock_options(options: &mut OpenOptions) {
    // Preserve other holders' read/write access, but never share deletion of
    // the named lock. Metadata is checked before any kernel lock operation.
    options
        .custom_flags(OPEN_REPARSE_POINT)
        .share_mode(SHARE_READ_WRITE);
}

pub(super) fn optional_identity(path: &Path) -> Result<Option<same_file::Handle>, PrefixError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !reparse(&metadata) => {
            same_file::Handle::from_path(path).map(Some).map_err(io)
        }
        Ok(_) => Err(PrefixError::Nonregular),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io(error)),
    }
}

pub(super) fn identified(
    file: &File,
    path: &Path,
    before: Option<&same_file::Handle>,
) -> Result<(), PrefixError> {
    let opened = identity(file)?;
    if before.is_some_and(|before| before != &opened) {
        return Err(PrefixError::Changed);
    }
    revalidate(&opened, path)
}

pub(super) fn identity(file: &File) -> Result<same_file::Handle, PrefixError> {
    same_file::Handle::from_file(file.try_clone().map_err(io)?).map_err(io)
}

pub(super) fn revalidate(opened: &same_file::Handle, path: &Path) -> Result<(), PrefixError> {
    let named = same_file::Handle::from_path(path).map_err(io)?;
    if opened != &named {
        return Err(PrefixError::Changed);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(io)?;
    if reparse(&metadata) {
        return Err(PrefixError::Nonregular);
    }
    Ok(())
}

pub(super) fn directories(prefix: &Path) -> Result<Vec<File>, PrefixError> {
    if !prefix.is_absolute() || prefix.components().count() > MAX_COMPONENTS {
        return Err(PrefixError::Nonregular);
    }
    let mut leases = Vec::new();
    let mut path = PathBuf::new();
    for component in prefix.components() {
        match component {
            Component::Prefix(value)
                if matches!(value.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) =>
            {
                path.push(component);
                continue;
            }
            Component::RootDir | Component::Normal(_) => path.push(component),
            _ => return Err(PrefixError::Nonregular),
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
            .share_mode(SHARE_READ_WRITE)
            .open(&path)
            .map_err(io)?;
        let metadata = file.metadata().map_err(io)?;
        if !metadata.is_dir() || reparse(&metadata) {
            return Err(PrefixError::Nonregular);
        }
        identified(&file, &path, None)?;
        leases.push(file);
    }
    Ok(leases)
}
