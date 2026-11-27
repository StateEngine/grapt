//! Test-only filesystem helpers, shared by the crate's inline test modules.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_SCRATCH: AtomicUsize = AtomicUsize::new(0);

/// A temporary directory removed automatically on drop (including on panic).
pub(crate) struct ScratchDir(PathBuf);

impl ScratchDir {
    pub(crate) fn new(label: &str) -> std::io::Result<Self> {
        Self::new_in(&std::env::temp_dir(), label)
    }

    fn new_in(base: &Path, label: &str) -> std::io::Result<Self> {
        let pid = std::process::id();
        loop {
            let index = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("grapt-{label}-{pid}-{index}"));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn to_path_buf(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for ScratchDir {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes an executable to `path` (mode 0o700) without this process ever
/// holding the file open for write.
///
/// A test binary spawns children from many threads at once. A child forked
/// while this process holds a write descriptor on a file keeps a copy of it
/// in its own table until that child execs, and a spawn of that file inside
/// the interval fails with ETXTBSY ("Text file busy"): the descriptor's
/// O_CLOEXEC flag does not help, because the descriptor is still open in the
/// forked child before that exec. Writing through a short-lived child keeps
/// every descriptor of the file in that child's own table, which sibling
/// forks never sample. Same technique as `tests/public_seams.rs`.
#[cfg(unix)]
pub(crate) fn write_executable(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut writer = std::process::Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 700 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    writer
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("the stub writer has no stdin"))?
        .write_all(contents)?;
    let status = writer.wait()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "writing {path:?} failed: {status}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_dir_never_deletes_preexisting_directory() -> std::io::Result<()> {
        let parent = ScratchDir::new("scratch-parent")?;
        let next = NEXT_SCRATCH.load(Ordering::Relaxed);
        let preexisting = parent.join(format!("grapt-sentinel-{}-{next}", std::process::id()));
        std::fs::create_dir(&preexisting)?;
        let sentinel = preexisting.join("keep.txt");
        std::fs::write(&sentinel, b"untouched")?;

        let claimed = ScratchDir::new_in(&parent, "sentinel")?;
        assert_ne!(claimed.as_ref() as &Path, preexisting.as_path());
        assert_eq!(std::fs::read(&sentinel)?, b"untouched");
        let claimed_path = claimed.to_path_buf();
        drop(claimed);
        assert!(!claimed_path.exists());
        assert_eq!(std::fs::read(&sentinel)?, b"untouched");
        Ok(())
    }
}
