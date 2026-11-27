//! Protection for the existing profile files; never a second credential store.
//!
//! The DPAPI operation.
//! Copyright 2025 OpenAI. Modified by Grapt; see licenses/openai-codex/.
//! Unlike that sandbox's machine scope, credentials use CurrentUser. The
//! managed Windows adapter avoids unsafe FFI and sends secrets only over pipes.
use std::io;
use std::path::Path;

mod private_read;
pub use private_read::{read_owner_only_file, verify_owner_only_file};
mod batch;
pub use batch::ProtectionBatch;
mod replace;
#[cfg(any(windows, test))]
mod request_budget;
#[cfg(any(windows, test))]
mod windows_protocol;
pub use replace::replace_private_file;
pub(crate) use replace::replace_sign_in_file;

mod refusal;
#[cfg(any(windows, test))]
pub(crate) use refusal::child_refusal;
pub use refusal::{failure_code, failure_diagnostic};

const ENVELOPE: &[u8] = b"secode.dpapi.current-user.v1\n";

/// Whether bytes carry the protected format, without attempting decryption.
#[must_use]
pub fn is_protected(bytes: &[u8]) -> bool {
    bytes.starts_with(ENVELOPE)
}

/// Finite room for DPAPI's header, padding and authentication data.
#[must_use]
pub const fn stored_bound(plaintext_bound: usize) -> usize {
    if cfg!(windows) {
        plaintext_bound.saturating_add(1024)
    } else {
        plaintext_bound
    }
}

/// Protect bytes before the existing atomic writer creates its temporary file.
pub fn seal(bytes: &[u8]) -> io::Result<Vec<u8>> {
    #[cfg(windows)]
    {
        let protected = windows::run("seal", bytes)?;
        let mut result = ENVELOPE.to_vec();
        result.extend(protected);
        Ok(result)
    }
    #[cfg(not(windows))]
    Ok(bytes.to_vec())
}

/// Decode an envelope in memory, bounding the plaintext independently of storage.
/// Legacy plaintext remains readable; an invalid envelope never falls back to it.
pub fn open(bytes: &[u8], plaintext_bound: usize) -> io::Result<Vec<u8>> {
    let plaintext = if let Some(protected) = bytes.strip_prefix(ENVELOPE) {
        #[cfg(windows)]
        {
            windows::run("open", protected)?
        }
        #[cfg(not(windows))]
        {
            let _ = protected;
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows user-protected credential requires its owning Windows account",
            ));
        }
    } else {
        bytes.to_vec()
    };
    if plaintext.len() > plaintext_bound {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "credential plaintext exceeds its byte bound",
        ));
    }
    Ok(plaintext)
}

/// Restrict an existing real directory to the current user, including inheritance
/// for newly created files. Call before any credential/configuration write.
pub fn protect_directory(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile protection requires a real directory",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "profile protection refuses a reparse point",
            ));
        }
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "profile protection requires a Unicode path",
            )
        })?;
        windows::run("directory", path.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Check persisted trust storage before consuming a prior decision; never
/// legitimize an insecure registry by changing its permissions at read time.
pub fn verify_directory(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "trust storage must be a real private directory",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust storage is a reparse point",
            ));
        }
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust storage requires a Unicode path",
            )
        })?;
        windows::run("directory-check", path.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trust storage is accessible to other users",
            ));
        }
    }
    Ok(())
}

/// Establish user ownership on a newly created private trust record. Windows
/// elevated tokens default new objects to their Administrators owner; permit
/// only that token owner to be normalized, then apply the strict read check.
/// Call only after create_new in the already protected trust directory.
pub fn protect_new_private_file(path: &Path, file: &std::fs::File) -> io::Result<()> {
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    if opened != same_file::Handle::from_path(path)? || !file.metadata()?.is_file() {
        return Err(io::Error::other("new trust record changed identity"));
    }
    #[cfg(windows)]
    {
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust record requires a Unicode path",
            )
        })?;
        windows::run("file-owner", path.as_bytes())?;
    }
    verify_private_file(path, file)
}

/// Check a trust record's own authority, not just its parent directory.
/// The caller keeps an opened, no-write/no-delete Windows lease through its
/// read. Identity is checked against that opened object before and after ACL
/// inspection; an unsafe existing record is refused, never repaired into trust.
pub fn verify_private_file(path: &Path, file: &std::fs::File) -> io::Result<()> {
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    if opened != same_file::Handle::from_path(path)? || !file.metadata()?.is_file() {
        return Err(io::Error::other("trust record changed while opening"));
    }
    #[cfg(windows)]
    {
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "trust record requires a Unicode path",
            )
        })?;
        windows::run("file-check", path.as_bytes())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = file.metadata()?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("trust record has no parent"))?;
        if metadata.uid() != std::fs::metadata(parent)?.uid()
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trust record is foreign-writable or foreign-owned",
            ));
        }
    }
    if opened != same_file::Handle::from_path(path)? {
        return Err(io::Error::other(
            "trust record changed during authority inspection",
        ));
    }
    Ok(())
}

#[cfg(windows)]
#[path = "profile_security/windows.rs"]
mod windows;

#[cfg(test)]
#[path = "profile_security/tests.rs"]
mod tests;
