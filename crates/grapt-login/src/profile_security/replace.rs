//! The existing sign-in writer, shared by the credential storage owner.
use std::{
    io::{self, Write},
    path::Path,
};

/// Atomically publish one bounded vault entry. The caller holds its storage lock
/// and recovers the fixed staging slot before retrying after interruption.
pub fn replace_private_file(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    write(directory, name, &format!(".{name}.new"), bytes)
}

pub(crate) fn replace_sign_in_file(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    write(
        directory,
        name,
        &format!(".{name}.{}.new", std::process::id()),
        bytes,
    )
}

fn write(directory: &Path, name: &str, staged: &str, bytes: &[u8]) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('.')
        || name.contains("..")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid private file name",
        ));
    }
    #[cfg(windows)]
    super::protect_directory(directory)?;
    let temporary = directory.join(staged);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temporary)?;
    let written = super::protect_new_private_file(&temporary, &file)
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    std::fs::rename(&temporary, directory.join(name)).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
    // Matches the sign-in owner's existing best-effort directory sync. Vault
    // mutations additionally require a supported platform's directory sync.
    if let Ok(file) = std::fs::File::open(directory) {
        let _ = file.sync_all();
    }
    Ok(())
}
