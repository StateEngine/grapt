//! Bounded prune descriptions contain no handles that could block removal.

use std::collections::BTreeMap;
use std::fs::Metadata;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::io;

use super::bounds::{TREE_DEPTH as DEPTH, TREE_ENTRIES, VERSION_ENTRIES as TOTAL_ENTRIES};

#[derive(Debug, Clone)]
struct Stamp {
    metadata: Metadata,
    #[cfg(windows)]
    volume: u64,
    #[cfg(windows)]
    index: u64,
}

impl Stamp {
    fn capture(path: &Path) -> Result<Self, String> {
        io::directory(path.parent().ok_or("prune entry has no parent")?)?;
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            format!(
                "prune entry {} cannot be inspected: {:?}",
                super::display_path(path),
                error.kind()
            )
        })?;
        #[cfg(target_os = "linux")]
        let linked = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let linked = metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0;
        if linked {
            return Err(format!(
                "prune refuses linked entry {}",
                super::display_path(path)
            ));
        }
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(format!(
                "prune refuses nonregular entry {}",
                super::display_path(path)
            ));
        }
        #[cfg(target_os = "linux")]
        if metadata.is_file() && metadata.nlink() != 1 {
            return Err(format!(
                "prune refuses hard-link alias {}",
                super::display_path(path)
            ));
        }
        if metadata.is_dir() {
            io::directory(path)?;
        }
        #[cfg(windows)]
        {
            // Query through a temporary metadata-only no-reparse handle. Close
            // it here: even a shared-delete metadata handle can block moving
            // its ancestor. The exclusive owner removes through std only.
            let file = io::snapshot_identity(path, metadata.is_dir())?;
            let metadata = file.metadata().map_err(|error| {
                format!("prune opened metadata is unreadable: {:?}", error.kind())
            })?;
            let information = winapi_util::file::information(&file).map_err(|error| {
                format!(
                    "prune identity {} is unreadable: {:?}",
                    super::display_path(path),
                    error.kind()
                )
            })?;
            if metadata.is_file() && information.number_of_links() != 1 {
                return Err(format!(
                    "prune refuses hard-link alias {}",
                    super::display_path(path)
                ));
            }
            Ok(Self {
                metadata,
                volume: information.volume_serial_number(),
                index: information.file_index(),
            })
        }
        #[cfg(target_os = "linux")]
        Ok(Self { metadata })
    }

    fn agrees(&self, named: &Self) -> bool {
        let before = &self.metadata;
        let after = &named.metadata;
        #[cfg(target_os = "linux")]
        let identity = before.dev() == after.dev() && before.ino() == after.ino();
        #[cfg(windows)]
        let identity = self.volume == named.volume && self.index == named.index;
        if !identity || before.is_dir() != after.is_dir() || before.is_file() != after.is_file() {
            return false;
        }
        if before.is_dir() {
            return true; // Removing our own children changes directory times.
        }
        #[cfg(target_os = "linux")]
        let times = before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
            && before.mode() == after.mode();
        #[cfg(windows)]
        let times = before.last_write_time() == after.last_write_time()
            && before.creation_time() == after.creation_time()
            && before.file_attributes() == after.file_attributes();
        before.len() == after.len() && times
    }
}

#[derive(Debug, Default)]
pub(super) struct Tree {
    pub(super) files: Vec<PathBuf>,
    // Children precede their parents.
    pub(super) directories: Vec<PathBuf>,
    stamps: BTreeMap<PathBuf, Stamp>,
}

pub(super) fn inspect(root: &Path, total: &mut usize) -> Result<Tree, String> {
    let mut tree = Tree::default();
    visit(root, 0, &mut 0, total, &mut tree)?;
    Ok(tree)
}

fn visit(
    path: &Path,
    depth: usize,
    entries: &mut usize,
    total: &mut usize,
    tree: &mut Tree,
) -> Result<(), String> {
    if depth > DEPTH || *entries >= TREE_ENTRIES || *total >= TOTAL_ENTRIES {
        return Err("prune tree exceeds its depth or entry bound".into());
    }
    *entries += 1;
    *total += 1;
    let stamp = Stamp::capture(path)?;
    let directory = stamp.metadata.is_dir();
    tree.stamps.insert(path.to_path_buf(), stamp);
    if !directory {
        tree.files.push(path.to_path_buf());
        return Ok(());
    }
    let children = std::fs::read_dir(path).map_err(|error| {
        format!(
            "prune directory {} cannot be read: {:?}",
            super::display_path(path),
            error.kind()
        )
    })?;
    for child in children {
        let child = child
            .map_err(|error| format!("prune directory entry is unreadable: {:?}", error.kind()))?;
        visit(&child.path(), depth + 1, entries, total, tree)?;
    }
    tree.directories.push(path.to_path_buf());
    Ok(())
}

impl Tree {
    pub(super) fn recheck(&self) -> Result<(), String> {
        for path in self.stamps.keys() {
            self.recheck_entry(path)?;
        }
        let mut counted = 0;
        for directory in &self.directories {
            for child in std::fs::read_dir(directory)
                .map_err(|error| format!("prune directory recheck failed: {:?}", error.kind()))?
            {
                counted += 1;
                if counted > TREE_ENTRIES {
                    return Err("prune directory changed beyond its entry bound".into());
                }
                let child = child
                    .map_err(|error| format!("prune entry recheck failed: {:?}", error.kind()))?;
                if !self.stamps.contains_key(&child.path()) {
                    return Err("prune directory acquired an uninspected entry".into());
                }
            }
        }
        Ok(())
    }

    pub(super) fn recheck_entry(&self, path: &Path) -> Result<(), String> {
        let expected = self
            .stamps
            .get(path)
            .ok_or("prune entry was not inspected")?;
        if !expected.agrees(&Stamp::capture(path)?) {
            return Err(format!(
                "prune entry identity or metadata changed at {}",
                super::display_path(path)
            ));
        }
        Ok(())
    }

    pub(super) fn relocate(&mut self, from: &Path, to: &Path) -> Result<(), String> {
        let moved = |path: &Path| -> Result<PathBuf, String> {
            Ok(to.join(
                path.strip_prefix(from)
                    .map_err(|_| "prune path escaped its inspected tree")?,
            ))
        };
        self.files = self
            .files
            .iter()
            .map(|path| moved(path))
            .collect::<Result<_, _>>()?;
        self.directories = self
            .directories
            .iter()
            .map(|path| moved(path))
            .collect::<Result<_, _>>()?;
        self.stamps = self
            .stamps
            .iter()
            .map(|(path, stamp)| moved(path).map(|path| (path, stamp.clone())))
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    pub(super) fn bytes(&self) -> Result<u64, String> {
        self.stamps
            .values()
            .filter(|stamp| stamp.metadata.is_file())
            .try_fold(0_u64, |sum, stamp| {
                sum.checked_add(stamp.metadata.len())
                    .ok_or_else(|| "prune byte count overflow".into())
            })
    }
}
