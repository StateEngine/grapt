//! A keystore in files, for tests off Windows that configured one. It keeps
//! keys across processes as a real keystore does, so a shipped binary under
//! test and the test that reads its output share keys when the test gives the
//! child its directory, and a test can make it unavailable on its own thread,
//! as a machine without a Secret Service is.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

#[cfg(test)]
thread_local! {
    static UNAVAILABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While held, this thread's keystore is unavailable.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct Unavailable(());

#[cfg(test)]
impl Unavailable {
    pub(crate) fn now() -> Self {
        UNAVAILABLE.set(true);
        Self(())
    }
}

#[cfg(test)]
impl Drop for Unavailable {
    fn drop(&mut self) {
        UNAVAILABLE.set(false);
    }
}

fn reachable() -> io::Result<()> {
    #[cfg(test)]
    if UNAVAILABLE.get() {
        return Err(super::unavailable(
            "the test keystore is unavailable on this thread".to_owned(),
        ));
    }
    Ok(())
}

fn directory(root: &Path) -> PathBuf {
    root.join("keystore")
}

/// The secret stored under `name`, or `None` when there is no such entry.
pub(super) fn get(root: &Path, name: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    reachable()?;
    match fs::read(directory(root).join(name)) {
        Ok(secret) => Ok(Some(Zeroizing::new(secret))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Store `secret` under a `name` no entry has yet.
pub(super) fn set(root: &Path, name: &str, secret: &[u8]) -> io::Result<()> {
    reachable()?;
    let directory = directory(root);
    fs::create_dir_all(&directory)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(directory.join(name))?;
    file.write_all(secret)?;
    file.sync_all()
}
