//! Windows verified snapshots, under the ruled trusted per-user prefix model.

use std::fs::{File, Metadata};
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::path::Path;

use super::windows_io as io;

#[derive(Debug)]
pub(super) struct Stamp {
    // Retain final-name physical identity with no data access. Windows still
    // refuses moving its ancestor while this metadata handle is held.
    identity: same_file::Handle,
    metadata: Metadata,
}

impl Stamp {
    pub(super) fn capture(path: &Path, file: &File) -> Result<Self, String> {
        let metadata = file
            .metadata()
            .map_err(|error| format!("verified input cannot be identified: {:?}", error.kind()))?;
        let retained = io::snapshot_identity(path, metadata.is_dir())?;
        let identity = same_file::Handle::from_file(retained).map_err(|error| {
            format!("verified input identity is unavailable: {:?}", error.kind())
        })?;
        let actual = same_file::Handle::from_file(file.try_clone().map_err(|error| {
            format!("verified input handle is unavailable: {:?}", error.kind())
        })?)
        .map_err(|error| format!("verified input identity is unavailable: {:?}", error.kind()))?;
        if identity != actual {
            return Err("verified installation input changed while retaining identity".into());
        }
        Ok(Self { identity, metadata })
    }

    pub(super) fn directory(path: &Path) -> Result<Self, String> {
        let file = io::snapshot_identity(path, true)?;
        let stamp = Self::capture(path, &file)?;
        stamp.recheck(path)?;
        Ok(stamp)
    }

    pub(super) fn recheck(&self, path: &Path) -> Result<(), String> {
        let file = io::snapshot_identity(path, self.metadata.is_dir())?;
        let named = file
            .metadata()
            .map_err(|error| format!("verified input cannot be rechecked: {:?}", error.kind()))?;
        let actual = same_file::Handle::from_file(file).map_err(|error| {
            format!(
                "verified input identity cannot be rechecked: {:?}",
                error.kind()
            )
        })?;
        let expected = &self.metadata;
        if self.identity != actual
            || expected.is_dir() != named.is_dir()
            || (!expected.is_dir()
                && (expected.len() != named.len()
                    || expected.last_write_time() != named.last_write_time()
                    || expected.creation_time() != named.creation_time()
                    || expected.file_attributes() != named.file_attributes()))
        {
            return Err("verified installation input changed before activation".into());
        }
        Ok(())
    }
}

pub(super) fn read(path: &Path, cap: u64) -> Result<(Vec<u8>, Stamp), String> {
    let file = io::snapshot_regular(path)?;
    let stamp = Stamp::capture(path, &file)?;
    let mut bytes = Vec::new();
    (&file)
        .take(cap.checked_add(1).ok_or("package record bound overflows")?)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("package record cannot be read: {:?}", error.kind()))?;
    if bytes.len() as u64 > cap {
        return Err("package record exceeds its byte bound".into());
    }
    stamp.recheck(path)?;
    Ok((bytes, stamp))
}

#[cfg(test)]
#[path = "windows_snapshot_tests.rs"]
mod tests;
