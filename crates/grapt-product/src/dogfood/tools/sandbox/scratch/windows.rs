//! Windows confined scratch allocation and cleanup.
//!
//! At allocation, the scratch root is opened with `FILE_LIST_DIRECTORY |
//! FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE | SYNCHRONIZE`,
//! `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`, and no
//! `FILE_SHARE_DELETE` (never `FILE_FLAG_DELETE_ON_CLOSE`). It is validated
//! on the open handle via `winapi_util::file::information` (real directory,
//! not a reparse point) before its handle is stored.
//!
//! At cleanup (only after child exit is confirmed), the already-open verified
//! root handle is emptied via `remove_dir_all::RemoveDir::remove_dir_contents`
//! (handle-relative `NtCreateFile` traversal + `SetFileInformationByHandle`
//! with `FileDispositionInfoEx`/`FileDispositionInfo`) and then deleted on
//! that exact same handle via `fs_at::os::windows::FileExt::delete_by_handle`,
//! with zero handle-drop-and-reopen-by-name.

use super::combine_errors;
use fs_at::os::windows::FileExt as _;
use remove_dir_all::RemoveDir as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
struct WinFileId {
    volume: u64,
    index: u64,
}

pub(super) struct PinnedScratch {
    root_handle: std::fs::File,
    root_id: WinFileId,
    path: PathBuf,
}

impl PinnedScratch {
    pub(super) fn create(parent: &Path) -> Result<Self, String> {
        // Disarm TempDir::drop immediately via `.keep()` so neither attach
        // errors nor unwinds can ever trigger recursive pathname remove_dir_all.
        let path = tempfile::Builder::new()
            .prefix("grapt-sandbox-tmp-")
            .tempdir_in(parent)
            .map_err(|error| format!("sandbox private temp directory unavailable: {error}"))?
            .keep();
        match Self::pin(path.clone()) {
            Ok(pinned) => Ok(pinned),
            Err(reason) => {
                let cleanup_err = std::fs::remove_dir(&path)
                    .err()
                    .map(|error| format!("sandbox private temp root rmdir failed: {error}"));
                Err(combine_errors(reason, [cleanup_err]))
            }
        }
    }

    fn pin(path: PathBuf) -> Result<Self, String> {
        let (root_handle, root_id) = open_pinned_dir_handle(&path)?;
        Ok(Self {
            root_handle,
            root_id,
            path,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn cleanup(mut self) -> Result<(), String> {
        verify_handle_identity(&self.root_handle, self.root_id, &self.path)?;
        self.root_handle
            .remove_dir_contents(Some(&self.path))
            .map_err(|error| {
                format!(
                    "sandbox private temp contents cleanup failed at {}: {error}",
                    self.path.display()
                )
            })?;
        verify_handle_identity(&self.root_handle, self.root_id, &self.path)?;
        self.root_handle
            .delete_by_handle()
            .map_err(|(_handle, error)| {
                format!(
                    "sandbox private temp root handle delete failed at {}: {error}",
                    self.path.display()
                )
            })
    }
}

fn open_pinned_dir_handle(path: &Path) -> Result<(std::fs::File, WinFileId), String> {
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const DELETE: u32 = 0x0001_0000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    // Request FILE_LIST_DIRECTORY (non-attribute access) with no
    // FILE_SHARE_DELETE so Windows pins the directory name against rename or
    // reparse replacement while this handle is held. Never arm
    // FILE_FLAG_DELETE_ON_CLOSE; deletion happens only via
    // SetFileInformationByHandle(FileDispositionInfo*) after validation.
    let access =
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE | SYNCHRONIZE;
    let flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
    let file = std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(flags)
        .open(path)
        .map_err(|error| {
            format!(
                "sandbox private temp directory handle open failed at {}: {error}",
                path.display()
            )
        })?;
    let info = winapi_util::file::information(&file).map_err(|error| {
        format!(
            "sandbox private temp directory handle info failed at {}: {error}",
            path.display()
        )
    })?;
    let attrs = info.file_attributes();
    if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0 || (attrs & FILE_ATTRIBUTE_DIRECTORY) == 0 {
        return Err(format!(
            "sandbox private temp directory is a reparse point or non-directory: {}",
            path.display()
        ));
    }
    let id = WinFileId {
        volume: info.volume_serial_number(),
        index: info.file_index(),
    };
    Ok((file, id))
}

fn verify_handle_identity(
    handle: &std::fs::File,
    expected: WinFileId,
    path: &Path,
) -> Result<(), String> {
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    let info = winapi_util::file::information(handle).map_err(|error| {
        format!(
            "sandbox private temp handle verify failed at {}: {error}",
            path.display()
        )
    })?;
    let attrs = info.file_attributes();
    let actual = WinFileId {
        volume: info.volume_serial_number(),
        index: info.file_index(),
    };
    if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || (attrs & FILE_ATTRIBUTE_DIRECTORY) == 0
        || actual != expected
    {
        return Err(format!(
            "sandbox private temp root handle identity changed before cleanup: {}",
            path.display()
        ));
    }
    Ok(())
}
