//! Per-command private scratch temp directory for Linux and Windows sandboxes
//! when the shared host temp directory is an ancestor of the workspace.
use super::Plan;
use std::path::{Path, PathBuf};

/// Owned private scratch temp directory attached to one sandboxed child.
/// Call [`PrivateScratch::cleanup`] only after child termination is confirmed
/// so permission-locked subdirectories are relaxed strictly inside the owned
/// scratch tree and any removal failure is surfaced explicitly.
pub(super) struct PrivateScratch {
    dir: Option<tempfile::TempDir>,
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
        let dir = tempfile::Builder::new()
            .prefix("grapt-sandbox-tmp-")
            .tempdir_in(parent)
            .map_err(|error| format!("sandbox private temp directory unavailable: {error}"))?;
        let canonical_private = std::fs::canonicalize(dir.path())
            .map_err(|error| format!("sandbox private temp directory unavailable: {error}"))?;
        let text = canonical_private
            .to_str()
            .ok_or("sandbox private temp directory is not Unicode")?;
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
        Ok(Some(Self { dir: Some(dir) }))
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> Option<&Path> {
        self.dir.as_ref().map(tempfile::TempDir::path)
    }

    pub(super) fn cleanup(mut self) -> Result<(), String> {
        let Some(dir) = self.dir.take() else {
            return Ok(());
        };
        clean_tree_confined(dir.path())?;
        dir.close()
            .map_err(|error| format!("sandbox private temp cleanup failed: {error}"))
    }

    /// Disarm `TempDir::drop` when child termination could not be confirmed so
    /// we never walk or remove scratch entries against a still-live child.
    pub(super) fn abandon_unreaped(mut self) {
        if let Some(dir) = self.dir.take() {
            let _kept: PathBuf = dir.keep();
        }
    }
}

/// Run confined scratch cleanup only when the child is confirmed stopped;
/// otherwise disarm `TempDir::drop` and return an explicit error so callers
/// preserve it alongside the primary failure.
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
fn clean_tree_confined(root: &Path) -> Result<(), String> {
    use nix::dir::Dir;
    use nix::errno::Errno;
    use nix::fcntl::AtFlags;
    use nix::sys::stat::{fstatat, SFlag};
    use nix::unistd::{unlinkat, UnlinkatFlags};
    use std::ffi::CString;
    use std::os::fd::AsFd as _;

    struct Frame {
        dir: Dir,
        pending: Vec<CString>,
        parent_entry_name: Option<CString>,
    }

    let (root_dir, root_entries) = open_confined_root(root)?;
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
            if SFlag::from_bits_truncate(st.st_mode).contains(SFlag::S_IFDIR) {
                let (child_dir, child_entries) =
                    open_confined_child_dir(frame.dir.as_fd(), name.as_c_str())?;
                stack.push(Frame {
                    dir: child_dir,
                    pending: child_entries,
                    parent_entry_name: Some(name),
                });
            } else {
                // Never chmod regular files, symlinks, or hardlinks: unlinkat
                // only requires write+exec on the parent directory fd.
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

#[cfg(unix)]
fn open_confined_root(root: &Path) -> Result<(nix::dir::Dir, Vec<std::ffi::CString>), String> {
    use nix::errno::Errno;
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let fd = match open(root, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::EACCES) => {
            // Root itself is the owned TempDir created by this process; if the
            // child stripped read bits from it, verify it is still a real
            // directory before restoring owner rwx and reopening O_NOFOLLOW.
            let meta = std::fs::symlink_metadata(root).map_err(|error| {
                format!(
                    "sandbox private temp metadata failed at {}: {error}",
                    root.display()
                )
            })?;
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(format!(
                    "sandbox private temp root is not a real directory: {}",
                    root.display()
                ));
            }
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).map_err(
                |error| {
                    format!(
                        "sandbox private temp root permission relax failed at {}: {error}",
                        root.display()
                    )
                },
            )?;
            open(root, flags, Mode::empty()).map_err(|error| {
                format!(
                    "sandbox private temp root open failed at {}: {error}",
                    root.display()
                )
            })?
        }
        Err(error) => {
            return Err(format!(
                "sandbox private temp root open failed at {}: {error}",
                root.display()
            ));
        }
    };
    relax_dir_fd_and_read(fd)
}

#[cfg(unix)]
fn open_confined_child_dir(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
) -> Result<(nix::dir::Dir, Vec<std::ffi::CString>), String> {
    use nix::errno::Errno;
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::{fchmodat, FchmodatFlags, Mode};
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let fd = match openat(parent, name, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::EACCES) => {
            // Caller verified `S_IFDIR` with AT_SYMLINK_NOFOLLOW and confirmed
            // child termination; restore owner rwx only for this unreadable
            // directory, then reopen strictly with O_DIRECTORY | O_NOFOLLOW.
            fchmodat(parent, name, Mode::S_IRWXU, FchmodatFlags::FollowSymlink).map_err(
                |error| {
                    format!(
                        "sandbox private temp directory permission relax failed for {:?}: {error}",
                        name.to_string_lossy()
                    )
                },
            )?;
            openat(parent, name, flags, Mode::empty()).map_err(|error| {
                format!(
                    "sandbox private temp directory open failed for {:?}: {error}",
                    name.to_string_lossy()
                )
            })?
        }
        Err(error) => {
            return Err(format!(
                "sandbox private temp directory open failed for {:?}: {error}",
                name.to_string_lossy()
            ));
        }
    };
    relax_dir_fd_and_read(fd)
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
fn clean_tree_confined(root: &Path) -> Result<(), String> {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

    enum Work {
        Enter(PathBuf),
        RemoveDir(PathBuf),
    }

    let mut stack = vec![Work::Enter(root.to_path_buf())];
    while let Some(item) = stack.pop() {
        match item {
            Work::RemoveDir(path) => {
                if path == root {
                    continue;
                }
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
            Work::Enter(path) => {
                let meta = match std::fs::symlink_metadata(&path) {
                    Ok(meta) => meta,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(format!(
                            "sandbox private temp metadata failed at {}: {error}",
                            path.display()
                        ));
                    }
                };
                let attrs = meta.file_attributes();
                let is_reparse =
                    meta.file_type().is_symlink() || (attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0;
                if is_reparse {
                    // Never descend into or modify attributes on symlinks or
                    // directory junctions; remove only the reparse entry itself.
                    let remove = if (attrs & FILE_ATTRIBUTE_DIRECTORY) != 0 {
                        std::fs::remove_dir(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    match remove {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(format!(
                                "sandbox private temp reparse unlink failed at {}: {error}",
                                path.display()
                            ));
                        }
                    }
                    continue;
                }
                if meta.is_dir() {
                    clear_readonly_if_set(&path, &meta)?;
                    stack.push(Work::RemoveDir(path.clone()));
                    let entries = std::fs::read_dir(&path).map_err(|error| {
                        format!(
                            "sandbox private temp directory scan failed at {}: {error}",
                            path.display()
                        )
                    })?;
                    for entry in entries {
                        let entry = entry.map_err(|error| {
                            format!(
                                "sandbox private temp entry scan failed at {}: {error}",
                                path.display()
                            )
                        })?;
                        stack.push(Work::Enter(entry.path()));
                    }
                } else {
                    remove_windows_file(&path, &meta)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn remove_windows_file(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                && meta.permissions().readonly() =>
        {
            clear_readonly_if_set(path, meta)?;
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

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly_if_set(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    let mut perms = meta.permissions();
    if perms.readonly() {
        perms.set_readonly(false);
        std::fs::set_permissions(path, perms).map_err(|error| {
            format!(
                "sandbox private temp permission relax failed at {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn clean_tree_confined(_root: &Path) -> Result<(), String> {
    Ok(())
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
        // file inside scratch and verify confined handle-based cleanup removes all.
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
    fn confined_cleanup_unlinks_symlinks_and_hardlinks_without_modifying_outside_targets(
    ) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = tempfile::Builder::new()
            .prefix("grapt-scratch-confined-")
            .tempdir()
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
    fn cleanup_after_stop_skips_unreaped_child_and_preserves_errors() -> Result<(), String> {
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
            "unreaped cleanup must disarm TempDir drop instead of walking scratch"
        );
        let combined = combine_errors(
            "sandbox receipt was not delivered".to_owned(),
            [Some("sandbox reap failed: ECHILD".to_owned()), skipped],
        );
        assert!(combined.contains("sandbox receipt was not delivered"));
        assert!(combined.contains("sandbox reap failed: ECHILD"));
        assert!(combined.contains("child termination was not confirmed"));
        let _ = std::fs::remove_dir_all(&scratch_path);
        Ok(())
    }
}
