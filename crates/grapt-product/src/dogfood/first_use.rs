//! First-run setup and explicit account import use the same profile owners.
mod import;
mod model_check;
pub(crate) use import::initialize as initialize_profile;
pub(crate) use import::initialize_empty as initialize_empty_profile;
pub(crate) use model_check::check as check_chatgpt_models;
use std::io::{Read, Write};
use std::path::Path;

pub(super) fn read_bounded_regular_file(
    path: &Path,
    what: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("{what} metadata unreadable: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{what} must be a regular file"));
    }
    if metadata.len() > limit as u64 {
        return Err(format!("{what} exceeds its read bound"));
    }
    let file = std::fs::File::open(path).map_err(|error| format!("{what} unreadable: {error}"))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{what} unreadable: {error}"))?;
    if bytes.len() > limit {
        return Err(format!("{what} byte bound exceeded"));
    }
    Ok(bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let protected;
    let bytes = if path
        .extension()
        .is_some_and(|extension| extension == "credential")
        && path
            .file_name()
            .is_some_and(|name| name != "basic.credential")
    {
        protected = grapt_login::profile_security::seal(bytes).map_err(|error| {
            format!(
                "profile credential protection failed: {}",
                grapt_login::profile_security::failure_diagnostic(&error)
            )
        })?;
        protected.as_slice()
    } else {
        bytes
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("local profile file creation: {error}"))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("local profile file write: {error}"))
}

fn executable_build() -> Result<String, String> {
    const MAX_EXECUTABLE_BYTES: u64 = 1024 * 1024 * 1024;
    let path = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    if length == 0 || length > MAX_EXECUTABLE_BYTES {
        return Err("executable fingerprint size bound exceeded".into());
    }
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    let mut read = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > length {
            return Err("executable changed during fingerprinting".into());
        }
        digest.update(&buffer[..count]);
    }
    if read != length {
        return Err("executable changed during fingerprinting".into());
    }
    let digest = digest.finish();
    let hex: String = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}
