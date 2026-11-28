//! Guarded acquisition shared by instruction sources and skill resources.
//! Extracted from tools/skill.rs; callers retain the guard through their read.

use std::path::Path;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
use std::path::PathBuf;

/// An open source, with whatever keeps its location honest until it is read.
pub(super) struct OpenedResource {
    pub(super) file: std::fs::File,
    /// Windows: every directory from the volume root to the file, held open
    /// without delete sharing, so none of them can be renamed, removed or
    /// replaced while the file is checked and read.
    #[cfg(windows)]
    _pinned: Vec<std::fs::File>,
    #[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
    requested: PathBuf,
}

impl OpenedResource {
    /// Whether the OPEN file lives inside `base`.
    pub(super) fn inside(&self, base: &Path) -> std::io::Result<bool> {
        // Linux names the descriptor's file itself, wherever its path has moved.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            use std::os::fd::AsRawFd as _;
            let location = std::fs::read_link(format!("/proc/self/fd/{}", self.file.as_raw_fd()))?;
            Ok(location.starts_with(base))
        }
        // Windows opened the file through a chain of pinned, link-free
        // directories below the canonical base, so it cannot be anywhere else.
        #[cfg(windows)]
        {
            let _ = base;
            Ok(true)
        }
        // Elsewhere (not a supported target): the canonical path counts only
        // while it is still the file that was opened.
        #[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
        {
            use std::os::unix::fs::MetadataExt as _;
            let location = std::fs::canonicalize(&self.requested)?;
            let (opened, named) = (self.file.metadata()?, std::fs::metadata(&location)?);
            Ok(location.starts_with(base)
                && opened.dev() == named.dev()
                && opened.ino() == named.ino())
        }
    }
}

/// `O_NONBLOCK`, which std does not name: 0o4000 on the supported Linux
/// targets, 0x4 on the Apple and BSD systems.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) const NONBLOCK: i32 = 0o4000;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
pub(super) const NONBLOCK: i32 = 0x0004;

/// `base` is the canonical authority anchor; `relative` has only normal components.
#[cfg(not(windows))]
pub(super) fn open_contained(base: &Path, relative: &Path) -> std::io::Result<OpenedResource> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let requested = base.join(relative);
    Ok(OpenedResource {
        // Opening a FIFO for reading waits for a writer. Open without waiting,
        // so whatever was opened reaches the regular-file refusal at once.
        file: std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK)
            .open(&requested)?,
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        requested,
    })
}

#[cfg(not(windows))]
fn open_dir_segment_nofollow(
    parent_fd: &std::os::fd::OwnedFd,
    segment: &std::ffi::OsStr,
) -> std::io::Result<std::os::fd::OwnedFd> {
    use nix::fcntl::{openat, AtFlags, OFlag};
    use nix::sys::stat::{fstatat, Mode};

    match openat(
        parent_fd,
        segment,
        OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => Ok(fd),
        Err(nix::errno::Errno::ENOTDIR)
            if fstatat(parent_fd, segment, AtFlags::AT_SYMLINK_NOFOLLOW)
                .is_ok_and(|st| (st.st_mode & nix::libc::S_IFMT) == nix::libc::S_IFLNK) =>
        {
            Err(std::io::Error::from_raw_os_error(nix::libc::ELOOP))
        }
        Err(err) => Err(std::io::Error::from(err)),
    }
}

#[cfg(not(windows))]
fn open_dir_nofollow(path: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    use std::path::Component;

    let mut dir_fd = open(
        "/",
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(segment) => {
                dir_fd = open_dir_segment_nofollow(&dir_fd, segment)?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "scope directory path contains a non-normal component",
                ));
            }
        }
    }
    Ok(dir_fd)
}

/// True when `open_contained_nofollow` rejected a symlink or reparse point.
pub(super) fn is_symlink_refusal(error: &std::io::Error) -> bool {
    #[cfg(not(windows))]
    {
        error.raw_os_error() == Some(nix::libc::ELOOP)
    }
    #[cfg(windows)]
    {
        error.to_string().contains("link or junction")
    }
}

/// Like [`open_contained`], but acquires `base` via a link-refusing component
/// walk from `/` and walks each original component of `relative` (including
/// `..` parent segments) via dirfd-relative `openat` with `O_NOFOLLOW`
/// (`O_DIRECTORY` on intermediate segments), never reopening by pathname.
#[cfg(not(windows))]
pub(super) fn open_contained_nofollow(
    base: &Path,
    relative: &Path,
) -> std::io::Result<OpenedResource> {
    use nix::fcntl::{openat, OFlag};
    use nix::sys::stat::Mode;
    use std::path::Component;

    let scope_fd = open_dir_nofollow(base)?;
    let mut dir_stack = vec![scope_fd];
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let mut normalized = base.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if dir_stack.len() <= 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "source file path escapes its governing scope",
                    ));
                }
                dir_stack.pop();
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                {
                    normalized.pop();
                }
            }
            Component::Normal(segment) => {
                let parent_fd = dir_stack.last().ok_or_else(|| {
                    std::io::Error::other("scope directory descriptor stack is empty")
                })?;
                if components.peek().is_some() {
                    let next_fd = open_dir_segment_nofollow(parent_fd, segment)?;
                    dir_stack.push(next_fd);
                    #[cfg(not(any(target_os = "linux", target_os = "android")))]
                    normalized.push(segment);
                } else {
                    let file_fd = openat(
                        parent_fd,
                        segment,
                        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                        Mode::empty(),
                    )
                    .map_err(std::io::Error::from)?;
                    #[cfg(not(any(target_os = "linux", target_os = "android")))]
                    normalized.push(segment);
                    return Ok(OpenedResource {
                        file: std::fs::File::from(file_fd),
                        #[cfg(not(any(target_os = "linux", target_os = "android")))]
                        requested: normalized,
                    });
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "source file relative path contains an absolute component",
                ));
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "source file relative path does not name a file",
    ))
}

#[cfg(windows)]
pub(super) fn open_contained_nofollow(
    base: &Path,
    relative: &Path,
) -> std::io::Result<OpenedResource> {
    // Windows `open_contained` already pins every component with
    // `FILE_FLAG_OPEN_REPARSE_POINT` and refuses reparse points.
    open_contained(base, relative)
}

#[cfg(windows)]
pub(super) fn open_contained(base: &Path, relative: &Path) -> std::io::Result<OpenedResource> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    // Share read and write but never delete: a held directory or file cannot be
    // renamed, removed or replaced until it is closed.
    const SHARE_READ_WRITE: u32 = 0x0000_0001 | 0x0000_0002;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    // Backup semantics let a directory open too, so a requested directory is
    // refused as "not a regular file" rather than as unreadable.
    let open = |path: &Path| -> std::io::Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ_WRITE)
            .custom_flags(OPEN_REPARSE_POINT | BACKUP_SEMANTICS)
            .open(path)?;
        if file.metadata()?.file_attributes() & ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(std::io::Error::other(
                "a link or junction is on the source file's path",
            ));
        }
        Ok(file)
    };
    let mut pinned = Vec::new();
    let mut prefix = PathBuf::new();
    for component in base.components() {
        prefix.push(component);
        if !matches!(component, std::path::Component::Prefix(_)) {
            pinned.push(open(&prefix)?);
        }
    }
    let mut depth = 0usize;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "source file path escapes its governing scope",
                    ));
                }
                prefix.pop();
                depth -= 1;
            }
            std::path::Component::Normal(segment) => {
                prefix.push(segment);
                depth += 1;
                if components.peek().is_some() {
                    pinned.push(open(&prefix)?);
                }
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "source file relative path contains an absolute component",
                ));
            }
        }
    }
    Ok(OpenedResource {
        file: open(&prefix)?,
        _pinned: pinned,
    })
}
