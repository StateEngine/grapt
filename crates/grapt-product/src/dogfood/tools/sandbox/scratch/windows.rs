//! Windows confined scratch allocation and iterative minimal-rights cleanup.
//!
//! At allocation, the scratch root is opened with `FILE_LIST_DIRECTORY |
//! FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE | SYNCHRONIZE`,
//! `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`, and no
//! `FILE_SHARE_DELETE` (never `FILE_FLAG_DELETE_ON_CLOSE`). It is validated
//! on the open handle via `winapi_util::file::information` (real directory,
//! not a reparse point) before its handle is stored.
//!
//! At cleanup (only after child exit is confirmed), an explicit iterative
//! `Vec` stack walks the tree strictly via `fs_at` relative to pinned parent
//! directory handles with minimal initial access (`DELETE |
//! FILE_READ_ATTRIBUTES | SYNCHRONIZE` — never `FILE_LIST_DIRECTORY` or
//! `FILE_WRITE_ATTRIBUTES` before inspecting the opened handle):
//! - Reparse points (`FILE_ATTRIBUTE_REPARSE_POINT`) are deleted directly on
//!   that minimal handle without descending.
//! - Real subdirectories are reopened relative to the parent with
//!   `FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE` (plus
//!   `FILE_WRITE_ATTRIBUTES` only if readonly), verified by `(volume, index)`,
//!   enumerated, and pushed onto the iterative stack.
//! - Regular files with `FILE_ATTRIBUTE_READONLY` set and `nNumberOfLinks == 1`
//!   are reopened relative to the parent with `FILE_WRITE_ATTRIBUTES | DELETE |
//!   FILE_READ_ATTRIBUTES | SYNCHRONIZE`, re-verified by `(volume, index)` and
//!   `nNumberOfLinks == 1`, have readonly cleared on that verified handle, and
//!   are deleted via `delete_by_handle()`. Ordinary files and multiply-linked
//!   files (`nNumberOfLinks > 1`) are deleted directly on the minimal
//!   `DELETE | FILE_READ_ATTRIBUTES` handle so `fs_at` cannot clear attributes
//!   on a hard-linked file.
//! - Finally, the verified root handle itself is deleted via
//!   `delete_by_handle()`.

use super::combine_errors;
use fs_at::os::windows::{FileExt as _, OpenOptionsExt as FsAtWindowsExt};
use std::ffi::OsString;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
const DELETE: u32 = 0x0001_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_SHARE_READ: u32 = 0x0001;
const FILE_SHARE_WRITE: u32 = 0x0002;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_ATTRIBUTE_READONLY: u64 = 0x0001;
const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

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

    pub(super) fn verify_canonical_path(&self, canonical: &Path) -> Result<(), String> {
        let probe = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(canonical)
            .map_err(|error| {
                format!(
                    "sandbox private temp canonical verify open failed at {}: {error}",
                    canonical.display()
                )
            })?;
        verify_handle_identity(&probe, self.root_id, canonical)
    }

    pub(super) fn cleanup(self) -> Result<(), String> {
        verify_handle_identity(&self.root_handle, self.root_id, &self.path)?;
        clean_tree_iterative(self.root_handle, &self.path)
    }
}

fn open_pinned_dir_handle(path: &Path) -> Result<(std::fs::File, WinFileId), String> {
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
            "sandbox private temp handle identity mismatch at {}",
            path.display()
        ));
    }
    Ok(())
}

fn read_dir_names(dir: &mut std::fs::File, label: &Path) -> Result<Vec<OsString>, String> {
    let entries = fs_at::read_dir(dir).map_err(|error| {
        format!(
            "sandbox private temp directory scan failed at {}: {error}",
            label.display()
        )
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "sandbox private temp entry scan failed at {}: {error}",
                label.display()
            )
        })?;
        let name = entry.name();
        if name == "." || name == ".." {
            continue;
        }
        names.push(name.to_os_string());
    }
    Ok(names)
}

#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly_on_handle(handle: &std::fs::File, label: &Path) -> Result<(), String> {
    let mut perms = handle
        .metadata()
        .map_err(|error| {
            format!(
                "sandbox private temp handle metadata failed at {}: {error}",
                label.display()
            )
        })?
        .permissions();
    perms.set_readonly(false);
    handle.set_permissions(perms).map_err(|error| {
        format!(
            "sandbox private temp handle readonly clear failed at {}: {error}",
            label.display()
        )
    })
}

fn open_child_with_access(
    parent: &std::fs::File,
    name: &OsString,
    access: u32,
) -> Result<Option<std::fs::File>, String> {
    let mut opts = fs_at::OpenOptions::default();
    opts.desired_access(access);
    opts.follow(false);
    match opts.open_path_at(parent, name) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "sandbox private temp child handle open failed for {}: {error}",
            Path::new(name).display()
        )),
    }
}

fn open_child_dir_with_access(
    parent: &std::fs::File,
    name: &OsString,
    access: u32,
) -> Result<Option<std::fs::File>, String> {
    let mut opts = fs_at::OpenOptions::default();
    opts.desired_access(access);
    // Pass FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT to NtCreateFile so a
    // non-directory or symlink replacement is rejected atomically before
    // FILE_LIST_DIRECTORY (= FILE_READ_DATA) is ever granted.
    opts.create_options(FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT);
    match opts.open_path_at(parent, name) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "sandbox private temp child directory handle open failed for {}: {error}",
            Path::new(name).display()
        )),
    }
}

fn delete_readonly_single_link_file(
    parent: &std::fs::File,
    name: &OsString,
    expected_id: WinFileId,
    child_label: &Path,
) -> Result<(), String> {
    // Cleanup runs only after child exit is confirmed on a child-owned scratch
    // directory; we keep the verified (volume, index) + nlink == 1 guard before
    // clearing readonly rather than adding workspace unsafe for share_access.
    let Some(writable) = open_child_with_access(
        parent,
        name,
        FILE_WRITE_ATTRIBUTES | DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
    )?
    else {
        return Ok(());
    };
    let info = winapi_util::file::information(&writable).map_err(|error| {
        format!(
            "sandbox private temp file re-verify info failed at {}: {error}",
            child_label.display()
        )
    })?;
    let attrs = info.file_attributes();
    let actual = WinFileId {
        volume: info.volume_serial_number(),
        index: info.file_index(),
    };
    if actual != expected_id
        || (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0
        || info.number_of_links() != 1
    {
        return Err(format!(
            "sandbox private temp file identity or link count changed before readonly clear at {}",
            child_label.display()
        ));
    }
    if (attrs & FILE_ATTRIBUTE_READONLY) != 0 {
        clear_readonly_on_handle(&writable, child_label)?;
    }
    writable.delete_by_handle().map_err(|(_handle, error)| {
        format!(
            "sandbox private temp file handle delete failed at {}: {error}",
            child_label.display()
        )
    })
}

fn clean_tree_iterative(mut root_handle: std::fs::File, root_path: &Path) -> Result<(), String> {
    struct Frame {
        handle: std::fs::File,
        label: PathBuf,
        pending: Vec<OsString>,
    }

    let root_entries = read_dir_names(&mut root_handle, root_path)?;
    let mut stack = vec![Frame {
        handle: root_handle,
        label: root_path.to_path_buf(),
        pending: root_entries,
    }];

    while let Some(frame) = stack.last_mut() {
        if let Some(name) = frame.pending.pop() {
            let child_label = frame.label.join(&name);
            // Open initially with minimal rights only (no FILE_LIST_DIRECTORY,
            // no FILE_WRITE_ATTRIBUTES) before inspecting entry type and link count.
            let Some(child) = open_child_with_access(
                &frame.handle,
                &name,
                DELETE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            )?
            else {
                continue;
            };
            let info = winapi_util::file::information(&child).map_err(|error| {
                format!(
                    "sandbox private temp child handle info failed at {}: {error}",
                    child_label.display()
                )
            })?;
            let attrs = info.file_attributes();
            let id = WinFileId {
                volume: info.volume_serial_number(),
                index: info.file_index(),
            };
            if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                // Treat every reparse point as a leaf and delete on the minimal
                // handle without descending.
                child.delete_by_handle().map_err(|(_handle, error)| {
                    format!(
                        "sandbox private temp reparse handle delete failed at {}: {error}",
                        child_label.display()
                    )
                })?;
            } else if (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                let mut dir_access =
                    FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE;
                if (attrs & FILE_ATTRIBUTE_READONLY) != 0 {
                    dir_access |= FILE_WRITE_ATTRIBUTES;
                }
                let Some(mut dir_handle) =
                    open_child_dir_with_access(&frame.handle, &name, dir_access)?
                else {
                    drop(child);
                    continue;
                };
                verify_handle_identity(&dir_handle, id, &child_label)?;
                drop(child);
                let child_entries = read_dir_names(&mut dir_handle, &child_label)?;
                stack.push(Frame {
                    handle: dir_handle,
                    label: child_label,
                    pending: child_entries,
                });
            } else if (attrs & FILE_ATTRIBUTE_READONLY) != 0 && info.number_of_links() == 1 {
                // Keep `child` open across reopen to pin file index, then drop
                // before delete_by_handle on the verified writable handle.
                let parent_ref = &frame.handle;
                let res = delete_readonly_single_link_file(parent_ref, &name, id, &child_label);
                drop(child);
                res?;
            } else {
                // Ordinary file or multiply-linked file (nlink > 1): delete
                // directly on the minimal handle without FILE_WRITE_ATTRIBUTES
                // so fs_at cannot clear attributes on a hard-linked file.
                child.delete_by_handle().map_err(|(_handle, error)| {
                    format!(
                        "sandbox private temp file handle delete failed at {}: {error}",
                        child_label.display()
                    )
                })?;
            }
            continue;
        }
        let Some(done) = stack.pop() else {
            break;
        };
        let info = winapi_util::file::information(&done.handle).map_err(|error| {
            format!(
                "sandbox private temp directory handle info failed at {}: {error}",
                done.label.display()
            )
        })?;
        if (info.file_attributes() & FILE_ATTRIBUTE_READONLY) != 0 {
            clear_readonly_on_handle(&done.handle, &done.label)?;
        }
        done.handle.delete_by_handle().map_err(|(_handle, error)| {
            format!(
                "sandbox private temp directory handle delete failed at {}: {error}",
                done.label.display()
            )
        })?;
    }
    Ok(())
}
