//! Retained, non-sharing directory handles prevent namespace and reparse swaps.
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

pub(in crate::migration) struct Parent {
    _pins: Vec<File>,
    path: PathBuf,
}
fn pin(path: &Path) -> io::Result<File> {
    // Deny both write and delete sharing, including SetReparsePoint access.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        return Err(io::Error::other(
            "import parent is a link or not a directory",
        ));
    }
    Ok(file)
}
impl Parent {
    pub(in crate::migration) fn open(path: &Path, create: bool) -> io::Result<Self> {
        if !path.is_absolute() || path.as_os_str().len() > 4096 {
            return Err(io::Error::other("import path must be absolute and bounded"));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("import destination has no parent"))?;
        let mut prefix = PathBuf::new();
        let mut pins = Vec::new();
        for component in parent.components() {
            if matches!(component, Component::ParentDir | Component::CurDir) {
                return Err(io::Error::other("import path must be normalized"));
            }
            prefix.push(component);
            if matches!(component, Component::Prefix(_)) {
                continue;
            }
            let file = match pin(&prefix) {
                Ok(file) => file,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    match fs::create_dir(&prefix) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                    pin(&prefix)?
                }
                Err(error) => return Err(error),
            };
            pins.push(file);
            if pins.len() > 256 {
                return Err(io::Error::other("import path exceeds 256 components"));
            }
        }
        Ok(Self {
            _pins: pins,
            path: path.to_path_buf(),
        })
    }
    pub(in crate::migration) fn create_file(&self) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(1)
            .custom_flags(0x0020_0000)
            .open(&self.path)
    }
    pub(in crate::migration) fn read_file(&self) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(0x0020_0000)
            .open(&self.path)
    }
    pub(in crate::migration) fn create_directory(&self) -> io::Result<()> {
        fs::create_dir(&self.path)
    }
}
