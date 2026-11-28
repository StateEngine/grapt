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
struct PinnedScratch {
    parent_fd: std::os::fd::OwnedFd,
    root_name: std::ffi::CString,
    root_fd: std::os::fd::OwnedFd,
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
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let parent_fd = open(parent, flags, Mode::empty()).map_err(|error| {
            format!(
                "sandbox private temp parent open failed at {}: {error}",
                parent.display()
            )
        })?;
        let root_fd = openat(
            parent_fd.as_fd(),
            root_name.as_c_str(),
            flags,
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
            path,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(self) -> Result<(), String> {
        use nix::errno::Errno;
        use nix::unistd::{unlinkat, UnlinkatFlags};
        use std::os::fd::AsFd as _;

        clean_tree_from_root_fd(self.root_fd)?;
        match unlinkat(
            self.parent_fd.as_fd(),
            self.root_name.as_c_str(),
            UnlinkatFlags::RemoveDir,
        ) {
            Ok(()) | Err(Errno::ENOENT) => Ok(()),
            Err(error) => Err(format!("sandbox private temp root rmdir failed: {error}")),
        }
    }
}

#[cfg(unix)]
fn is_directory_mode(mode: nix::sys::stat::mode_t) -> bool {
    use nix::sys::stat::SFlag;
    (SFlag::from_bits_truncate(mode) & SFlag::S_IFMT) == SFlag::S_IFDIR
}

#[cfg(unix)]
fn clean_tree_from_root_fd(root_fd: std::os::fd::OwnedFd) -> Result<(), String> {
    use nix::dir::Dir;
    use nix::errno::Errno;
    use nix::fcntl::AtFlags;
    use nix::sys::stat::fstatat;
    use nix::unistd::{unlinkat, UnlinkatFlags};
    use std::ffi::CString;
    use std::os::fd::AsFd as _;

    struct Frame {
        dir: Dir,
        pending: Vec<CString>,
        parent_entry_name: Option<CString>,
    }

    let (root_dir, root_entries) = relax_dir_fd_and_read(root_fd)?;
    let mut stack = vec![Frame {
        dir: root_dir,
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
                let (child_dir, child_entries) =
                    open_confined_child_dir(frame.dir.as_fd(), name.as_c_str())?;
                stack.push(Frame {
                    dir: child_dir,
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
        drop(done.dir);
        if let Some(name) = done.parent_entry_name {
            if let Some(parent) = stack.last() {
                match unlinkat(
                    parent.dir.as_fd(),
                    name.as_c_str(),
                    UnlinkatFlags::RemoveDir,
                ) {
                    Ok(()) | Err(Errno::ENOENT) => {}
                    Err(error) => {
                        return Err(format!(
                            "sandbox private temp rmdir failed for {:?}: {error}",
                            name.to_string_lossy()
                        ));
                    }
                }
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
) -> Result<(nix::dir::Dir, Vec<std::ffi::CString>), String> {
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
) -> Result<(nix::dir::Dir, Vec<std::ffi::CString>), String> {
    use nix::dir::Dir;
    use nix::sys::stat::{fchmod, fstat, Mode};
    use std::ffi::CString;
    use std::os::fd::AsFd as _;
    let st =
        fstat(fd.as_fd()).map_err(|error| format!("sandbox private temp fstat failed: {error}"))?;
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
    Ok((dir, names))
}

#[cfg(windows)]
struct PinnedScratch {
    root_handle: std::fs::File,
    path: PathBuf,
}

#[cfg(windows)]
impl PinnedScratch {
    fn pin(_parent: &Path, path: PathBuf) -> Result<Self, String> {
        let root_handle = open_pinned_dir_handle(&path)?;
        Ok(Self { root_handle, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn cleanup(self) -> Result<(), String> {
        clean_tree_from_pinned_root(self.root_handle, &self.path)?;
        match std::fs::remove_dir(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("sandbox private temp root rmdir failed: {error}")),
        }
    }
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn open_pinned_dir_handle(path: &Path) -> Result<std::fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_READONLY: u64 = 0x0001;
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    // Omit FILE_SHARE_DELETE so this directory cannot be renamed or replaced
    // with a reparse point while this handle remains pinned.
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
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
    Ok(file)
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
fn clean_tree_from_pinned_root(root_handle: std::fs::File, root: &Path) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

    struct Frame {
        // Pinned with no FILE_SHARE_DELETE across enumeration and child deletion.
        _handle: std::fs::File,
        path: PathBuf,
        pending: Vec<PathBuf>,
        remove_on_exit: bool,
    }

    let root_entries = read_dir_entries(root)?;
    let mut stack = vec![Frame {
        _handle: root_handle,
        path: root.to_path_buf(),
        pending: root_entries,
        remove_on_exit: false,
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
                let remove = if (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                    std::fs::remove_dir(&child_path)
                } else {
                    std::fs::remove_file(&child_path)
                };
                match remove {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "sandbox private temp reparse unlink failed at {}: {error}",
                            child_path.display()
                        ));
                    }
                }
            } else if meta.is_dir() {
                let handle = open_pinned_dir_handle(&child_path)?;
                let children = read_dir_entries(&child_path)?;
                stack.push(Frame {
                    _handle: handle,
                    path: child_path,
                    pending: children,
                    remove_on_exit: true,
                });
            } else {
                remove_windows_file(&child_path)?;
            }
            continue;
        }
        let Some(done) = stack.pop() else {
            break;
        };
        let remove = done.remove_on_exit;
        let path = done.path;
        drop(done._handle);
        if remove {
            match std::fs::remove_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "sandbox private temp rmdir failed at {}: {error}",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn remove_windows_file(path: &Path) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_SHARE_READ: u32 = 0x0001;
    const FILE_SHARE_WRITE: u32 = 0x0002;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_READONLY: u64 = 0x0001;
    const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u64 = 0x0400;

    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            // Open without following reparse points and mutate FILE_ATTRIBUTE_READONLY
            // strictly on the open handle ONLY when nNumberOfLinks == 1.
            let file = std::fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
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
            if (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0
                || (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0
            {
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
            std::fs::remove_file(path).map_err(|retry| {
                format!(
                    "sandbox private temp unlink failed at {}: {retry}",
                    path.display()
                )
            })
        }
        Err(error) => Err(format!(
            "sandbox private temp unlink failed at {}: {error}",
            path.display()
        )),
    }
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
