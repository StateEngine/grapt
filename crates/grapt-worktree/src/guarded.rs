//! Descriptor-relative setup access on Unix; pinned, link-free paths on Windows.
//! No guard returned here is held across an await.

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod inspection;

use std::collections::BTreeSet;
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

pub(super) fn retained_content(
    root: &Path,
    tracked: &BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    cleanup_supported()?;
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    {
        inspection::retained_content(root, tracked)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (root, tracked);
        Err("worktree cleanup is not supported on this platform: descendant mount crossings cannot be verified; remove the checkout manually".into())
    }
}

pub(super) fn cleanup_supported() -> Result<(), String> {
    if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        Ok(())
    } else {
        Err("worktree cleanup is not supported on this platform: descendant mount crossings cannot be verified; remove the checkout manually".into())
    }
}

pub(super) fn read(root: &Path, relative: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let parent = Directory::root(root)?.parent(relative, false)?;
    let file = parent.read_file(relative.file_name().ok_or("file path has no name")?)?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(format!(
            "{} must be a regular file of at most {limit} bytes",
            relative.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(format!(
                "{} is hard-linked; use an independent regular file",
                relative.display()
            ));
        }
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", relative.display()))?;
    if bytes.len() > limit {
        return Err(format!("{} grew beyond {limit} bytes", relative.display()));
    }
    Ok(bytes)
}

pub(super) fn write(root: &Path, files: &[(PathBuf, Vec<u8>)]) -> Result<(), String> {
    let root = Directory::root(root)?;
    for (relative, bytes) in files {
        let parent = root.parent(relative, true)?;
        let mut file = parent
            .create_file(relative.file_name().ok_or("file path has no name")?)
            .map_err(|e| {
                format!(
                    "cannot exclusively create setup file {} (no overwrite): {e}",
                    relative.display()
                )
            })?;
        file.write_all(bytes)
            .map_err(|e| format!("cannot write setup file {}: {e}", relative.display()))?;
    }
    Ok(())
}

struct Directory {
    #[cfg(unix)]
    file: File,
    #[cfg(windows)]
    path: PathBuf,
    #[cfg(windows)]
    pins: Vec<File>,
}

impl Directory {
    #[cfg(unix)]
    fn root(root: &Path) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("guarded file root must be absolute".into());
        }
        let fd = rustix::fs::open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| format!("cannot acquire filesystem root: {e}"))?;
        let mut directory = Self {
            file: File::from(fd),
        };
        for component in root.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => directory = directory.child(name, false)?,
                _ => return Err("guarded root has a non-normal component".into()),
            }
        }
        Ok(directory)
    }

    #[cfg(windows)]
    fn root(root: &Path) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("guarded file root must be absolute".into());
        }
        let mut path = PathBuf::new();
        let mut pins = Vec::new();
        for component in root.components() {
            path.push(component);
            if !matches!(component, Component::Prefix(_)) {
                pins.push(windows_node(&path, true)?);
            }
        }
        Ok(Self { path, pins })
    }

    #[cfg(not(any(unix, windows)))]
    fn root(_root: &Path) -> Result<Self, String> {
        Err("guarded file access requires Unix descriptors or Windows pinned handles".into())
    }

    fn parent(&self, relative: &Path, create: bool) -> Result<Self, String> {
        let mut directory = self.duplicate()?;
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                let Component::Normal(name) = component else {
                    return Err("setup path must contain only normal relative components".into());
                };
                directory = directory.child(name, create)?;
            }
        }
        Ok(directory)
    }

    #[cfg(unix)]
    fn child(&self, name: &std::ffi::OsStr, create: bool) -> Result<Self, String> {
        if create {
            match rustix::fs::mkdirat(&self.file, name, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(format!("cannot create setup parent: {error}")),
            }
        }
        let fd = rustix::fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| format!("cannot acquire link-free setup directory: {e}"))?;
        Ok(Self {
            file: File::from(fd),
        })
    }

    #[cfg(windows)]
    fn child(&self, name: &std::ffi::OsStr, create: bool) -> Result<Self, String> {
        let path = self.path.join(name);
        if create {
            match std::fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("cannot create setup parent: {error}")),
            }
        }
        let mut directory = self.duplicate()?;
        directory.pins.push(windows_node(&path, true)?);
        directory.path = path;
        Ok(directory)
    }

    #[cfg(not(any(unix, windows)))]
    fn child(&self, _name: &std::ffi::OsStr, _create: bool) -> Result<Self, String> {
        Err("guarded directories are unsupported on this platform".into())
    }

    fn duplicate(&self) -> Result<Self, String> {
        #[cfg(unix)]
        {
            Ok(Self {
                file: self.file.try_clone().map_err(|e| e.to_string())?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                path: self.path.clone(),
                pins: self
                    .pins
                    .iter()
                    .map(File::try_clone)
                    .collect::<Result<_, _>>()
                    .map_err(|e| e.to_string())?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err("guarded directories are unsupported on this platform".into())
        }
    }

    #[cfg(unix)]
    fn read_file(&self, name: &std::ffi::OsStr) -> Result<File, String> {
        // Refuse visible special files before opening; O_NONBLOCK also prevents
        // a concurrently substituted FIFO from waiting for a writer. O_NOFOLLOW
        // applies to the actual open, not merely the preceding stat.
        let before = rustix::fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| format!("cannot inspect setup source: {e}"))?;
        if !FileType::from_raw_mode(before.st_mode).is_file() {
            return Err("setup source must be a link-free regular file".into());
        }
        let fd = rustix::fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| format!("cannot open setup source: {e}"))?;
        let after = rustix::fs::fstat(&fd)
            .map_err(|e| format!("cannot inspect opened setup source: {e}"))?;
        if !FileType::from_raw_mode(after.st_mode).is_file()
            || before.st_dev != after.st_dev
            || before.st_ino != after.st_ino
        {
            return Err(
                "setup source changed while opening; retry with a stable regular file".into(),
            );
        }
        Ok(File::from(fd))
    }

    #[cfg(windows)]
    fn read_file(&self, name: &std::ffi::OsStr) -> Result<File, String> {
        windows_node(&self.path.join(name), false)
    }

    #[cfg(not(any(unix, windows)))]
    fn read_file(&self, _name: &std::ffi::OsStr) -> Result<File, String> {
        Err("guarded reads are unsupported on this platform".into())
    }

    #[cfg(unix)]
    fn create_file(&self, name: &std::ffi::OsStr) -> Result<File, String> {
        // The already-open directory is the write authority. Exclusive creation
        // cannot open an existing symlink, special file, or tracked/user file.
        let fd = rustix::fs::openat(
            &self.file,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| e.to_string())?;
        Ok(File::from(fd))
    }

    #[cfg(windows)]
    fn create_file(&self, name: &std::ffi::OsStr) -> Result<File, String> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(self.path.join(name))
            .map_err(|e| e.to_string())
    }

    #[cfg(not(any(unix, windows)))]
    fn create_file(&self, _name: &std::ffi::OsStr) -> Result<File, String> {
        Err("guarded writes are unsupported on this platform".into())
    }
}

#[cfg(windows)]
fn windows_node(path: &Path, directory: bool) -> Result<File, String> {
    use std::os::windows::fs::MetadataExt as _;
    // Denying delete sharing pins every ancestor against rename/replacement.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(if directory { 3 } else { 1 })
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)
        .map_err(|e| format!("cannot pin link-free path {}: {e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if metadata.file_attributes() & 0x0000_0400 != 0
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err("setup path contains a link, junction, or non-regular file".into());
    }
    Ok(file)
}
