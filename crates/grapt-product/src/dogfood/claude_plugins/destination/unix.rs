use nix::fcntl::{open, openat, AtFlags, OFlag};
use nix::sys::stat::{fstat, fstatat, mkdirat, Mode};
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::fd::{AsRawFd, OwnedFd},
    path::{Component, Path, PathBuf},
};

pub(in crate::dogfood::claude_plugins) struct Parent {
    chain: Vec<OwnedFd>,
    names: Vec<OsString>,
    leaf: OsString,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn access() -> OFlag {
    OFlag::O_PATH
}
#[cfg(target_vendor = "apple")]
fn access() -> OFlag {
    // Read-only directory handles permit the same no-follow operations on Apple.
    OFlag::O_RDONLY
}
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn access() -> OFlag {
    OFlag::O_RDONLY
}

fn directory(parent: &OwnedFd, name: &OsString) -> io::Result<OwnedFd> {
    openat(
        parent,
        name.as_os_str(),
        access() | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
}

impl Parent {
    /// The descriptor stays owned by this parent; path lookup cannot select a
    /// replacement of the original scratch name. Unsupported hosts refuse.
    pub(in crate::dogfood::claude_plugins) fn anchored_directory(&self) -> io::Result<PathBuf> {
        self.validate()?;
        if !cfg!(any(target_os = "linux", target_os = "android")) {
            return Err(io::Error::other(
                "anchored plugin Git acquisition is not supported on this platform yet",
            ));
        }
        let path = descriptor_path(self.directory()?);
        let held = fstat(self.directory()?).map_err(io::Error::from)?;
        let named = nix::sys::stat::stat(&path).map_err(io::Error::from)?;
        if held.st_dev != named.st_dev || held.st_ino != named.st_ino {
            return Err(io::Error::other(
                "plugin acquisition descriptor path is unavailable",
            ));
        }
        Ok(path)
    }

    pub(in crate::dogfood::claude_plugins) fn remove_directory(&self) -> io::Result<()> {
        use nix::unistd::{unlinkat, UnlinkatFlags};
        self.anchored_directory()?;
        clear_directory(self.directory()?, 0, &mut 0)?;
        self.validate()?;
        let parent = self
            .chain
            .get(self.chain.len().saturating_sub(2))
            .ok_or_else(|| io::Error::other("plugin acquisition parent is absent"))?;
        let name = self
            .names
            .last()
            .ok_or_else(|| io::Error::other("plugin acquisition name is absent"))?;
        // Only remove an empty directory under the held parent. This final
        // operation cannot follow a replacement link or traverse its target.
        unlinkat(parent, name.as_os_str(), UnlinkatFlags::RemoveDir).map_err(io::Error::from)
    }
    pub(in crate::dogfood::claude_plugins) fn open(path: &Path, create: bool) -> io::Result<Self> {
        if !path.is_absolute() || path.as_os_str().len() > 4096 {
            return Err(io::Error::other("plugin path must be absolute and bounded"));
        }
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => names.push(name.to_os_string()),
                _ => return Err(io::Error::other("plugin path must be normalized")),
            }
            if names.len() > 256 {
                return Err(io::Error::other("plugin path exceeds 256 components"));
            }
        }
        let leaf = names
            .pop()
            .ok_or_else(|| io::Error::other("plugin path must name a file"))?;
        let root = open(
            "/",
            access() | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let mut chain = vec![root];
        for name in &names {
            let parent = chain
                .last()
                .ok_or_else(|| io::Error::other("plugin parent chain is empty"))?;
            let child = match directory(parent, name) {
                Ok(child) => child,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    match mkdirat(parent, name.as_os_str(), Mode::from_bits_truncate(0o700)) {
                        Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
                        Err(error) => return Err(io::Error::from(error)),
                    }
                    directory(parent, name)?
                }
                Err(error) => return Err(error),
            };
            chain.push(child);
        }
        let parent = Self { chain, names, leaf };
        parent.validate()?;
        Ok(parent)
    }
    fn directory(&self) -> io::Result<&OwnedFd> {
        self.chain
            .last()
            .ok_or_else(|| io::Error::other("plugin parent chain is empty"))
    }
    pub(in crate::dogfood::claude_plugins) fn validate(&self) -> io::Result<()> {
        for (index, name) in self.names.iter().enumerate() {
            let parent = self
                .chain
                .get(index)
                .ok_or_else(|| io::Error::other("plugin parent chain is incomplete"))?;
            let child = self
                .chain
                .get(index + 1)
                .ok_or_else(|| io::Error::other("plugin parent chain is incomplete"))?;
            let named = fstatat(parent, name.as_os_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            let held = fstat(child).map_err(io::Error::from)?;
            if named.st_dev != held.st_dev || named.st_ino != held.st_ino {
                return Err(io::Error::other("plugin parent changed identity"));
            }
        }
        Ok(())
    }
    pub(in crate::dogfood::claude_plugins) fn create_file(&self) -> io::Result<File> {
        self.validate()?;
        let file = openat(
            self.directory()?,
            self.leaf.as_os_str(),
            OFlag::O_WRONLY
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(io::Error::from)?;
        self.validate()?;
        Ok(File::from(file))
    }
    pub(in crate::dogfood::claude_plugins) fn create_directory(&self) -> io::Result<()> {
        self.validate()?;
        mkdirat(
            self.directory()?,
            self.leaf.as_os_str(),
            Mode::from_bits_truncate(0o700),
        )
        .map_err(io::Error::from)?;
        self.validate()
    }
}

fn descriptor_path(directory: &OwnedFd) -> PathBuf {
    PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        directory.as_raw_fd()
    ))
}

fn clear_directory(parent: &OwnedFd, depth: usize, entries: &mut usize) -> io::Result<()> {
    use nix::unistd::{unlinkat, UnlinkatFlags};
    if depth > 32 {
        return Err(io::Error::other("plugin scratch cleanup exceeds depth 32"));
    }
    for entry in std::fs::read_dir(descriptor_path(parent))? {
        *entries += 1;
        if *entries > 8192 {
            return Err(io::Error::other(
                "plugin scratch cleanup exceeds 8192 entries",
            ));
        }
        let name = entry?.file_name();
        match directory(parent, &name) {
            Ok(child) => {
                clear_directory(&child, depth + 1, entries)?;
                unlinkat(parent, name.as_os_str(), UnlinkatFlags::RemoveDir)
                    .map_err(io::Error::from)?;
            }
            Err(error)
                if error.kind() == io::ErrorKind::NotADirectory
                    || error.raw_os_error() == Some(nix::libc::ELOOP) =>
            {
                unlinkat(parent, name.as_os_str(), UnlinkatFlags::NoRemoveDir)
                    .map_err(io::Error::from)?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
