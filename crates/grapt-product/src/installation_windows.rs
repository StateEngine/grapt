//! Select the product's immutable build while its prefix SH guard is held.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use grapt_types::installation::{decode_pointer, POINTER_BYTES};

const REPARSE_POINT: u32 = 0x400;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const SHARE_READ_WRITE: u32 = 3;

pub(super) fn selected(prefix: &Path, binary: &Path) -> Result<(), String> {
    let path = prefix.join("current");
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("active installation cannot be selected: {:?}", error.kind()))?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err("installation current must be a regular pointer file".into());
    }
    // A no-delete lease prevents replacement through this bounded read. Open
    // the named entry itself so a reparse substitution cannot be followed.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OPEN_REPARSE_POINT)
        .share_mode(SHARE_READ_WRITE)
        .open(path)
        .map_err(|error| {
            format!(
                "active installation pointer cannot open: {:?}",
                error.kind()
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "active installation pointer is unreadable: {:?}",
            error.kind()
        )
    })?;
    if !metadata.is_file() || metadata.file_attributes() & REPARSE_POINT != 0 {
        return Err("installation current changed before selection".into());
    }
    let mut bytes = Vec::new();
    file.take(POINTER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            format!(
                "active installation pointer cannot be read: {:?}",
                error.kind()
            )
        })?;
    let selected = decode_pointer(&bytes).map_err(str::to_owned)?;
    let version = binary
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .ok_or("product has no immutable version")?;
    if selected.as_str() != version {
        return Err(format!(
            "this product is no longer active; restart with {} code",
            prefix.join("bin").join("grapt.exe").display()
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "installation_windows_tests.rs"]
mod tests;
