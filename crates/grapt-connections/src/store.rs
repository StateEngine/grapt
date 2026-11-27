//! Secret-free connection records, serialized under one profile lock.
//!
//! File storage for the provider auth state.
//! Copyright 2025 OpenAI. Modified by Grapt: bounded private reads,
//! validated records and atomic publication instead of truncating in place.

use grapt_login::profile_security;
use std::{
    fs::File,
    io,
    io::Write,
    path::{Path, PathBuf},
};

mod install;
use install::sync_directory;
mod migration;
mod removal;
pub use install::{next_key_files, InstallCommit};
mod import;
mod model;
pub use import::ProfileImport;
pub(crate) mod selection;
pub use model::{
    AuthMethod, Connection, Connections, CredentialFormat, DefaultModel, Family, CONNECTION_CAP,
};

pub const FILE: &str = "connections.json";
const STAGED: &str = ".connections.json.new";
const LOCK: &str = "connections.lock";
const BYTE_CAP: usize = 64 * 1024;

/// An exclusive, synchronous storage operation. Never retain across an await.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    _lock: File,
    #[cfg(test)]
    failure: Option<install::Failure>,
}

impl Store {
    /// Refuse contention immediately. The caller may retry outside its I/O owner.
    pub fn open(profile: &Path) -> io::Result<Self> {
        let mut store = Self::lock(profile)?;
        store.discard_staged()?;
        store.recover_install()?;
        store.recover_removal()?;
        Ok(store)
    }

    fn lock(profile: &Path) -> io::Result<Self> {
        profile_security::verify_directory(profile)?;
        let path = profile.join(LOCK);
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 3);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let lock = match options.open(&path) {
            Ok(file) => {
                profile_security::protect_new_private_file(&path, &file)?;
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // Open the lock with compatible sharing so Windows contention
                // reaches try_lock. Private snapshot reads deliberately deny
                // write sharing and must not be used for a live lock file.
                let before = std::fs::symlink_metadata(&path)?;
                if !before.is_file() || before.file_type().is_symlink() {
                    return Err(invalid("connections lock is not a regular file"));
                }
                let mut options = std::fs::OpenOptions::new();
                options.read(true).write(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::custom_flags(
                    &mut options,
                    libc::O_NOFOLLOW | libc::O_NONBLOCK,
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.custom_flags(0x0020_0000).share_mode(3);
                }
                let file = options.open(&path)?;
                profile_security::verify_owner_only_file(&path, &file)?;
                if file.metadata()?.len() != 0 {
                    return Err(invalid("connections lock is not empty"));
                }
                file
            }
            Err(error) => return Err(error),
        };
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "connections are being changed by another process",
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;
        if grapt_login::chatgpt::has_pending_switch(profile)? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "a pending ChatGPT switch must be recovered before changing connections",
            ));
        }
        Ok(Self {
            root: profile.to_path_buf(),
            _lock: lock,
            #[cfg(test)]
            failure: None,
        })
    }

    /// Absence alone returns None; corrupt or unreadable storage refuses.
    pub fn load(&self) -> io::Result<Option<Connections>> {
        Self::read_snapshot(&self.root)
    }

    /// Read a complete published generation without taking the mutation lock.
    /// Writers publish by rename, so an open handle retains either generation.
    pub fn read_snapshot(profile: &Path) -> io::Result<Option<Connections>> {
        match profile_security::read_owner_only_file(&profile.join(FILE), BYTE_CAP) {
            Ok(bytes) => {
                profile_security::verify_directory(profile)?;
                Connections::from_slice(&bytes).map(Some)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Publish one complete generation. A crash before rename preserves the old
    /// generation; the next lock owner discards the uncommitted private staging
    /// file. After rename, the new document is the sole committed generation.
    /// Mutation also requires exclusive access to this lock-owning instance:
    ///
    /// ```compile_fail
    /// use grapt_connections::store::{Connections, Store};
    /// fn share_one_lock(store: &Store, records: &Connections) {
    ///     std::thread::scope(|scope| {
    ///         scope.spawn(|| { let _ = store.save(records); });
    ///         scope.spawn(|| { let _ = store.save(records); });
    ///     });
    /// }
    /// ```
    pub fn save(&mut self, connections: &Connections) -> io::Result<()> {
        self.require_no_install()?;
        match self.publish(connections)? {
            None => Ok(()),
            Some(kind) => Err(io::Error::new(
                kind,
                "connection metadata is visible but its directory sync failed",
            )),
        }
    }

    /// Prepare a replacement live view before committing metadata. A visible
    /// post-rename sync failure is returned as a committed, uncertain result.
    pub fn update_with<T>(
        &mut self,
        connections: &Connections,
        prepare: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<(T, InstallCommit)> {
        self.require_no_install()?;
        let _checked = model::encode(connections)?;
        let prepared = prepare()?;
        let durability_error = self.publish(connections)?;
        Ok((
            prepared,
            InstallCommit {
                durability_error,
                cleanup_error: None,
            },
        ))
    }

    // Errors mean rename did not happen. A directory-sync failure after rename
    // is an explicitly visible commit whose caller must retain prepared state.
    fn publish(&mut self, connections: &Connections) -> io::Result<Option<io::ErrorKind>> {
        let bytes = model::encode(connections)?;
        self.discard_staged()?;
        let path = self.root.join(STAGED);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path)?;
        profile_security::protect_new_private_file(&path, &file)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&path, self.root.join(FILE))?;
        Ok(self.sync_publication().err().map(|error| error.kind()))
    }

    fn sync_publication(&self) -> io::Result<()> {
        #[cfg(test)]
        self.inject_failure(install::Failure::MetadataSync)?;
        sync_directory(&self.root)
    }

    #[cfg(test)]
    fn inject_failure(&self, failure: install::Failure) -> io::Result<()> {
        if self.failure == Some(failure) {
            return Err(io::Error::other("injected storage failure"));
        }
        Ok(())
    }

    /// Import the legacy provider/accounts shape exactly once, without moving
    /// credential files or rewriting user configuration. Existing storage wins.
    pub fn migrate(&mut self, legacy_jsonc: &[u8]) -> io::Result<Connections> {
        if let Some(current) = self.load()? {
            return Ok(current);
        }
        let current = migration::decode(legacy_jsonc)?;
        self.save(&current)?;
        Ok(current)
    }

    fn discard_staged(&mut self) -> io::Result<()> {
        let path = self.root.join(STAGED);
        match profile_security::read_owner_only_file(&path, BYTE_CAP) {
            Ok(_) => std::fs::remove_file(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for Store {
    /// Release the flock on shutdown. A child spawned mid-operation inherits
    /// the lock descriptor and keeps the open file description alive until it
    /// execs, which would otherwise outlive this close and hold the lock.
    fn drop(&mut self) {
        let _ = self._lock.unlock();
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;

mod cloudflare;
