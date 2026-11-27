//! Keep private acquisition roots stable while Git writes inside them.
use super::destination::Parent;
use std::{
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
pub(super) struct Guard {
    parent: Option<Parent>,
    path: PathBuf,
}
#[cfg(unix)]
impl Guard {
    pub fn open(root: &Path) -> io::Result<Self> {
        let parent = Parent::open(&root.join(".ownership-probe"), false)?;
        let path = parent.anchored_directory()?;
        Ok(Self {
            parent: Some(parent),
            path,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn cleanup(mut self, _root: &Path) -> io::Result<()> {
        self.parent
            .take()
            .ok_or_else(|| io::Error::other("plugin acquisition guard is absent"))?
            .remove_directory()
    }
}
#[cfg(unix)]
impl Drop for Guard {
    fn drop(&mut self) {
        // Cancellation/unconfirmed exit must not recycle the descriptor used by
        // an in-flight Git child. The install registry caps retained attempts at
        // four; a process restart releases these intentionally retained pins.
        if let Some(parent) = self.parent.take() {
            std::mem::forget(parent);
        }
    }
}

#[cfg(windows)]
pub(super) struct Guard {
    // Default drop retains pins if acquisition is cancelled or exit is unknown.
    // Only confirmed cleanup consumes them. At most four attempts are retained;
    // restart releases pins before an operator removes abandoned directories.
    _parents: std::mem::ManuallyDrop<Parent>,
    _root: std::mem::ManuallyDrop<std::fs::File>,
    _marker: std::mem::ManuallyDrop<std::fs::File>,
    path: PathBuf,
}
#[cfg(windows)]
impl Guard {
    pub fn open(root: &Path) -> io::Result<Self> {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

        let strict = Parent::open(&root.join(".grapt-acquisition-owner"), false)?;
        // This file cannot be written or deleted while retained. It keeps the
        // root nonempty: Windows refuses FSCTL_SET_REPARSE_POINT on a nonempty
        // directory (learn.microsoft.com/windows-hardware/drivers/ifs/fsctl-set-reparse-point).
        let marker = strict.create_file()?;
        // Git needs write sharing on its repository directory to publish config.
        // Deny delete sharing so the root cannot be renamed or replaced; keep
        // every ancestor under the ordinary stricter publisher pins.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .custom_flags(0x0020_0000 | 0x0200_0000)
            .open(root)?;
        let metadata = held.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other(
                "plugin acquisition root is a link or not a directory",
            ));
        }
        let parents = Parent::open(root, false)?;
        drop(strict);
        Ok(Self {
            _parents: std::mem::ManuallyDrop::new(parents),
            _root: std::mem::ManuallyDrop::new(held),
            _marker: std::mem::ManuallyDrop::new(marker),
            path: root.to_owned(),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn cleanup(self, root: &Path) -> io::Result<()> {
        // Restore the ordinary no-write pin before releasing the nonempty
        // marker. Cleanup then uses exactly the publisher's parent fences.
        let strict = Parent::open(&root.join(".ownership-probe"), false)?;
        let Self {
            _parents: parents,
            _root: held,
            _marker: marker,
            path: _,
        } = self;
        drop(std::mem::ManuallyDrop::into_inner(marker));
        drop(std::mem::ManuallyDrop::into_inner(held));
        drop(std::mem::ManuallyDrop::into_inner(parents));
        strict.validate()?;
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if std::fs::symlink_metadata(&path)?.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
        }
        let parent = Parent::open(root, false)?;
        drop(strict);
        std::fs::remove_dir(root)?;
        parent.validate()
    }
}
