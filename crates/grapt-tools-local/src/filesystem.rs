//! Bounded byte capture and unlinked filesystem access shared by local tools.

use std::io::Read;
use std::path::Path;

pub(crate) fn capture(reader: impl Read, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unreadable: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err("file byte bound exceeded".into());
    }
    Ok(bytes)
}

pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let reader = std::fs::File::open(path).map_err(|error| format!("unreadable: {error}"))?;
    capture(reader, limit)
}

/// Check components relative to an application-validated, authorized workspace root.
pub fn reject_links(root: &Path, relative: &Path) -> Result<(), String> {
    walk_without_links(root, relative, || {
        format!("{} not found", relative.display())
    })
}

/// Refuses links along an application-validated relative path. Only a NotFound
/// component gets `missing`'s wording; other failures keep their real cause.
pub fn walk_without_links(
    root: &Path,
    relative: &Path,
    missing: impl Fn() -> String,
) -> Result<(), String> {
    let mut current = root.to_path_buf();
    for part in relative.components() {
        current.push(part);
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                missing()
            } else {
                format!("{} is unreadable: {error}", relative.display())
            }
        })?;
        if is_link(&metadata) {
            return Err(format!(
                "coding path contains a symlink or reparse point: {}",
                relative.display()
            ));
        }
    }
    Ok(())
}

pub fn is_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
