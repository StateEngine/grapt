//! Recovery shares the managed writer's existing lock only while publishing.
use std::{fs::File, io, path::Path};

pub(super) fn recover_lock(profile: &Path, journal: &Path) -> io::Result<Option<File>> {
    match std::fs::symlink_metadata(journal.join("connections.json")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    // Staging a managed generation first acquires this lock. Its absence is
    // corruption, never a reason to create a different recovery authority.
    let path = profile.join("connections.lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NOFOLLOW | libc::O_NONBLOCK,
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000).share_mode(3);
    }
    let file = options.open(&path)?;
    crate::profile_security::verify_owner_only_file(&path, &file)?;
    if file.metadata()?.len() != 0 {
        return Err(super::invalid("connection lock is not empty"));
    }
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => {
            io::Error::new(io::ErrorKind::WouldBlock, "connections are being changed")
        }
        std::fs::TryLockError::Error(error) => error,
    })?;
    Ok(Some(file))
}
