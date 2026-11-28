// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reading the host's private files: a regular file, never a link, the same
//! file once opened, and on Unix owned by the client's principal alone.
use crate::peer::Principal;
use std::io::Read;
use std::path::Path;
use zeroize::Zeroizing;

/// Read a private regular file of at most `max_bytes`: not a link, the same
/// file once opened, and on Unix owned by `principal` with mode 0600.
pub(crate) fn read_private(
    path: &Path,
    principal: &Principal,
    max_bytes: u64,
) -> Result<Zeroizing<Vec<u8>>, String> {
    let shown = |error: std::io::Error| format!("{}: {error}", path.display());
    let before = std::fs::symlink_metadata(path).map_err(shown)?;
    if !before.is_file() {
        return Err(format!(
            "{} is a link or not a regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    owned_with_mode(path, &before, principal, 0o600)?;
    #[cfg(not(unix))]
    let _ = principal;
    let file = std::fs::File::open(path).map_err(shown)?;
    if !same_file(&before, &file.metadata().map_err(shown)?) {
        return Err(format!("{} changed while it was opened", path.display()));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(shown)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(format!(
            "{} is larger than {max_bytes} bytes",
            path.display()
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
pub(crate) fn owned_with_mode(
    path: &Path,
    metadata: &std::fs::Metadata,
    principal: &Principal,
    mode: u32,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let Principal::Unix(uid) = principal else {
        return Err("a Unix state directory needs a Unix principal".into());
    };
    if metadata.uid() != *uid {
        return Err(format!("{} belongs to another user", path.display()));
    }
    if metadata.mode() & 0o777 != mode {
        return Err(format!("{} is not mode {mode:o}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn same_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    opened.is_file() && before.dev() == opened.dev() && before.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(_before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    opened.is_file()
}
