//! The private lock file protocol shared by the credential vault and the
//! credential key pointer.
//!
//! A new lock is created exclusively and given this user's ownership, which
//! also normalizes an elevated token's Administrators owner. An existing lock
//! is opened without following a link or reparse point and must already be
//! this user's, private and empty. On Windows neither open shares delete, so
//! the path cannot be replaced while the lock is held.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Which step of opening a private lock failed.
#[derive(Debug)]
pub(crate) enum LockFailure {
    Create(io::Error),
    Protect(io::Error),
    Open(io::Error),
    Verify(io::Error),
    Metadata(io::Error),
    /// An existing lock holds bytes, so it is not a lock this protocol made.
    NotEmpty,
}

/// Read and write, sharing read and write but never delete.
#[cfg(windows)]
const SHARE_READ_WRITE: u32 = 0x0000_0001 | 0x0000_0002;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Create or open the private lock at `path`. The caller then locks it.
pub(crate) fn open_private_lock(path: &Path) -> Result<File, LockFailure> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, SHARE_READ_WRITE);
    match options.open(path) {
        Ok(file) => {
            super::protect_new_private_file(path, &file).map_err(LockFailure::Protect)?;
            Ok(file)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_existing(path),
        Err(error) => Err(LockFailure::Create(error)),
    }
}

fn open_existing(path: &Path) -> Result<File, LockFailure> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(SHARE_READ_WRITE);
    }
    let file = options.open(path).map_err(LockFailure::Open)?;
    super::verify_owner_only_file(path, &file).map_err(LockFailure::Verify)?;
    if file.metadata().map_err(LockFailure::Metadata)?.len() != 0 {
        return Err(LockFailure::NotEmpty);
    }
    Ok(file)
}

/// The failure as one I/O error, for callers without per-step reporting.
impl From<LockFailure> for io::Error {
    fn from(failure: LockFailure) -> Self {
        match failure {
            LockFailure::Create(error)
            | LockFailure::Protect(error)
            | LockFailure::Open(error)
            | LockFailure::Verify(error)
            | LockFailure::Metadata(error) => error,
            LockFailure::NotEmpty => io::Error::new(
                io::ErrorKind::InvalidData,
                "an existing lock file holds data; it was left in place",
            ),
        }
    }
}
