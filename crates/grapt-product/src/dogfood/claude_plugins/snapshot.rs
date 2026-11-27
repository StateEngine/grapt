//! Bounded whole-tree capture. Installation and activation inspect the same bytes.
use super::destination::Parent;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const FILES: usize = 4096;
const BYTES: usize = 64 * 1024 * 1024;
const FILE_BYTES: usize = 16 * 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) struct CapturedFile {
    pub bytes: Vec<u8>,
    pub(super) executable: bool,
}
pub(super) struct Snapshot {
    pub files: BTreeMap<String, CapturedFile>,
    pub digest: String,
    pub directories: BTreeSet<String>,
    pub bytes: usize,
}
pub(super) fn io(error: std::io::Error) -> String {
    format!("plugin filesystem operation refused: {:?}", error.kind())
}
pub(super) fn unlinked(metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let linked = metadata.file_type().is_symlink();
    if linked {
        return Err("plugin tree contains a link or reparse point".into());
    }
    Ok(())
}
impl Snapshot {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn capture(root: &Path) -> Result<Self, String> {
        let meta = fs::symlink_metadata(root).map_err(io)?;
        unlinked(&meta)?;
        if !meta.is_dir() {
            return Err("plugin source must be a directory".into());
        }
        let root = fs::canonicalize(root).map_err(io)?;
        let guard = Parent::open(&root.join(".source-probe"), false).map_err(io)?;
        let mut files = BTreeMap::new();
        let mut directories = BTreeSet::new();
        let mut pending = vec![(root.clone(), 0)];
        let mut total = 0usize;
        let mut entries = 0;
        while let Some((directory, depth)) = pending.pop() {
            if depth > 32 {
                return Err("plugin tree exceeds depth 32".into());
            }
            let metadata = fs::symlink_metadata(&directory).map_err(io)?;
            unlinked(&metadata)?;
            if !metadata.is_dir() {
                return Err("plugin directory changed type".into());
            }
            for entry in fs::read_dir(&directory).map_err(io)? {
                entries += 1;
                if entries > FILES {
                    return Err("plugin tree exceeds 4096 entries".into());
                }
                let path = entry.map_err(io)?.path();
                let relative = path
                    .strip_prefix(&root)
                    .map_err(|_| "plugin source escaped its root")?;
                if relative.components().any(|part| {
                    part.as_os_str()
                        .to_str()
                        .is_none_or(|name| name.contains('\\'))
                }) {
                    return Err("plugin resource path contains a nonportable name".into());
                }
                let key = relative
                    .to_str()
                    .ok_or("plugin resource path is not UTF-8")?
                    .replace('\\', "/");
                if key.len() > 1024
                    || key.contains(':')
                    || key.contains('\\')
                    || relative
                        .components()
                        .any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err("plugin resource path is not portable and bounded".into());
                }
                let named = fs::symlink_metadata(&path).map_err(io)?;
                unlinked(&named)?;
                if named.is_dir() {
                    directories.insert(key);
                    pending.push((path, depth + 1));
                    continue;
                }
                if !named.is_file() {
                    return Err("plugin resource is not a regular file".into());
                }
                let opened =
                    super::super::source_file::open_contained(&root, relative).map_err(io)?;
                let metadata = opened.file.metadata().map_err(io)?;
                unlinked(&metadata)?;
                if !metadata.is_file() || !opened.inside(&root).map_err(io)? {
                    return Err("plugin resource changed type or escaped its root".into());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.nlink() != 1
                        || named.dev() != metadata.dev()
                        || named.ino() != metadata.ino()
                    {
                        return Err("plugin resource changed identity or has hard links".into());
                    }
                }
                #[cfg(windows)]
                {
                    if winapi_util::file::information(&opened.file)
                        .map_err(io)?
                        .number_of_links()
                        != 1
                        || same_file::Handle::from_file(opened.file.try_clone().map_err(io)?)
                            .map_err(io)?
                            != same_file::Handle::from_path(&path).map_err(io)?
                    {
                        return Err("plugin resource changed identity or has hard links".into());
                    }
                }
                let executable = executable(&metadata);
                let mut bytes = Vec::new();
                (&opened.file)
                    .take(FILE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(io)?;
                total = total.saturating_add(bytes.len());
                if bytes.len() > FILE_BYTES || total > BYTES {
                    return Err("plugin tree exceeds its file or 64 MiB bound".into());
                }
                files.insert(key, CapturedFile { bytes, executable });
            }
        }
        guard.validate().map_err(io)?;
        Self::from_files(files, directories)
    }
    /// Every acquisition path finalizes through the same package bounds and digest.
    pub(super) fn from_files(
        files: BTreeMap<String, CapturedFile>,
        directories: BTreeSet<String>,
    ) -> Result<Self, String> {
        if files.len().saturating_add(directories.len()) > FILES {
            return Err("plugin tree exceeds 4096 entries".into());
        }
        for (key, depth) in files
            .keys()
            .map(|key| (key, 33))
            .chain(directories.iter().map(|key| (key, 32)))
        {
            let path = Path::new(key);
            if key.is_empty()
                || key.len() > 1024
                || key.contains([':', '\\'])
                || key.chars().any(char::is_control)
                || path.components().count() > depth
                || path
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("plugin resource path is not portable and bounded".into());
            }
        }
        let total = files.values().try_fold(0usize, |total, file| {
            let total = total
                .checked_add(file.bytes.len())
                .ok_or("plugin byte count overflowed")?;
            if file.bytes.len() > FILE_BYTES || total > BYTES {
                return Err("plugin tree exceeds its file or 64 MiB bound");
            }
            Ok(total)
        })?;
        let digest = grapt_plugin::claude::tree_digest(
            files
                .iter()
                .map(|(path, file)| (path.as_str(), file.bytes.as_slice(), file.executable)),
        );
        Ok(Self {
            files,
            digest,
            directories,
            bytes: total,
        })
    }
    pub fn bytes(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|f| f.bytes.as_slice())
    }
    pub fn contains(&self, path: &Path) -> bool {
        let key = path.to_string_lossy().replace('\\', "/");
        key.is_empty()
            || self.directories.contains(&key)
            || self.files.contains_key(&key)
            || self.files.keys().any(|k| k.starts_with(&format!("{key}/")))
    }
    /// Create a fresh, never reused tree. Nothing here deletes an orphan or old generation.
    pub fn publish(&self, config: &Path) -> Result<PathBuf, String> {
        let cache = config.join("claude-plugins/cache");
        let probe = Parent::open(&cache.join(".probe"), true).map_err(io)?;
        drop(probe);
        if fs::read_dir(&cache).map_err(io)?.take(129).count() >= 128 {
            return Err(
                "plugin cache reached 128 retained trees; cleanup is unavailable in this slice"
                    .into(),
            );
        }
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = cache.join(format!("{}-{}-{serial}", self.digest, std::process::id()));
        Parent::open(&root, false)
            .map_err(io)?
            .create_directory()
            .map_err(io)?;
        for relative in &self.directories {
            Parent::open(&root.join(relative), true)
                .map_err(io)?
                .create_directory()
                .map_err(io)?;
        }
        for (relative, captured) in &self.files {
            let parent = Parent::open(&root.join(relative), true).map_err(io)?;
            let mut file = parent.create_file().map_err(io)?;
            file.write_all(&captured.bytes)
                .and_then(|()| file.sync_all())
                .map_err(io)?;
            readonly(&file, captured.executable)?;
            file.sync_all().map_err(io)?;
        }
        #[cfg(unix)]
        for directory in self
            .directories
            .iter()
            .rev()
            .map(|path| root.join(path))
            .chain([root.clone(), cache.clone(), config.join("claude-plugins")])
        {
            let guard = Parent::open(&directory.join(".sync-probe"), false).map_err(io)?;
            File::open(&directory)
                .and_then(|file| file.sync_all())
                .map_err(io)?;
            guard.validate().map_err(io)?;
        }
        if Snapshot::capture(&root)?.digest != self.digest {
            return Err("published plugin tree failed verification".into());
        }
        Ok(root)
    }
}
#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o100 != 0
}
#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    false
}
fn readonly(file: &File, executable: bool) -> Result<(), String> {
    let mut permissions = file.metadata().map_err(io)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(if executable { 0o500 } else { 0o400 });
    }
    #[cfg(not(unix))]
    {
        let _ = executable;
        permissions.set_readonly(true);
    }
    file.set_permissions(permissions).map_err(io)
}
