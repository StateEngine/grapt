//! Unix confined scratch allocation and cleanup.
//!
//! POSIX has no `funlink(fd)` syscall, so a separate `fstatat` + `unlinkat`
//! directly inside a shared world-writable temp parent (such as `/tmp`) cannot
//! rule out a concurrent rename by another user. Instead, every per-command
//! scratch directory is anchored inside a verified UID-exclusive `0700`
//! subdirectory (`grapt-scratch-<euid>`) of `private_temp_parent`:
//!
//! 1. `private_temp_parent` is opened with `O_PATH`/`O_SEARCH` so even `0300`
//!    (write+search-only) temp parents work without read permission.
//! 2. `grapt-scratch-<euid>` is created via `mkdirat(..., 0o700)`, opened via
//!    `openat(..., O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`, and
//!    verified via `fstat` to be a real directory owned by `geteuid()` with
//!    exact mode `0o700`.
//! 3. That verified `0700` parent directory descriptor is held across the
//!    child's lifetime. Because only `euid` can modify entries inside it and
//!    the sandboxed child is reaped before cleanup begins, no foreign user and
//!    no child process can rename or swap entries inside the anchor directory.
//! 4. As defense-in-depth, `verify_and_rmdir_under` still checks `(st_dev,
//!    st_ino)` before `unlinkat(..., AT_REMOVEDIR)`.

use super::combine_errors;
use nix::dir::Dir;
use nix::errno::Errno;
use nix::fcntl::{open, openat, AtFlags, OFlag};
use nix::sys::stat::{fchmod, fstat, fstatat, mkdirat, Mode, SFlag};
use nix::unistd::{geteuid, unlinkat, UnlinkatFlags};
use std::ffi::{CStr, CString};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
struct DirIdentity {
    dev: nix::libc::dev_t,
    ino: nix::libc::ino_t,
}

fn parent_dir_search_flag() -> OFlag {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        OFlag::O_PATH
    }
    #[cfg(target_vendor = "apple")]
    {
        OFlag::from_bits_retain(nix::libc::O_SEARCH)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        OFlag::O_RDONLY
    }
}

pub(in crate::dogfood::tools::sandbox) fn unix_scratch_anchor_path(parent: &Path) -> PathBuf {
    parent.join(format!("grapt-scratch-{}", geteuid().as_raw()))
}

pub(in crate::dogfood::tools::sandbox) fn ensure_unix_scratch_anchor(
    parent: &Path,
) -> Result<PathBuf, String> {
    let (_anchor_fd, anchor_path) = open_verified_uid_anchor(parent)?;
    Ok(anchor_path)
}

fn open_verified_uid_anchor(parent: &Path) -> Result<(OwnedFd, PathBuf), String> {
    let euid = geteuid().as_raw();
    let anchor_str = format!("grapt-scratch-{euid}");
    let anchor_name = CString::new(anchor_str.as_bytes())
        .map_err(|_| "sandbox private temp anchor name contains NUL".to_owned())?;

    // Open parent with O_PATH (Linux) / O_SEARCH (macOS) so write+search-only
    // (0300/1733) temp parents work without requiring read permission.
    let parent_flags =
        parent_dir_search_flag() | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let outer_fd = open(parent, parent_flags, Mode::empty()).map_err(|error| {
        format!(
            "sandbox private temp parent open failed at {}: {error}",
            parent.display()
        )
    })?;
    let outer_st = fstat(outer_fd.as_fd()).map_err(|error| {
        format!(
            "sandbox private temp parent fstat failed at {}: {error}",
            parent.display()
        )
    })?;
    let outer_perms = outer_st.st_mode & 0o7777;
    let trusted_owner = outer_st.st_uid == 0 || outer_st.st_uid == euid;
    let safe_mode = (outer_perms & 0o1000) != 0 || (outer_perms & 0o022) == 0;
    if !is_directory_mode(outer_st.st_mode) || !trusted_owner || !safe_mode {
        return Err(format!(
            "sandbox private temp parent must be root/euid-owned and sticky or non-group/world-writable (uid={}, mode={outer_perms:04o}) at {}",
            outer_st.st_uid,
            parent.display()
        ));
    }

    match mkdirat(outer_fd.as_fd(), anchor_name.as_c_str(), Mode::S_IRWXU) {
        Ok(()) | Err(Errno::EEXIST) => {}
        Err(error) => {
            return Err(format!(
                "sandbox private temp UID anchor mkdirat failed at {}: {error}",
                parent.join(&anchor_str).display()
            ));
        }
    }

    let anchor_flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let anchor_fd = openat(
        outer_fd.as_fd(),
        anchor_name.as_c_str(),
        anchor_flags,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "sandbox private temp UID anchor openat failed at {}: {error}",
            parent.join(&anchor_str).display()
        )
    })?;

    let st = fstat(anchor_fd.as_fd()).map_err(|error| {
        format!(
            "sandbox private temp UID anchor fstat failed at {}: {error}",
            parent.join(&anchor_str).display()
        )
    })?;
    let perms = st.st_mode & 0o7777;
    if !is_directory_mode(st.st_mode) || st.st_uid != euid || perms != 0o700 {
        return Err(format!(
            "sandbox private temp UID anchor failed 0700 ownership check (uid={}, mode={perms:04o}) at {}",
            st.st_uid,
            parent.join(&anchor_str).display()
        ));
    }

    Ok((anchor_fd, parent.join(anchor_str)))
}

fn create_unique_child_dir(
    anchor_fd: BorrowedFd<'_>,
    anchor_path: &Path,
) -> Result<(CString, String), String> {
    const MAX_ATTEMPTS: usize = 64;
    for _ in 0..MAX_ATTEMPTS {
        let ids = grapt_store::OsRandomIds::seed()
            .map_err(|error| format!("sandbox private temp entropy unavailable: {error}"))?;
        let suffix = &ids.instance()[..12.min(ids.instance().len())];
        let name_str = format!("grapt-sandbox-tmp-{suffix}");
        let name = CString::new(name_str.as_bytes())
            .map_err(|_| "sandbox private temp name contains NUL".to_owned())?;
        match mkdirat(anchor_fd, name.as_c_str(), Mode::S_IRWXU) {
            Ok(()) => return Ok((name, name_str)),
            Err(Errno::EEXIST) => continue,
            Err(error) => {
                return Err(format!(
                    "sandbox private temp mkdirat failed at {}: {error}",
                    anchor_path.join(&name_str).display()
                ));
            }
        }
    }
    Err(format!(
        "sandbox private temp exhausted {MAX_ATTEMPTS} unique directory attempts under {}",
        anchor_path.display()
    ))
}

pub(super) struct PinnedScratch {
    anchor_fd: OwnedFd,
    root_name: CString,
    root_fd: OwnedFd,
    root_id: DirIdentity,
    path: PathBuf,
}

impl PinnedScratch {
    pub(super) fn create(parent: &Path) -> Result<Self, String> {
        let (anchor_fd, anchor_path) = open_verified_uid_anchor(parent)?;
        let euid = geteuid().as_raw();
        let (root_name, name_str) = create_unique_child_dir(anchor_fd.as_fd(), &anchor_path)?;
        let path = anchor_path.join(&name_str);
        match Self::pin_under_anchor(anchor_fd, root_name.clone(), path.clone(), euid) {
            Ok(pinned) => Ok(pinned),
            Err((anchor_fd, reason)) => {
                let cleanup_err = match unlinkat(
                    anchor_fd.as_fd(),
                    root_name.as_c_str(),
                    UnlinkatFlags::RemoveDir,
                ) {
                    Ok(()) | Err(Errno::ENOENT) => None,
                    Err(error) => Some(format!("sandbox private temp root rmdir failed: {error}")),
                };
                Err(combine_errors(reason, [cleanup_err]))
            }
        }
    }

    fn pin_under_anchor(
        anchor_fd: OwnedFd,
        root_name: CString,
        path: PathBuf,
        euid: nix::libc::uid_t,
    ) -> Result<Self, (OwnedFd, String)> {
        let root_flags =
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let root_fd = match openat(
            anchor_fd.as_fd(),
            root_name.as_c_str(),
            root_flags,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(error) => {
                return Err((
                    anchor_fd,
                    format!(
                        "sandbox private temp root openat failed at {}: {error}",
                        path.display()
                    ),
                ));
            }
        };
        let st = match fstat(root_fd.as_fd()) {
            Ok(st) => st,
            Err(error) => {
                return Err((
                    anchor_fd,
                    format!(
                        "sandbox private temp root fstat failed at {}: {error}",
                        path.display()
                    ),
                ));
            }
        };
        let perms = st.st_mode & 0o7777;
        if !is_directory_mode(st.st_mode) || st.st_uid != euid || perms != 0o700 {
            return Err((
                anchor_fd,
                format!(
                    "sandbox private temp root failed 0700 ownership check (uid={}, mode={perms:04o}) at {}",
                    st.st_uid,
                    path.display()
                ),
            ));
        }
        Ok(Self {
            anchor_fd,
            root_name,
            root_fd,
            root_id: DirIdentity {
                dev: st.st_dev,
                ino: st.st_ino,
            },
            path,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn verify_canonical_path(&self, canonical: &Path) -> Result<(), String> {
        let st = nix::sys::stat::lstat(canonical).map_err(|error| {
            format!(
                "sandbox private temp canonical lstat failed at {}: {error}",
                canonical.display()
            )
        })?;
        if !is_directory_mode(st.st_mode)
            || st.st_dev != self.root_id.dev
            || st.st_ino != self.root_id.ino
        {
            return Err(format!(
                "sandbox private temp canonical path does not match pinned scratch identity: {}",
                canonical.display()
            ));
        }
        Ok(())
    }

    pub(super) fn cleanup(self) -> Result<(), String> {
        clean_tree_from_root_fd(self.root_fd, self.root_id)?;
        // `self.anchor_fd` is a verified 0700 UID-exclusive directory and the
        // sandboxed child is already reaped, establishing the ownership
        // boundary; `(st_dev, st_ino)` verification stays as defense-in-depth.
        verify_and_rmdir_under(
            self.anchor_fd.as_fd(),
            self.root_name.as_c_str(),
            self.root_id,
        )
    }
}

fn verify_and_rmdir_under(
    parent: BorrowedFd<'_>,
    name: &CStr,
    expected: DirIdentity,
) -> Result<(), String> {
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

fn is_directory_mode(mode: nix::sys::stat::mode_t) -> bool {
    (SFlag::from_bits_truncate(mode) & SFlag::S_IFMT) == SFlag::S_IFDIR
}

fn clean_tree_from_root_fd(root_fd: OwnedFd, root_id: DirIdentity) -> Result<(), String> {
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
    path_fd: OwnedFd,
    open_flags: OFlag,
    label: &str,
) -> Result<OwnedFd, String> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::PermissionsExt as _;

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
    std::fs::set_permissions(&proc_fd, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("sandbox private temp fd chmod failed for {label}: {error}"))?;
    open(
        proc_fd.as_str(),
        open_flags - OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| format!("sandbox private temp fd reopen failed for {label}: {error}"))
}

fn open_confined_child_dir(
    parent: BorrowedFd<'_>,
    name: &CStr,
) -> Result<(Dir, DirIdentity, Vec<CString>), String> {
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
    parent: BorrowedFd<'_>,
    name: &CStr,
    open_flags: OFlag,
) -> Result<OwnedFd, String> {
    let path_flags = OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let path_fd = openat(parent, name, path_flags, Mode::empty()).map_err(|error| {
        format!(
            "sandbox private temp directory O_PATH open failed for {:?}: {error}",
            name.to_string_lossy()
        )
    })?;
    relax_owned_opath_dir_and_reopen(path_fd, open_flags, &name.to_string_lossy())
}

#[cfg(not(target_os = "linux"))]
fn reopen_unreadable_child_dir(
    parent: BorrowedFd<'_>,
    name: &CStr,
    open_flags: OFlag,
) -> Result<OwnedFd, String> {
    use nix::sys::stat::{fchmodat, FchmodatFlags};
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

fn relax_dir_fd_and_read(fd: OwnedFd) -> Result<(Dir, DirIdentity, Vec<CString>), String> {
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
