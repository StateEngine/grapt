//! Read-only, bounded content inspection independent of Git's untracked cache.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use super::Directory;

const MAX_ENTRIES: usize = 65_536;
const MAX_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_TOTAL_PATH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct Budget {
    entries: usize,
    path_bytes: usize,
}

#[derive(PartialEq, Eq)]
enum Kind {
    Directory,
    File,
    Link,
    Special,
}

pub(super) fn retained_content(
    root: &Path,
    tracked: &BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    let root = Directory::root(root)?;
    #[cfg(target_os = "macos")]
    mounted_on(&root.file)?;
    root.inspect(Path::new(""), tracked, &mut Budget::default(), 0)
}

impl Directory {
    fn inspect(
        &self,
        relative: &Path,
        tracked: &BTreeSet<PathBuf>,
        budget: &mut Budget,
        depth: usize,
    ) -> Result<Option<PathBuf>, String> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let entries = rustix::fs::Dir::read_from(&self.file)
                .map_err(|e| format!("cannot enumerate guarded directory: {e}"))?;
            for entry in entries {
                let entry =
                    entry.map_err(|e| format!("cannot read guarded directory entry: {e}"))?;
                let name = OsStr::from_bytes(entry.file_name().to_bytes());
                if matches!(name.to_str(), Some("." | "..")) {
                    continue;
                }
                if let Some(path) = self.inspect_entry(name, relative, tracked, budget, depth)? {
                    return Ok(Some(path));
                }
            }
        }
        #[cfg(windows)]
        {
            // All ancestors, including this directory, are pinned without
            // delete sharing. read_dir cannot be redirected through a swap.
            let entries = std::fs::read_dir(&self.path)
                .map_err(|e| format!("cannot enumerate pinned directory: {e}"))?;
            for entry in entries {
                let entry =
                    entry.map_err(|e| format!("cannot read pinned directory entry: {e}"))?;
                if let Some(path) =
                    self.inspect_entry(&entry.file_name(), relative, tracked, budget, depth)?
                {
                    return Ok(Some(path));
                }
            }
        }
        #[cfg(not(any(unix, windows)))]
        return Err("guarded content inspection is unsupported on this platform".into());
        #[cfg(any(unix, windows))]
        Ok(None)
    }

    fn inspect_entry(
        &self,
        name: &OsStr,
        relative: &Path,
        tracked: &BTreeSet<PathBuf>,
        budget: &mut Budget,
        depth: usize,
    ) -> Result<Option<PathBuf>, String> {
        if !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        ) || Path::new(name).components().count() != 1
        {
            return Err("directory inspection returned a non-local entry name".into());
        }
        let path = relative.join(name);
        let length = path.as_os_str().as_encoded_bytes().len();
        budget.entries += 1;
        budget.path_bytes += length;
        if budget.entries > MAX_ENTRIES
            || budget.path_bytes > MAX_TOTAL_PATH_BYTES
            || length > MAX_PATH_BYTES
            || depth >= MAX_DEPTH
        {
            return Err(
                "checkout inspection exceeds its entry/depth/path bounds; refusing removal".into(),
            );
        }
        let kind = self.entry_kind(name)?;
        let root_marker = if cfg!(windows) {
            name.to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
        } else {
            name == OsStr::new(".git")
        };
        if relative.as_os_str().is_empty() && root_marker && kind == Kind::File {
            // registered() already authenticated exactly this root marker.
            return Ok(None);
        }
        match kind {
            Kind::Directory => {
                self.inspection_child(name)?
                    .inspect(&path, tracked, budget, depth + 1)
            }
            Kind::File | Kind::Link if tracked.contains(&path) => Ok(None),
            Kind::File | Kind::Link | Kind::Special => Ok(Some(path)),
        }
    }

    #[cfg(target_os = "linux")]
    fn inspection_open(
        &self,
        name: &OsStr,
        flags: rustix::fs::OFlags,
    ) -> Result<std::fs::File, String> {
        use rustix::fs::{Mode, ResolveFlags};
        let fd = rustix::fs::openat2(
            &self.file,
            name,
            flags,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_XDEV,
        ).map_err(|error| {
            if error == rustix::io::Errno::XDEV {
                "checkout contains a descendant mount/bind-mount crossing; detach it before removal".to_owned()
            } else {
                format!("cannot verify descendant mount confinement with Linux openat2(NO_XDEV); refusing removal without a fallback: {error}")
            }
        })?;
        Ok(std::fs::File::from(fd))
    }

    #[cfg(target_os = "linux")]
    fn inspection_child(&self, name: &OsStr) -> Result<Self, String> {
        use rustix::fs::OFlags;
        // Only descendant inspection is mount-confined. Root acquisition still
        // permits the checkout itself to reside on a mounted volume.
        let file = self.inspection_open(
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        )?;
        Ok(Self { file })
    }

    #[cfg(target_os = "macos")]
    fn inspection_child(&self, name: &OsStr) -> Result<Self, String> {
        // Compare mounted-on identities of the held descriptors, not st_dev:
        // another mount of the same filesystem must still be refused. Root
        // acquisition is deliberately unchanged so mounted checkout roots work.
        let parent_mount = mounted_on(&self.file)?;
        let child = self.child(name, false)?;
        if mounted_on(&child.file)? != parent_mount {
            return Err(
                "checkout contains a descendant mount crossing; detach it before removal".into(),
            );
        }
        Ok(child)
    }

    #[cfg(windows)]
    fn inspection_child(&self, name: &OsStr) -> Result<Self, String> {
        self.child(name, false)
    }

    #[cfg(target_os = "linux")]
    fn entry_kind(&self, name: &OsStr) -> Result<Kind, String> {
        use rustix::fs::{FileType, OFlags};
        // O_PATH acquires the actual node without opening a socket, device, or
        // FIFO for I/O, and NO_XDEV rejects file bind mounts as well as directories.
        let node = self.inspection_open(name, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC)?;
        let metadata = rustix::fs::fstat(&node)
            .map_err(|e| format!("cannot inspect mount-confined node: {e}"))?;
        Ok(match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => Kind::Directory,
            FileType::RegularFile => Kind::File,
            FileType::Symlink => Kind::Link,
            _ => Kind::Special,
        })
    }

    #[cfg(target_os = "macos")]
    fn entry_kind(&self, name: &OsStr) -> Result<Kind, String> {
        use rustix::fs::{AtFlags, FileType};
        let metadata = rustix::fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| format!("cannot inspect directory entry without following links: {e}"))?;
        Ok(match FileType::from_raw_mode(metadata.st_mode) {
            FileType::Directory => Kind::Directory,
            FileType::RegularFile => Kind::File,
            FileType::Symlink => Kind::Link,
            _ => Kind::Special,
        })
    }

    #[cfg(windows)]
    fn entry_kind(&self, name: &OsStr) -> Result<Kind, String> {
        use std::os::windows::fs::MetadataExt as _;
        let metadata = std::fs::symlink_metadata(self.path.join(name))
            .map_err(|e| format!("cannot inspect pinned directory entry: {e}"))?;
        Ok(if metadata.file_attributes() & 0x0000_0400 != 0 {
            Kind::Link
        } else if metadata.is_dir() {
            Kind::Directory
        } else if metadata.is_file() {
            Kind::File
        } else {
            Kind::Special
        })
    }

    #[cfg(not(any(unix, windows)))]
    fn entry_kind(&self, _name: &OsStr) -> Result<Kind, String> {
        Err("guarded metadata inspection is unsupported on this platform".into())
    }
}

#[cfg(target_os = "macos")]
fn mounted_on(file: &std::fs::File) -> Result<Vec<std::ffi::c_char>, String> {
    let stat = rustix::fs::fstatfs(file)
        .map_err(|e| format!("cannot verify mounted-on identity for checkout inspection: {e}"))?;
    let name = &stat.f_mntonname;
    let length = name.iter().position(|byte| *byte == 0).ok_or(
        "cannot verify descendant mount confinement: unterminated mounted-on identity; refusing removal",
    )?;
    // A full buffer may have been truncated even when its last byte is NUL.
    // Compare only the meaningful prefix, never unspecified array padding.
    if length == 0 || length >= name.len() - 1 || name[0] != b'/' as std::ffi::c_char {
        return Err("cannot verify descendant mount confinement: missing or truncated mounted-on identity; refusing removal".into());
    }
    Ok(name[..length].to_vec())
}

#[cfg(all(test, target_os = "linux"))]
#[test]
fn descendant_inspection_refuses_existing_proc_mount() -> Result<(), String> {
    use rustix::fs::OFlags;
    use std::os::unix::fs::MetadataExt as _;

    let mounted = std::fs::symlink_metadata("/proc").map_err(|error| error.to_string())?;
    let filesystem_root = std::fs::metadata("/").map_err(|error| error.to_string())?;
    assert!(
        mounted.is_dir() && mounted.dev() != filesystem_root.dev(),
        "required /proc fixture is not a real descendant filesystem mount"
    );
    let root = Directory::root(Path::new("/"))?;
    let result = root.inspection_open(
        OsStr::new("proc"),
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    );
    assert!(
        result.is_err(),
        "descendant inspection must refuse the /proc mount crossing"
    );
    Ok(())
}
