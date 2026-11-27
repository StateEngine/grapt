//! Drop-owned test scratch directories.
//!
//! A test that needs a filesystem scratch takes one of these instead of
//! naming a directory under the process temp by hand. The directory is
//! unique per call and is removed on drop, including on panic; it is kept
//! only when GRAPT_KEEP_TMP is set, for a developer inspecting a failure.
//! The base is std::env::temp_dir, so a harness can point tests off the
//! RAM-backed /tmp by setting TMPDIR.

#![allow(dead_code)]

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub struct Dir(PathBuf);

impl Dir {
    /// Creates `<base>-<pid>-<n>` under the process temp directory.
    pub fn new(base: &str) -> std::io::Result<Dir> {
        Self::named(base, None)
    }

    /// Creates `<base>-<pid>-<label>-<n>` under the process temp directory.
    /// The label is reduced to a filename-safe fragment.
    pub fn labeled(base: &str, label: &str) -> std::io::Result<Dir> {
        Self::named(base, Some(label))
    }

    /// Gives up the cleanup guarantee; the caller now owns the directory.
    pub fn into_path(mut self) -> PathBuf {
        let path = std::mem::replace(&mut self.0, PathBuf::new());
        std::mem::forget(self);
        path
    }

    /// Creates a directory with the given exact name under the process temp
    /// directory. The name is not sanitized and not made unique.
    pub fn exact(name: &str) -> std::io::Result<Dir> {
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Dir(path))
    }

    /// Takes the cleanup guarantee over the given path, creating it if absent.
    pub fn exact_path(path: PathBuf) -> std::io::Result<Dir> {
        std::fs::create_dir_all(&path)?;
        Ok(Dir(path))
    }

    fn named(base: &str, label: Option<&str>) -> std::io::Result<Dir> {
        let call = CALLS.fetch_add(1, Ordering::Relaxed);
        let label = label.map(sanitize).filter(|label| !label.is_empty());
        let name = match label {
            Some(label) => format!("{base}-{}-{label}-{call}", std::process::id()),
            None => format!("{base}-{}-{call}", std::process::id()),
        };
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Dir(path))
    }
}

fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

impl Deref for Dir {
    type Target = PathBuf;

    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if std::env::var_os("GRAPT_KEEP_TMP").is_some() {
            eprintln!(
                "grapt test scratch kept for inspection: {}",
                self.0.display()
            );
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "grapt test scratch cleanup failed: {}: {error}",
                    self.0.display()
                );
            }
        }
    }
}
