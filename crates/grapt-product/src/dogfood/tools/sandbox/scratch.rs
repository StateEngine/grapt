//! Per-command private scratch temp directory for Linux and Windows sandboxes
//! when the shared host temp directory is an ancestor of the workspace.
use super::Plan;
use std::path::{Path, PathBuf};

/// Owned private scratch temp directory attached to one sandboxed child.
///
/// `TempDir`'s recursive drop fallback is disarmed immediately at allocation;
/// an OS directory handle is pinned at creation and held until explicit
/// confined cleanup after child termination is confirmed.
pub(super) struct PrivateScratch {
    pinned: Option<PinnedScratch>,
}

impl PrivateScratch {
    pub(super) fn attach(
        plan: &Plan,
        policy: &mut grapt_sandbox::Policy,
        environment: &mut Vec<String>,
    ) -> Result<Option<Self>, String> {
        let Some(parent) = plan.private_temp_parent() else {
            return Ok(None);
        };
        // Disarm TempDir::drop immediately via `.keep()` so neither attach
        // errors nor unwinds can ever trigger recursive pathname remove_dir_all.
        let raw_path = tempfile::Builder::new()
            .prefix("grapt-sandbox-tmp-")
            .tempdir_in(parent)
            .map_err(|error| format!("sandbox private temp directory unavailable: {error}"))?
            .keep();
        let pinned = match PinnedScratch::pin(parent, raw_path.clone()) {
            Ok(pinned) => pinned,
            Err(reason) => {
                let cleanup_err = std::fs::remove_dir(&raw_path)
                    .err()
                    .map(|error| format!("sandbox private temp root rmdir failed: {error}"));
                return Err(combine_errors(reason, [cleanup_err]));
            }
        };
        let canonical_private = match std::fs::canonicalize(pinned.path()) {
            Ok(path) => path,
            Err(error) => {
                let cleanup_err = pinned.cleanup().err();
                return Err(combine_errors(
                    format!("sandbox private temp directory unavailable: {error}"),
                    [cleanup_err],
                ));
            }
        };
        let Some(text) = canonical_private.to_str() else {
            let cleanup_err = pinned.cleanup().err();
            return Err(combine_errors(
                "sandbox private temp directory is not Unicode".to_owned(),
                [cleanup_err],
            ));
        };
        environment.retain(|entry| {
            !entry.split_once('=').is_some_and(|(name, _)| {
                name.eq_ignore_ascii_case("TMPDIR")
                    || name.eq_ignore_ascii_case("TEMP")
                    || name.eq_ignore_ascii_case("TMP")
            })
        });
        environment.push(format!("TMPDIR={text}"));
        environment.push(format!("TEMP={text}"));
        environment.push(format!("TMP={text}"));
        policy.writable_roots.push(canonical_private);
        Ok(Some(Self {
            pinned: Some(pinned),
        }))
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> Option<&Path> {
        self.pinned.as_ref().map(PinnedScratch::path)
    }

    pub(super) fn cleanup(mut self) -> Result<(), String> {
        let Some(pinned) = self.pinned.take() else {
            return Ok(());
        };
        pinned.cleanup()
    }

    /// Release the pinned handles without walking or removing scratch when
    /// child termination could not be confirmed.
    pub(super) fn abandon_unreaped(mut self) {
        let _ = self.pinned.take();
    }
}

/// Run confined scratch cleanup only when the child is confirmed stopped;
/// otherwise release handles without walking scratch and return an explicit
/// error so callers preserve it alongside the primary failure.
pub(super) fn cleanup_after_stop(
    scratch: Option<PrivateScratch>,
    child_stopped: bool,
) -> Option<String> {
    let scratch = scratch?;
    if child_stopped {
        scratch.cleanup().err()
    } else {
        scratch.abandon_unreaped();
        Some(
            "sandbox private temp cleanup skipped because child termination was not confirmed"
                .to_owned(),
        )
    }
}

/// Finalize a completed child's exit code alongside scratch cleanup without
/// overwriting a child's nonzero exit code with generic status 1.
pub(super) fn finish_with_exit_code(
    code: i32,
    cleanup: Result<(), String>,
) -> (i32, Option<String>) {
    match cleanup {
        Ok(()) => (code, None),
        Err(error) if code == 0 => (1, Some(error)),
        Err(error) => (code, Some(error)),
    }
}

/// Preserve secondary kill/reap/receipt/cleanup errors alongside the primary
/// launch or observation error instead of discarding them.
pub(super) fn combine_errors<const N: usize>(
    primary: String,
    extras: [Option<String>; N],
) -> String {
    let mut combined = primary;
    for extra in extras.into_iter().flatten() {
        if !extra.is_empty() {
            combined.push_str("; ");
            combined.push_str(&extra);
        }
    }
    combined
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct DirIdentity {
    dev: nix::libc::dev_t,
    ino: nix::libc::ino_t,
}

#[cfg(unix)]
fn parent_dir_search_flag() -> nix::fcntl::OFlag {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        nix::fcntl::OFlag::O_PATH
    }
    #[cfg(target_vendor = "apple")]
    {
        nix::fcntl::OFlag::from_bits_retain(nix::libc::O_SEARCH)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        nix::fcntl::OFlag::O_RDONLY
    }
}

#[cfg(unix)]
struct PinnedScratch {
    parent_fd: std::os::fd::OwnedFd,
    root_name: std::ffi::CString,
    root_fd: std::os::fd::OwnedFd,
    root_id: DirIdentity,
    path: PathBuf,
}

#[cfg(unix)]
impl PinnedScratch {
    fn pin(parent: &Path, path: PathBuf) -> Result<Self, String> {
        use nix::fcntl::{open, openat, OFlag};
        use nix::sys::stat::{fstat, Mode};
        use nix::unistd::geteuid;
        use std::ffi::CString;
        use std::os::fd::AsFd as _;
        use std::os::unix::ffi::OsStrExt as _;

        let raw_name = path
            .file_name()
            .ok_or_else(|| format!("sandbox private temp has no file name: {}", path.display()))?;
        let root_name = CString::new(raw_name.as_bytes())
            .map_err(|_| format!("sandbox private temp name contains NUL: {}", path.display()))?;
        // Open parent with O_PATH (Linux) / O_SEARCH (macOS) so write+search-only
        // (0300/1733) temp parents work without requiring read permission.
        let parent_flags =
            parent_dir_search_flag() | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let parent_fd = open(parent, parent_flags, Mode::empty()).map_err(|error| {
            format!(
                "sandbox private temp parent open failed at {}: {error}",
                parent.display()
            )
        })?;
        let root_flags =
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let root_fd = openat(
            parent_fd.as_fd(),
            root_name.as_c_str(),
            root_flags,
            Mode::empty(),
        )
        .map_err(|error| {
            format!(
                "sandbox private temp root openat failed at {}: {error}",
                path.display()
            )
        })?;
        let st = fstat(root_fd.as_fd()).map_err(|error| {
            format!(
                "sandbox private temp root fstat failed at {}: {error}",
                path.display()
            )
        })?;
        if !is_directory_mode(st.st_mode) || st.st_uid != geteuid().as_raw() {
            return Err(format!(
                "sandbox private temp root failed identity check: {}",
                path.display()
            ));
        }
        Ok(Self {
            parent_fd,
            root_name,
            root_fd,
            root_id: DirIdentity {
                dev: st.st_dev,
                ino: st.st_ino,
            },
            path,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(self) -> Result<(), String> {
        use std::os::fd::AsFd as _;

        clean_tree_from_root_fd(self.root_fd, self.root_id)?;
        verify_and_rmdir_under(
            self.parent_fd.as_fd(),
            self.root_name.as_c_str(),
            self.root_id,
        )
    }
}

#[cfg(unix)]
fn verify_and_rmdir_under(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
    expected: DirIdentity,
) -> Result<(), String> {
    use nix::errno::Errno;
    use nix::fcntl::AtFlags;
    use nix::sys::stat::fstatat;
    use nix::unistd::{unlinkat, UnlinkatFlags};

    let st = match fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::ENOENT) => return Ok(()),
        Err(error) => {
            return Err(format!(
                "sandbox private temp pre-rmdir stat failed for {:?}: {error}",
                name.to_string_lossy()
            ));
        }
    };
    if !is_directory_mode(st.st_mode) || st.st_dev != expected.dev || st.st_ino != expected.ino {
        return Err(format!(
            "sandbox private temp directory identity changed before removal for {:?}",
            name.to_string_lossy()
        ));
    }
    match unlinkat(parent, name, UnlinkatFlags::RemoveDir) {
        Ok(()) | Err(Errno::ENOENT) => Ok(()),
        Err(error) => Err(format!(
            "sandbox private temp rmdir failed for {:?}: {error}",
            name.to_string_lossy()
        )),
    }
}

#[cfg(unix)]
fn is_directory_mode(mode: nix::sys::stat::mode_t) -> bool {
    use nix::sys::stat::SFlag;
    (SFlag::from_bits_truncate(mode) & SFlag::S_IFMT) == SFlag::S_IFDIR
}

#[cfg(unix)]
fn clean_tree_from_root_fd(
    root_fd: std::os::fd::OwnedFd,
    root_id: DirIdentity,
) -> Result<(), String> {
    use nix::dir::Dir;
    use nix::errno::Errno;
    use nix::fcntl::AtFlags;
    use nix::sys::stat::fstatat;
    use nix::unistd::{unlinkat, UnlinkatFlags};
    use std::ffi::CString;
    use std::os::fd::AsFd as _;

    struct Frame {
        dir: Dir,
        id: DirIdentity,
        pending: Vec<CString>,
        parent_entry_name: Option<CString>,
    }

    let (root_dir, actual_id, root_entries) = relax_dir_fd_and_read(root_fd)?;
    if actual_id != root_id {
        return Err("sandbox private temp root identity changed before cleanup".to_owned());
    }
    let mut stack = vec![Frame {
        dir: root_dir,
        id: root_id,
        pending: root_entries,
        parent_entry_name: None,
    }];

    while let Some(frame) = stack.last_mut() {
        if let Some(name) = frame.pending.pop() {
            let st = match fstatat(
                frame.dir.as_fd(),
                name.as_c_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(st) => st,
                Err(Errno::ENOENT) => continue,
                Err(error) => {
                    return Err(format!(
                        "sandbox private temp stat failed for {:?}: {error}",
                        name.to_string_lossy()
                    ));
                }
            };
            if is_directory_mode(st.st_mode) {
                let (child_dir, child_id, child_entries) =
                    open_confined_child_dir(frame.dir.as_fd(), name.as_c_str())?;
                stack.push(Frame {
                    dir: child_dir,
                    id: child_id,
                    pending: child_entries,
                    parent_entry_name: Some(name),
                });
            } else {
                // Never chmod regular files, symlinks, hardlinks, or sockets:
                // unlinkat only requires write+exec on the parent directory fd.
                match unlinkat(
                    frame.dir.as_fd(),
                    name.as_c_str(),
                    UnlinkatFlags::NoRemoveDir,
                ) {
                    Ok(()) | Err(Errno::ENOENT) => {}
                    Err(error) => {
                        return Err(format!(
                            "sandbox private temp unlink failed for {:?}: {error}",
                            name.to_string_lossy()
                        ));
                    }
                }
            }
            continue;
        }
        let Some(done) = stack.pop() else {
            break;
        };
        let done_id = done.id;
        drop(done.dir);
        if let Some(name) = done.parent_entry_name {
            if let Some(parent) = stack.last() {
                verify_and_rmdir_under(parent.dir.as_fd(), name.as_c_str(), done_id)?;
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn relax_owned_opath_dir_and_reopen(
    path_fd: std::os::fd::OwnedFd,
    open_flags: nix::fcntl::OFlag,
    label: &str,
) -> Result<std::os::fd::OwnedFd, String> {
    use nix::fcntl::open;
    use nix::sys::stat::{fstat, Mode};
    use nix::unistd::geteuid;
    use std::os::fd::{AsFd as _, AsRawFd as _};

    let st = fstat(path_fd.as_fd())
        .map_err(|error| format!("sandbox private temp fstat failed for {label}: {error}"))?;
    if !is_directory_mode(st.st_mode) {
        return Err(format!(
            "sandbox private temp target is not a real directory: {label}"
        ));
    }
    if st.st_uid != geteuid().as_raw() {
        return Err(format!(
            "sandbox private temp directory is not owned by this user: {label}"
        ));
    }
    // Pin chmod and reopen to the already-opened O_PATH directory descriptor via
    // /proc/self/fd/<fd> rather than any child-controlled pathname. Strip
    // O_NOFOLLOW because /proc/self/fd/<fd> is itself a kernel magic symlink.
    let proc_fd = format!("/proc/self/fd/{}", path_fd.as_raw_fd());
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&proc_fd, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("sandbox private temp fd chmod failed for {label}: {error}"))?;
    open(
        proc_fd.as_str(),
        open_flags - nix::fcntl::OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| format!("sandbox private temp fd reopen failed for {label}: {error}"))
}

#[cfg(unix)]
fn open_confined_child_dir(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
) -> Result<(nix::dir::Dir, DirIdentity, Vec<std::ffi::CString>), String> {
    use nix::errno::Errno;
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::Mode;
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let fd = match openat(parent, name, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::EACCES) => reopen_unreadable_child_dir(parent, name, flags)?,
        Err(error) => {
            return Err(format!(
                "sandbox private temp directory open failed for {:?}: {error}",
                name.to_string_lossy()
            ));
        }
    };
    relax_dir_fd_and_read(fd)
}

#[cfg(target_os = "linux")]
fn reopen_unreadable_child_dir(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
    open_flags: nix::fcntl::OFlag,
) -> Result<std::os::fd::OwnedFd, String> {
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::Mode;
    let path_flags = OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let path_fd = openat(parent, name, path_flags, Mode::empty()).map_err(|error| {
        format!(
            "sandbox private temp directory O_PATH open failed for {:?}: {error}",
            name.to_string_lossy()
        )
    })?;
    relax_owned_opath_dir_and_reopen(path_fd, open_flags, &name.to_string_lossy())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn reopen_unreadable_child_dir(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
    open_flags: nix::fcntl::OFlag,
) -> Result<std::os::fd::OwnedFd, String> {
    use nix::fcntl::openat;
    use nix::sys::stat::{fchmodat, FchmodatFlags, Mode};
    fchmodat(parent, name, Mode::S_IRWXU, FchmodatFlags::NoFollowSymlink).map_err(|error| {
        format!(
            "sandbox private temp directory permission relax failed for {:?}: {error}",
            name.to_string_lossy()
        )
    })?;
    openat(parent, name, open_flags, Mode::empty()).map_err(|error| {
        format!(
            "sandbox private temp directory open failed for {:?}: {error}",
            name.to_string_lossy()
        )
    })
}

#[cfg(unix)]
fn relax_dir_fd_and_read(
    fd: std::os::fd::OwnedFd,
) -> Result<(nix::dir::Dir, DirIdentity, Vec<std::ffi::CString>), String> {
    use nix::dir::Dir;
    use nix::sys::stat::{fchmod, fstat, Mode};
    use std::ffi::CString;
    use std::os::fd::AsFd as _;
    let st =
        fstat(fd.as_fd()).map_err(|error| format!("sandbox private temp fstat failed: {error}"))?;
    let id = DirIdentity {
        dev: st.st_dev,
        ino: st.st_ino,
    };
    let required = Mode::S_IRWXU;
    let current = Mode::from_bits_truncate(st.st_mode);
    if !current.contains(required) {
        fchmod(fd.as_fd(), current | required)
            .map_err(|error| format!("sandbox private temp fchmod failed: {error}"))?;
    }
    let mut dir = Dir::from_fd(fd)
        .map_err(|error| format!("sandbox private temp fdopendir failed: {error}"))?;
    let mut names = Vec::new();
    for entry in dir.iter() {
        let entry = entry.map_err(|error| {
            format!("sandbox private temp directory entry scan failed: {error}")
        })?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        names.push(CString::from(name));
    }
    Ok((dir, id, names))
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct WinFileId {
    volume: u64,
    index: u64,
}

#[cfg(windows)]
struct PinnedScratch {
    root_handle: std::fs::File,
    root_id: WinFileId,
    path: PathBuf,
}

#[cfg(windows)]
impl PinnedScratch {
    fn pin(_parent: &Path, path: PathBuf) -> Result<Self, String> {
        let (root_handle, root_id) = open_pinned_dir_handle(&path, false)?;
        Ok(Self {
            root_handle,
            root_id,
            path,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(self) -> Result<(), String> {
        clean_tree_from_pinned_root(self.root_handle, self.root_id, &self.path)?;
        delete_verified_windows_dir(&self.path, self.root_id)
    }
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn open_pinned_dir_handle(
    path: &Path,
    delete_on_close: bool,
) -> Result<(std::fs::File, WinFileId), String> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    const FILE_ATTRIBUTE_READONLY: u64 = 0x0001;
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    // Request FILE_LIST_DIRECTORY (non-attribute access) so Windows enforces
    // share_mode without FILE_SHARE_DELETE, pinning the directory name against
    // rename or reparse replacement while this handle is held.
    let mut access = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES;
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
    if delete_on_close {
        access |= DELETE;
        flags |= FILE_FLAG_DELETE_ON_CLOSE;
    }
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
    if (attrs & FILE_ATTRIBUTE_READONLY) != 0 {
        let mut perms = file
            .metadata()
            .map_err(|error| {
                format!(
                    "sandbox private temp directory metadata failed at {}: {error}",
                    path.display()
                )
            })?
            .permissions();
        perms.set_readonly(false);
        file.set_permissions(perms).map_err(|error| {
            format!(
                "sandbox private temp directory permission relax failed at {}: {error}",
                path.display()
            )
        })?;
    }
    let id = WinFileId {
        volume: info.volume_serial_number(),
        index: info.file_index(),
    };
    Ok((file, id))
}

#[cfg(windows)]
fn delete_verified_windows_dir(path: &Path, expected: WinFileId) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_LIST_DIRECTORY: u32 = 0x0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const DELETE: u32 = 0x0001_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    let file = match std::fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "sandbox private temp root verify open failed at {}: {error}",
                path.display()
            ));
        }
    };
    let info = winapi_util::file::information(&file).map_err(|error| {
        format!(
            "sandbox private temp root verify info failed at {}: {error}",
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
            "sandbox private temp root identity changed before removal: {}",
            path.display()
        ));
    }
    drop(file);
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("sandbox private temp root rmdir failed: {error}")),
    }
}

#[cfg(windows)]
fn read_dir_entries(path: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(path).map_err(|error| {
        format!(
            "sandbox private temp directory scan failed at {}: {error}",
            path.display()
        )
    })?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "sandbox private temp entry scan failed at {}: {error}",
                path.display()
            )
        })?;
        children.push(entry.path());
    }
    Ok(children)
}

#[cfg(windows)]
fn clean_tree_from_pinned_root(
    root_handle: std::fs::File,
    root_id: WinFileId,
    root: &Path,
) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

    struct Frame {
        // Pinned with FILE_LIST_DIRECTORY and no FILE_SHARE_DELETE across
        // enumeration and child deletion; child frames also carry
        // FILE_FLAG_DELETE_ON_CLOSE so dropping the emptied handle deletes it.
        _handle: std::fs::File,
        _id: WinFileId,
        pending: Vec<PathBuf>,
    }

    let root_entries = read_dir_entries(root)?;
    let mut stack = vec![Frame {
        _handle: root_handle,
        _id: root_id,
        pending: root_entries,
    }];

    while let Some(frame) = stack.last_mut() {
        if let Some(child_path) = frame.pending.pop() {
            let meta = match std::fs::symlink_metadata(&child_path) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "sandbox private temp metadata failed at {}: {error}",
                        child_path.display()
                    ));
                }
            };
            let attrs = meta.file_attributes();
            let is_reparse =
                meta.file_type().is_symlink() || (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
            if is_reparse {
                delete_windows_reparse_handle(
                    &child_path,
                    (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0,
                )?;
            } else if meta.is_dir() {
                let (handle, id) = open_pinned_dir_handle(&child_path, true)?;
                let children = read_dir_entries(&child_path)?;
                stack.push(Frame {
                    _handle: handle,
                    _id: id,
                    pending: children,
                });
            } else {
                remove_windows_file(&child_path)?;
            }
            continue;
        }
        let Some(done) = stack.pop() else {
            break;
        };
        // For child directories, `_handle` carries FILE_FLAG_DELETE_ON_CLOSE so
        // dropping it after all children are gone deletes the verified handle.
        drop(done._handle);
    }
    Ok(())
}

#[cfg(windows)]
fn delete_windows_reparse_handle(path: &Path, is_dir: bool) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const DELETE: u32 = 0x0001_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_DELETE_ON_CLOSE;
    if is_dir {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    let file = match std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | DELETE)
        .share_mode(0)
        .custom_flags(flags)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "sandbox private temp reparse handle open failed at {}: {error}",
                path.display()
            ));
        }
    };
    let info = winapi_util::file::information(&file).map_err(|error| {
        format!(
            "sandbox private temp reparse handle info failed at {}: {error}",
            path.display()
        )
    })?;
    if (info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) == 0 {
        return Err(format!(
            "sandbox private temp entry ceased to be a reparse point at {}",
            path.display()
        ));
    }
    drop(file);
    Ok(())
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn remove_windows_file(path: &Path) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_READ_DATA: u32 = 0x0001;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const DELETE: u32 = 0x0001_0000;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    const FILE_ATTRIBUTE_READONLY: u64 = 0x0001;
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    // Fast path: open exclusive handle with FILE_FLAG_OPEN_REPARSE_POINT and
    // FILE_FLAG_DELETE_ON_CLOSE so deletion occurs on the verified handle.
    match std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
    {
        Ok(file) => {
            drop(file);
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
        Err(error) => {
            return Err(format!(
                "sandbox private temp file delete-on-close open failed at {}: {error}",
                path.display()
            ));
        }
    }

    // Read-only fallback: open pinned handle (FILE_READ_DATA, no FILE_SHARE_DELETE),
    // verify non-reparse, non-directory, and nNumberOfLinks == 1, clear readonly
    // on the open handle, then delete via handle.
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|open_err| {
            format!(
                "sandbox private temp file handle open failed at {}: {open_err}",
                path.display()
            )
        })?;
    let info = winapi_util::file::information(&file).map_err(|info_err| {
        format!(
            "sandbox private temp file handle info failed at {}: {info_err}",
            path.display()
        )
    })?;
    let attrs = info.file_attributes();
    if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0 || (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0 {
        return Err(format!(
            "sandbox private temp file changed to reparse point or directory: {}",
            path.display()
        ));
    }
    if info.number_of_links() != 1 {
        return Err(format!(
            "sandbox private temp refused to clear readonly on hard-linked file at {}",
            path.display()
        ));
    }
    if (attrs & FILE_ATTRIBUTE_READONLY) != 0 {
        let mut perms = file
            .metadata()
            .map_err(|meta_err| {
                format!(
                    "sandbox private temp file metadata failed at {}: {meta_err}",
                    path.display()
                )
            })?
            .permissions();
        perms.set_readonly(false);
        file.set_permissions(perms).map_err(|perm_err| {
            format!(
                "sandbox private temp permission relax failed at {}: {perm_err}",
                path.display()
            )
        })?;
    }
    drop(file);
    let delete_handle = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
        .map_err(|retry| {
            format!(
                "sandbox private temp unlink failed at {}: {retry}",
                path.display()
            )
        })?;
    drop(delete_handle);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
struct PinnedScratch {
    path: PathBuf,
}

#[cfg(not(any(unix, windows)))]
impl PinnedScratch {
    fn pin(_parent: &Path, path: PathBuf) -> Result<Self, String> {
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(self) -> Result<(), String> {
        std::fs::remove_dir(&self.path)
            .map_err(|error| format!("sandbox private temp root rmdir failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_plan_and_scratch(
        parent: &Path,
    ) -> Result<(PrivateScratch, PathBuf, grapt_sandbox::Policy, Vec<String>), String> {
        let canonical_parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
        let workspace = canonical_parent.join("workspace");
        std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
        let plan = Plan {
            git_metadata_read_only: false,
            readable: vec![PathBuf::from("/")],
            writable: vec![workspace],
            protected: Vec::new(),
            denied: Vec::new(),
            network: false,
            private_temp_parent: Some(canonical_parent),
        };
        let mut policy = plan.policy();
        let mut env = vec![format!("TMPDIR={}", parent.display())];
        let scratch = PrivateScratch::attach(&plan, &mut policy, &mut env)?
            .ok_or("expected private scratch from planner decision")?;
        let scratch_path = std::fs::canonicalize(scratch.path().ok_or("missing scratch path")?)
            .map_err(|e| e.to_string())?;
        Ok((scratch, scratch_path, policy, env))
    }

    #[test]
    fn private_scratch_honors_planner_parent_and_cleans_readonly_subdir() -> Result<(), String> {
        let configured_temp = tempfile::Builder::new()
            .prefix("grapt-configured-tmpdir-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let canonical_parent =
            std::fs::canonicalize(configured_temp.path()).map_err(|e| e.to_string())?;
        let (scratch, scratch_path, policy, env) = test_plan_and_scratch(configured_temp.path())?;
        assert!(scratch_path.starts_with(&canonical_parent));
        assert!(policy.writable_roots.contains(&scratch_path));
        assert!(!policy.writable_roots.contains(&canonical_parent));
        assert!(env.contains(&format!("TMPDIR={}", scratch_path.display())));
        assert!(env.contains(&format!("TEMP={}", scratch_path.display())));
        assert!(env.contains(&format!("TMP={}", scratch_path.display())));

        // Create read-only (0500) and mode-000 subdirectories plus a read-only
        // file inside scratch, and even strip permissions from scratch root
        // itself — pinned root_fd must still relax and remove everything.
        let locked_dir = scratch_path.join("locked");
        let zero_dir = scratch_path.join("zero");
        std::fs::create_dir(&locked_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir(&zero_dir).map_err(|e| e.to_string())?;
        std::fs::write(locked_dir.join("artifact.txt"), "data").map_err(|e| e.to_string())?;
        std::fs::write(zero_dir.join("secret.txt"), "data").map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                locked_dir.join("artifact.txt"),
                std::fs::Permissions::from_mode(0o400),
            )
            .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o500))
                .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&zero_dir, std::fs::Permissions::from_mode(0o000))
                .map_err(|e| e.to_string())?;
            std::fs::set_permissions(&scratch_path, std::fs::Permissions::from_mode(0o000))
                .map_err(|e| e.to_string())?;
        }
        scratch.cleanup()?;
        assert!(
            !scratch_path.exists(),
            "private scratch directory survived cleanup: {}",
            scratch_path.display()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn write_search_only_0300_temp_parent_attaches_and_cleans_up() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt as _;

        let outer = tempfile::Builder::new()
            .prefix("grapt-scratch-0300-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let parent = outer.path().join("wx-only");
        std::fs::create_dir(&parent).map_err(|e| e.to_string())?;
        let workspace = parent.join("ws");
        std::fs::create_dir(&workspace).map_err(|e| e.to_string())?;
        // Strip read bit from parent directory (0300: write + search only).
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o300))
            .map_err(|e| e.to_string())?;

        let result = (|| -> Result<(), String> {
            let plan = Plan {
                git_metadata_read_only: false,
                readable: vec![PathBuf::from("/")],
                writable: vec![workspace.clone()],
                protected: Vec::new(),
                denied: Vec::new(),
                network: false,
                private_temp_parent: Some(parent.clone()),
            };
            let mut policy = plan.policy();
            let mut env = Vec::new();
            let scratch = PrivateScratch::attach(&plan, &mut policy, &mut env)?
                .ok_or("expected scratch under 0300 parent")?;
            let scratch_path = scratch.path().ok_or("missing scratch path")?.to_path_buf();
            std::fs::write(scratch_path.join("probe.txt"), "ok").map_err(|e| e.to_string())?;
            scratch.cleanup()?;
            assert!(!scratch_path.exists());
            Ok(())
        })();

        // Restore permissions so `outer` TempDir cleans up cleanly.
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700));
        result
    }

    #[cfg(unix)]
    #[test]
    fn confined_cleanup_unlinks_symlinks_hardlinks_and_unix_sockets_safely() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = tempfile::Builder::new()
            .prefix("gs-")
            .tempdir_in("/tmp")
            .map_err(|e| e.to_string())?;
        let outside = parent.path().join("outside");
        std::fs::create_dir(&outside).map_err(|e| e.to_string())?;
        let outside_file = outside.join("keep.txt");
        std::fs::write(&outside_file, "immutable").map_err(|e| e.to_string())?;
        std::fs::set_permissions(&outside_file, std::fs::Permissions::from_mode(0o400))
            .map_err(|e| e.to_string())?;

        let (scratch, scratch_path, _, _) = test_plan_and_scratch(parent.path())?;
        std::os::unix::fs::symlink(&outside, scratch_path.join("link-dir"))
            .map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(&outside_file, scratch_path.join("link-file"))
            .map_err(|e| e.to_string())?;
        std::fs::hard_link(&outside_file, scratch_path.join("hard-file"))
            .map_err(|e| e.to_string())?;
        let sock_path = scratch_path.join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock_path)
            .map_err(|e| format!("bind unix socket: {e}"))?;
        drop(listener);

        scratch.cleanup()?;
        assert!(!scratch_path.exists());
        assert!(outside.is_dir());
        assert_eq!(
            std::fs::read_to_string(&outside_file).map_err(|e| e.to_string())?,
            "immutable"
        );
        let mode = std::fs::metadata(&outside_file)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o400,
            "hardlink cleanup must never chmod the shared target inode"
        );
        Ok(())
    }

    #[test]
    fn cleanup_after_stop_and_exit_code_preservation() -> Result<(), String> {
        let parent = tempfile::Builder::new()
            .prefix("grapt-scratch-unreaped-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let (scratch, scratch_path, _, _) = test_plan_and_scratch(parent.path())?;
        let skipped = cleanup_after_stop(Some(scratch), false);
        assert!(
            skipped
                .as_deref()
                .is_some_and(|msg| msg.contains("child termination was not confirmed")),
            "expected explicit skip error, got {skipped:?}"
        );
        assert!(
            scratch_path.exists(),
            "unreaped cleanup must leave scratch untouched instead of walking against a live child"
        );
        let combined = combine_errors(
            "sandbox receipt was not delivered".to_owned(),
            [Some("sandbox reap failed: ECHILD".to_owned()), skipped],
        );
        assert!(combined.contains("sandbox receipt was not delivered"));
        assert!(combined.contains("sandbox reap failed: ECHILD"));
        assert!(combined.contains("child termination was not confirmed"));

        assert_eq!(finish_with_exit_code(0, Ok(())), (0, None));
        assert_eq!(finish_with_exit_code(42, Ok(())), (42, None));
        assert_eq!(
            finish_with_exit_code(0, Err("cleanup boom".to_owned())),
            (1, Some("cleanup boom".to_owned()))
        );
        assert_eq!(
            finish_with_exit_code(42, Err("cleanup boom".to_owned())),
            (42, Some("cleanup boom".to_owned()))
        );

        let _ = std::fs::remove_dir_all(&scratch_path);
        Ok(())
    }
}
