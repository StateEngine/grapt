//! Shared leases for absent read-only mount targets. The last owner removes
//! only the empty inode created by this adapter, including after helper death.
//! Copyright 2025 OpenAI, Apache-2.0.
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) fn registry() -> Result<PathBuf, String> {
    let uid = nix::unistd::getuid().as_raw();
    let root = PathBuf::from(format!("/tmp/grapt-bwrap-targets-{uid}"));
    let mut options = fs::DirBuilder::new();
    use std::os::unix::fs::DirBuilderExt;
    match options.mode(0o700).create(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("sandbox target registry unavailable: {error}")),
    }
    let metadata = fs::symlink_metadata(&root).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err("sandbox target registry has an untrusted owner or permissions".into());
    }
    Ok(root)
}

fn paths(root: &Path, target: &Path) -> (PathBuf, PathBuf) {
    let mut hash = DefaultHasher::new();
    target.hash(&mut hash);
    let name = format!("{:016x}", hash.finish());
    (
        root.join(format!("{name}.state")),
        root.join(format!("{name}.lease")),
    )
}

fn open(path: &Path, create: bool) -> Result<File, std::io::Error> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
}

fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn current(file: &File, path: &Path) -> bool {
    match (file.metadata(), fs::symlink_metadata(path)) {
        (Ok(open), Ok(named)) => named.is_file() && identity(&open) == identity(&named),
        _ => false,
    }
}

fn record(target: &Path, metadata: &fs::Metadata) -> String {
    format!(
        "{} {}\n{}",
        metadata.dev(),
        metadata.ino(),
        target.display()
    )
}

fn exclusive(file: &File) -> Result<(), String> {
    for _ in 0..16 {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(format!("sandbox target lock unavailable: {error}")),
        }
    }
    Err("sandbox target setup is busy".into())
}

/// Only an empty directory with this adapter's exact inode record is
/// synthetic. Repository discovery must not treat our temporary mount target
/// as a new .git entry while a command is still using it.
pub(super) fn synthetic_directory(target: &Path) -> Result<bool, String> {
    // Ordinary populated repositories never depend on the sandbox registry.
    if fs::read_dir(target)
        .and_then(|mut entries| entries.next().transpose())
        .map_err(|error| format!("sandbox target directory unreadable: {error}"))?
        .is_some()
    {
        return Ok(false);
    }
    let root = registry()?;
    let (state, _) = paths(&root, target);
    let mut guard = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&state)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("sandbox target ownership unavailable: {error}")),
    };
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.is_dir() => metadata,
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("sandbox target unavailable: {error}")),
    };
    let mut contents = String::new();
    Read::take(&mut guard, 33000)
        .read_to_string(&mut contents)
        .map_err(|error| format!("sandbox target ownership unreadable: {error}"))?;
    if !current(&guard, &state) || contents != record(target, &metadata) {
        return Ok(false);
    }
    let empty = fs::read_dir(target)
        .and_then(|mut entries| entries.next().transpose())
        .map_err(|error| format!("sandbox target directory unreadable: {error}"))?
        .is_none();
    Ok(empty
        && current(&guard, &state)
        && fs::symlink_metadata(target)
            .is_ok_and(|named| named.is_dir() && identity(&named) == identity(&metadata)))
}

pub(super) struct Target {
    target: PathBuf,
    state: PathBuf,
    lease: PathBuf,
    file: File,
}

impl Target {
    pub fn lease_path(&self) -> &Path {
        &self.lease
    }

    pub fn acquire(root: &Path, target: &Path, create: bool) -> Result<Option<Self>, String> {
        let (state, lease) = paths(root, target);
        let mut guard = match open(&state, create) {
            Ok(file) => file,
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(error) => return Err(format!("sandbox target ownership unavailable: {error}")),
        };
        exclusive(&guard)?;
        if !current(&guard, &state) {
            return Err("sandbox target registry changed during setup".into());
        }
        let file = open(&lease, true).map_err(|e| e.to_string())?;
        file.try_lock_shared()
            .map_err(|e| format!("sandbox target lease unavailable: {e}"))?;
        if create {
            fs::create_dir(target).map_err(|e| format!("sandbox missing target changed: {e}"))?;
            let metadata = fs::symlink_metadata(target).map_err(|e| e.to_string())?;
            guard.set_len(0).map_err(|e| e.to_string())?;
            guard
                .write_all(record(target, &metadata).as_bytes())
                .map_err(|e| e.to_string())?;
            guard.flush().map_err(|e| e.to_string())?;
        } else {
            let mut contents = String::new();
            Read::take(&mut guard, 33000)
                .read_to_string(&mut contents)
                .map_err(|e| e.to_string())?;
            let matches = fs::symlink_metadata(target)
                .is_ok_and(|metadata| metadata.is_dir() && contents == record(target, &metadata));
            if !matches {
                return Ok(None);
            }
        }
        Ok(Some(Self {
            target: target.into(),
            state,
            lease,
            file,
        }))
    }

    fn clean(&self) -> Result<(), String> {
        let mut guard = match open(&self.state, false) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        // Release our lease before waiting for setup/cleanup serialization.
        // The last concurrent closer can then clean after earlier closers.
        self.file.unlock().map_err(|e| e.to_string())?;
        exclusive(&guard)?;
        if !current(&guard, &self.state) {
            return Ok(());
        }
        // An active child retains the target. Its last owner retries cleanup;
        // never unlink a mount point another namespace still uses.
        if self.file.try_lock().is_err() {
            return Ok(());
        }
        // Bubblewrap's namespace init also holds a POSIX read lock. A killed
        // helper releases its flock before its namespace has necessarily died;
        // take the conflicting write lock before unlinking anything. During
        // startup it also prevents init from accepting a now-removed target.
        let native = nix::libc::flock {
            l_type: nix::libc::F_WRLCK as i16,
            l_whence: nix::libc::SEEK_SET as i16,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };
        let mut locked = false;
        for _ in 0..16 {
            match nix::fcntl::fcntl(&self.file, nix::fcntl::FcntlArg::F_SETLK(&native)) {
                Ok(_) => {
                    locked = true;
                    break;
                }
                Err(nix::errno::Errno::EACCES | nix::errno::Errno::EAGAIN) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => {
                    return Err(format!("sandbox native target lease unavailable: {error}"))
                }
            }
        }
        if !locked {
            return Err("sandbox namespace still holds a protected target".into());
        }
        let mut contents = String::new();
        Read::take(&mut guard, 33000)
            .read_to_string(&mut contents)
            .map_err(|e| e.to_string())?;
        match fs::symlink_metadata(&self.target) {
            Ok(metadata) if metadata.is_dir() && contents == record(&self.target, &metadata) => {
                // Nonrecursive: real user data, replacements and aliases survive.
                fs::remove_dir(&self.target)
                    .map_err(|e| format!("sandbox target cleanup failed: {e}"))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Ok(()),
        }
        guard.set_len(0).map_err(|e| e.to_string())?;
        guard.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        if current(&self.file, &self.lease) {
            fs::remove_file(&self.lease).map_err(|e| e.to_string())?;
        }
        if current(&guard, &self.state) {
            fs::remove_file(&self.state).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// The trusted PID 1 retains POSIX read locks until its namespace exits.
/// Bubblewrap forbids --lock-file with --as-pid-1. Read-only, close-on-exec
/// descriptors let this helper own the same cleanup fence without exposing
/// writable registry handles to the command it spawns.
pub(super) fn native_ready(paths: &[String]) -> Result<Vec<File>, String> {
    if paths.len() > 128 {
        return Err("sandbox native lease count exceeds bound".into());
    }
    let native = nix::libc::flock {
        l_type: nix::libc::F_RDLCK as i16,
        l_whence: nix::libc::SEEK_SET as i16,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    let mut leases = Vec::with_capacity(paths.len());
    for path in paths {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("sandbox native lease absent: {error}"))?;
        if !current(&file, Path::new(path)) {
            return Err("sandbox native lease changed before startup".into());
        }
        nix::fcntl::fcntl(&file, nix::fcntl::FcntlArg::F_SETLK(&native))
            .map_err(|error| format!("sandbox native lease unavailable: {error}"))?;
        if !current(&file, Path::new(path)) {
            return Err("sandbox native lease changed during startup".into());
        }
        leases.push(file);
    }
    Ok(leases)
}

impl Drop for Target {
    fn drop(&mut self) {
        let _cleaned = self.clean();
    }
}

/// Called by the existing process owner after it has reaped a helper. A killed
/// helper releases its leases in the kernel, even when Rust destructors did not run.
pub(super) fn cleanup(paths: &[PathBuf]) -> Result<(), String> {
    let root = registry()?;
    for path in paths {
        for (depth, ancestor) in path.ancestors().enumerate() {
            if depth >= 128 {
                return Err("sandbox protected target exceeds 128 ancestors".into());
            }
            if let Some(target) = Target::acquire(&root, ancestor, false)? {
                target.clean()?;
            }
        }
    }
    Ok(())
}
