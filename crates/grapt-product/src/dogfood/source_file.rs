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

/// `O_NOFOLLOW`, which std does not name: 0o100000 on Linux ARM/AArch64,
/// 0o400000 on Linux x86/x86_64/other, and 0x0100 on Apple/BSD.
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

/// Like [`open_contained`], but walks each relative component from `base` via
/// dirfd-relative `openat` with `O_NOFOLLOW` (`O_DIRECTORY` on intermediate
/// segments), so the final open is anchored to the exact verified parent fd
/// without reopening by pathname.
#[cfg(not(windows))]
pub(super) fn open_contained_nofollow(
    base: &Path,
    relative: &Path,
) -> std::io::Result<OpenedResource> {
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::Mode;
    use std::path::Component;

    let mut dir_fd = open(
        base,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;

    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(segment) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "source file relative path contains a non-normal component",
            ));
        };
        if components.peek().is_some() {
            dir_fd = openat(
                &dir_fd,
                segment,
                OFlag::O_RDONLY
                    | OFlag::O_DIRECTORY
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC
                    | OFlag::O_NONBLOCK,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?;
        } else {
            let file_fd = openat(
                &dir_fd,
                segment,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?;
            return Ok(OpenedResource {
                file: std::fs::File::from(file_fd),
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                requested: base.join(relative),
            });
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "source file relative path is empty",
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
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        prefix.push(component);
        if components.peek().is_some() {
            pinned.push(open(&prefix)?);
        }
    }
    Ok(OpenedResource {
        file: open(&prefix)?,
        _pinned: pinned,
    })
}
