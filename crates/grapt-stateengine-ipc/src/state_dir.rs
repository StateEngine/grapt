// SPDX-License-Identifier: MIT OR Apache-2.0
//! The host's private state directory, opened once and read relative to that
//! handle: a file replaced by a link, a FIFO or another user's file is
//! refused on the opened descriptor itself, never followed or waited on.
//!
//! Windows owner-only access lists cannot be checked here yet, so on Windows
//! the directory refuses rather than trusting any regular file.
use crate::peer::Principal;
use std::path::Path;
use zeroize::Zeroizing;

#[cfg(unix)]
const PRIVATE_DIRECTORY: rustix::fs::RawMode = 0o700;
#[cfg(unix)]
const PRIVATE_FILE: rustix::fs::RawMode = 0o600;

/// A state directory checked to be `principal`'s alone.
pub(crate) struct StateDir {
    #[cfg(unix)]
    directory: std::os::fd::OwnedFd,
    #[cfg(unix)]
    uid: u32,
}

#[cfg(unix)]
impl StateDir {
    pub(crate) fn open(path: &Path, principal: &Principal) -> Result<Self, String> {
        use rustix::fs::{FileType, Mode, OFlags};
        let Principal::Unix(uid) = principal else {
            return Err("a Unix state directory needs a Unix principal".into());
        };
        let directory = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
        let stat = rustix::fs::fstat(&directory)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(format!("{} is not a directory", path.display()));
        }
        owned(
            &path.display().to_string(),
            stat.st_uid,
            stat.st_mode,
            *uid,
            PRIVATE_DIRECTORY,
        )?;
        Ok(Self {
            directory,
            uid: *uid,
        })
    }

    /// The private file `name`, at most `max_bytes`, checked on the opened
    /// descriptor: a regular file, the principal's, mode 0600.
    pub(crate) fn read(&self, name: &str, max_bytes: u64) -> Result<Zeroizing<Vec<u8>>, String> {
        use rustix::fs::{FileType, Mode, OFlags};
        use std::io::Read;
        let file = rustix::fs::openat(
            &self.directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| format!("{name}: {error}"))?;
        let stat = rustix::fs::fstat(&file).map_err(|error| format!("{name}: {error}"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(format!("{name} is not a regular file"));
        }
        owned(name, stat.st_uid, stat.st_mode, self.uid, PRIVATE_FILE)?;
        let mut bytes = Zeroizing::new(Vec::new());
        std::fs::File::from(file)
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{name}: {error}"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(format!("{name} is larger than {max_bytes} bytes"));
        }
        Ok(bytes)
    }

    /// `name` in this directory is a socket of the principal's, not a link.
    pub(crate) fn socket(&self, name: &std::ffi::OsStr) -> Result<(), String> {
        use rustix::fs::{AtFlags, FileType};
        let stat = rustix::fs::statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| format!("the socket: {error}"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Socket {
            return Err("the socket entry is not a socket".into());
        }
        if stat.st_uid != self.uid {
            return Err("the socket belongs to another user".into());
        }
        Ok(())
    }
}

#[cfg(unix)]
fn owned(
    name: &str,
    owner: u32,
    mode: rustix::fs::RawMode,
    uid: u32,
    expected: rustix::fs::RawMode,
) -> Result<(), String> {
    if owner != uid {
        return Err(format!("{name} belongs to another user"));
    }
    if mode & 0o777 != expected {
        return Err(format!("{name} is not mode {expected:o}"));
    }
    Ok(())
}

#[cfg(not(unix))]
impl StateDir {
    pub(crate) fn open(path: &Path, _principal: &Principal) -> Result<Self, String> {
        Err(format!(
            "{}: owner-only access cannot be verified on this platform yet, so it is not trusted",
            path.display()
        ))
    }

    pub(crate) fn read(&self, name: &str, _max_bytes: u64) -> Result<Zeroizing<Vec<u8>>, String> {
        Err(format!("{name}: no verified state directory"))
    }
}
