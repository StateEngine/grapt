//! Windows confined scratch allocation and iterative handle-only cleanup.
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
//! directory handles:
//! - Every reparse point (`FILE_ATTRIBUTE_REPARSE_POINT`) is treated as a leaf
//!   and deleted via `delete_by_handle()` without descending.
//! - Real subdirectories are pushed onto the iterative stack (zero recursion),
//!   emptied, have `FILE_ATTRIBUTE_READONLY` cleared on that same handle if
//!   set, and are deleted via `delete_by_handle()`.
//! - Regular files with `FILE_ATTRIBUTE_READONLY` set and `nNumberOfLinks == 1`
//!   have readonly cleared on that same handle before `delete_by_handle()`.
//! - Finally, the verified root handle itself is deleted via
//!   `delete_by_handle()`, with zero `FILE_FLAG_DELETE_ON_CLOSE` and zero
//!   handle-drop-and-reopen-by-name.

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
            "sandbox private temp root handle identity changed before cleanup: {}",
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

fn open_child_handle(
    parent: &std::fs::File,
    name: &OsString,
) -> Result<Option<std::fs::File>, String> {
    let mut opts = fs_at::OpenOptions::default();
    opts.desired_access(
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE | SYNCHRONIZE,
    );
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
            let Some(mut child) = open_child_handle(&frame.handle, &name)? else {
                continue;
            };
            let info = winapi_util::file::information(&child).map_err(|error| {
                format!(
                    "sandbox private temp child handle info failed at {}: {error}",
                    child_label.display()
                )
            })?;
            let attrs = info.file_attributes();
            if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                // Treat every reparse point (symlink, junction, mount point) as
                // a leaf and delete it on its verified handle without descending.
                child.delete_by_handle().map_err(|(_handle, error)| {
                    format!(
                        "sandbox private temp reparse handle delete failed at {}: {error}",
                        child_label.display()
                    )
                })?;
            } else if (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                let child_entries = read_dir_names(&mut child, &child_label)?;
                stack.push(Frame {
                    handle: child,
                    label: child_label,
                    pending: child_entries,
                });
            } else {
                if (attrs & FILE_ATTRIBUTE_READONLY) != 0 && info.number_of_links() == 1 {
                    clear_readonly_on_handle(&child, &child_label)?;
                }
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
