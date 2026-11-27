use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::fd::OwnedFd,
    path::{Component, Path},
};

pub(in crate::migration) struct Parent {
    chain: Vec<OwnedFd>,
    names: Vec<OsString>,
    leaf: OsString,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn access() -> OFlags {
    OFlags::PATH
}
#[cfg(target_vendor = "apple")]
fn access() -> OFlags {
    OFlags::from_bits_retain(libc::O_SEARCH as u32)
}
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn access() -> OFlags {
    OFlags::RDONLY
}

fn directory(parent: &OwnedFd, name: &OsString) -> io::Result<OwnedFd> {
    fs::openat(
        parent,
        name,
        access() | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
}

impl Parent {
    pub(in crate::migration) fn open(path: &Path, create: bool) -> io::Result<Self> {
        if !path.is_absolute() || path.as_os_str().len() > 4096 {
            return Err(io::Error::other("import path must be absolute and bounded"));
        }
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => names.push(name.to_os_string()),
                _ => return Err(io::Error::other("import path must be normalized")),
            }
            if names.len() > 256 {
                return Err(io::Error::other("import path exceeds 256 components"));
            }
        }
        let leaf = names
            .pop()
            .ok_or_else(|| io::Error::other("import path must name a file"))?;
        let root = fs::open(
            "/",
            access() | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let mut chain = vec![root];
        for name in &names {
            let parent = chain
                .last()
                .ok_or_else(|| io::Error::other("import parent chain is empty"))?;
            let child = match directory(parent, name) {
                Ok(child) => child,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    match fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
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
            .ok_or_else(|| io::Error::other("import parent chain is empty"))
    }
    fn validate(&self) -> io::Result<()> {
        for (index, name) in self.names.iter().enumerate() {
            let parent = self
                .chain
                .get(index)
                .ok_or_else(|| io::Error::other("import parent chain is incomplete"))?;
            let child = self
                .chain
                .get(index + 1)
                .ok_or_else(|| io::Error::other("import parent chain is incomplete"))?;
            let named =
                fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
            let held = fs::fstat(child).map_err(io::Error::from)?;
            if named.st_dev != held.st_dev || named.st_ino != held.st_ino {
                return Err(io::Error::other("import parent changed identity"));
            }
        }
        Ok(())
    }
    pub(in crate::migration) fn create_file(&self) -> io::Result<File> {
        self.validate()?;
        let file = fs::openat(
            self.directory()?,
            &self.leaf,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io::Error::from)?;
        self.validate()?;
        Ok(File::from(file))
    }
    pub(in crate::migration) fn read_file(&self) -> io::Result<File> {
        self.validate()?;
        let file = fs::openat(
            self.directory()?,
            &self.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        self.validate()?;
        Ok(File::from(file))
    }
    pub(in crate::migration) fn create_directory(&self) -> io::Result<()> {
        self.validate()?;
        fs::mkdirat(self.directory()?, &self.leaf, Mode::from_raw_mode(0o700))
            .map_err(io::Error::from)?;
        self.validate()
    }
}
