//! Reject protected inode aliases before installing path-based containment.
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

pub(super) fn check(paths: &[PathBuf], writable: &[PathBuf]) -> Result<(), String> {
    let mut pending = paths
        .iter()
        .map(|path| (path.clone(), 0))
        .collect::<Vec<_>>();
    let mut remaining = 65_536_usize;
    while let Some((path, depth)) = pending.pop() {
        remaining = remaining
            .checked_sub(1)
            .ok_or("sandbox protected tree exceeds 65536 entries")?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("sandbox protected metadata unavailable: {error}")),
        };
        if metadata.is_file() && metadata.nlink() != 1 {
            return Err(format!(
                "sandbox protected file has hard-link aliases: {}",
                path.display()
            ));
        }
        if metadata.is_symlink() {
            let target = fs::canonicalize(&path)
                .map_err(|error| format!("sandbox protected link unavailable: {error}"))?;
            let target_metadata = fs::metadata(&target)
                .map_err(|error| format!("sandbox protected link target unavailable: {error}"))?;
            if target_metadata.is_dir() || writable.iter().any(|root| target.starts_with(root)) {
                return Err(format!(
                    "sandbox protected link exposes a mutable or unbounded target: {}",
                    path.display()
                ));
            }
            if target_metadata.is_file() && target_metadata.nlink() != 1 {
                return Err(format!(
                    "sandbox protected file has hard-link aliases: {}",
                    path.display()
                ));
            }
        }
        if !metadata.is_dir() {
            continue;
        }
        if depth >= 128 {
            return Err("sandbox protected tree exceeds 128 levels".into());
        }
        for entry in fs::read_dir(&path)
            .map_err(|error| format!("sandbox protected directory unavailable: {error}"))?
        {
            if pending.len() >= remaining || pending.len() >= 4096 {
                return Err("sandbox protected tree exceeds 65536 entries".into());
            }
            let entry =
                entry.map_err(|error| format!("sandbox protected entry unavailable: {error}"))?;
            pending.push((entry.path(), depth + 1));
        }
    }
    Ok(())
}
