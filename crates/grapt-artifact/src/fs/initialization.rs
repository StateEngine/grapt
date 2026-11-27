//! A ready marker caches completed namespace durability, never pending mkdirs.
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use super::commit::sync_directory;

// Bumping the contract version changes both the name checked by the warm
// one-stat path and the content written before publication. Older versions
// therefore reinitialize without trusting their different durability contract.
const VERSION: &str = "1";
const TEMP_ATTEMPTS: usize = 64;

fn ready_path(root: &Path) -> PathBuf {
    root.join(format!(".secode-artifact-ready-v{VERSION}"))
}

fn content() -> String {
    format!("secode-artifact-ready-v{VERSION}\n")
}

fn ready(root: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(ready_path(root)) {
        Ok(metadata) if metadata.is_file() && metadata.len() == content().len() as u64 => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid artifact initialization marker",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn open(root: &Path) -> io::Result<()> {
    let root = if root.as_os_str().is_empty() {
        Path::new(".")
    } else {
        root
    };
    initialize(root, sync_directory, File::sync_all)
}

fn initialize(
    root: &Path,
    mut sync: impl FnMut(&Path) -> io::Result<()>,
    flush_marker: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    if ready(root)? {
        return Ok(());
    }
    for area in ["objects", "meta", "tmp"] {
        std::fs::create_dir_all(root.join(area))?;
    }
    // Existing names might belong to an initializer which has not synced yet,
    // or one which died. Both fresh and legacy trees need the complete barrier
    // before either initializer may publish readiness.
    for area in ["objects", "meta", "tmp"] {
        sync(&root.join(area))?;
    }
    for parent in root.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        sync(parent)?;
    }
    if root.is_relative() {
        sync(Path::new("."))?;
    }
    publish_ready(root, &mut sync, flush_marker)
}

fn temporary(root: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMP_ATTEMPTS {
        let ordinal = super::TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = root
            .join("tmp")
            .join(format!("init-{}-{ordinal}", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            // A dead initializer may have used the same PID and ordinal.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "artifact initialization temporary names are occupied",
    ))
}

fn publish_ready(
    root: &Path,
    sync: &mut impl FnMut(&Path) -> io::Result<()>,
    flush_marker: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    let (path, mut file) = temporary(root)?;
    let published = ready_path(root);
    let result = (|| {
        file.write_all(content().as_bytes())?;
        flush_marker(&file)?;
        drop(file);
        std::fs::rename(&path, &published)?;
        if let Err(error) = sync(root) {
            // Do not leave this failed publication as a fast-path marker. A
            // concurrent successful initializer's identical marker may also be
            // removed; that costs a retry, without weakening durability.
            match std::fs::remove_file(&published) {
                Ok(()) => {}
                Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => {}
                Err(cleanup) => return Err(cleanup),
            }
            return Err(error);
        }
        Ok(())
    })();
    if result.is_err() {
        // Only this opener's create_new temporary is eligible for cleanup.
        let _ = std::fs::remove_file(path);
    }
    result
}

// Unix directory flushes establish the namespace guarantee above. Windows
// retains commit::sync_directory's no-op; the marker adds no stronger promise.
#[cfg(test)]
#[path = "initialization/tests.rs"]
mod tests;
