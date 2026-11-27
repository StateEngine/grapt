//! Pinned plugin source snapshots.
//!
//! Admission pins every loadable module file under the plugin root before any
//! runtime or guest code runs. The bridge loader later verifies each file it
//! reads against these pins, so the bytes that execute are the bytes admitted.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub const MAX_SNAPSHOT_FILES: usize = 8_192;
pub const MAX_SNAPSHOT_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_SNAPSHOT_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_SNAPSHOT_DEPTH: usize = 32;
const MAX_RELATIVE_PATH_BYTES: usize = 1_024;
const LOADABLE_EXTENSIONS: [&str; 4] = ["js", "mjs", "cjs", "json"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PinnedFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// An immutable description of the plugin module graph admitted for execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceSnapshot {
    root: PathBuf,
    entrypoint: String,
    files: BTreeMap<String, PinnedFile>,
    digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("plugin root could not be read: {0}")]
    Read(String),
    #[error("plugin root contains a symbolic link or reparse point at {0}; vendor dependencies as regular files")]
    Link(String),
    #[error("plugin root exceeds the {MAX_SNAPSHOT_DEPTH}-directory depth bound at {0}")]
    Depth(String),
    #[error("plugin root exceeds the {MAX_SNAPSHOT_FILES}-module-file bound")]
    Files,
    #[error("plugin root exceeds the {MAX_SNAPSHOT_BYTES}-byte module bound")]
    Bytes,
    #[error("plugin module {0} exceeds the {MAX_SNAPSHOT_FILE_BYTES}-byte file bound")]
    FileBytes(String),
    #[error("plugin module path is not representable: {0}")]
    Path(String),
    #[error("plugin entrypoint {0} is not a loadable module inside its root")]
    Entrypoint(String),
}

impl SourceSnapshot {
    /// Snapshot the directory containing `entrypoint`.
    pub fn capture(entrypoint: &Path) -> Result<Self, SnapshotError> {
        let entrypoint = std::fs::canonicalize(entrypoint)
            .map_err(|error| SnapshotError::Read(error.to_string()))?;
        let root = entrypoint
            .parent()
            .ok_or_else(|| SnapshotError::Entrypoint(entrypoint.display().to_string()))?
            .to_path_buf();
        let mut files = BTreeMap::new();
        let mut total = 0u64;
        walk(&root, &root, 0, &mut files, &mut total)?;
        let relative = relative_key(&root, &entrypoint)?;
        if !files.contains_key(&relative) {
            return Err(SnapshotError::Entrypoint(relative));
        }
        let digest = snapshot_digest(&relative, &files);
        Ok(Self {
            root,
            entrypoint: relative,
            files,
            digest,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entrypoint(&self) -> &str {
        &self.entrypoint
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn files(&self) -> impl ExactSizeIterator<Item = &PinnedFile> {
        self.files.values()
    }

    /// Re-read every pinned file and prove the admitted snapshot still holds.
    pub fn verify(&self) -> Result<(), SnapshotError> {
        for file in self.files.values() {
            let path = file
                .path
                .split('/')
                .fold(self.root.clone(), |path, part| path.join(part));
            let bytes =
                std::fs::read(&path).map_err(|error| SnapshotError::Read(error.to_string()))?;
            if sha256(&bytes) != file.sha256 {
                return Err(SnapshotError::Read(format!(
                    "plugin module {} changed after trust validation",
                    file.path
                )));
            }
        }
        Ok(())
    }
}

fn walk(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut BTreeMap<String, PinnedFile>,
    total: &mut u64,
) -> Result<(), SnapshotError> {
    if depth > MAX_SNAPSHOT_DEPTH {
        return Err(SnapshotError::Depth(directory.display().to_string()));
    }
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| SnapshotError::Read(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| SnapshotError::Read(error.to_string()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| SnapshotError::Read(error.to_string()))?;
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(SnapshotError::Link(path.display().to_string()));
        }
        if metadata.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            walk(root, &path, depth + 1, files, total)?;
            continue;
        }
        if !metadata.is_file() || !loadable(&path) {
            continue;
        }
        let key = relative_key(root, &path)?;
        if metadata.len() > MAX_SNAPSHOT_FILE_BYTES {
            return Err(SnapshotError::FileBytes(key));
        }
        if files.len() >= MAX_SNAPSHOT_FILES {
            return Err(SnapshotError::Files);
        }
        let bytes = std::fs::read(&path).map_err(|error| SnapshotError::Read(error.to_string()))?;
        let length = u64::try_from(bytes.len()).map_err(|_| SnapshotError::Bytes)?;
        if length > MAX_SNAPSHOT_FILE_BYTES {
            return Err(SnapshotError::FileBytes(key));
        }
        *total = total.checked_add(length).ok_or(SnapshotError::Bytes)?;
        if *total > MAX_SNAPSHOT_BYTES {
            return Err(SnapshotError::Bytes);
        }
        files.insert(
            key.clone(),
            PinnedFile {
                path: key,
                sha256: sha256(&bytes),
                bytes: length,
            },
        );
    }
    Ok(())
}

fn loadable(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| LOADABLE_EXTENSIONS.contains(&extension))
}

fn relative_key(root: &Path, path: &Path) -> Result<String, SnapshotError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| SnapshotError::Path(path.display().to_string()))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .filter(|part| !part.contains(['/', '\\']))
                    .ok_or_else(|| SnapshotError::Path(path.display().to_string()))?,
            ),
            _ => return Err(SnapshotError::Path(path.display().to_string())),
        }
    }
    let key = parts.join("/");
    if key.is_empty() || key.len() > MAX_RELATIVE_PATH_BYTES {
        return Err(SnapshotError::Path(path.display().to_string()));
    }
    Ok(key)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn snapshot_digest(entrypoint: &str, files: &BTreeMap<String, PinnedFile>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grapt.plugin-source-snapshot.v1\n");
    hasher.update(entrypoint.as_bytes());
    hasher.update(b"\n");
    for file in files.values() {
        hasher.update(format!("{} {} {}\n", file.sha256, file.bytes, file.path).as_bytes());
    }
    hex(&hasher.finalize())
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "secode-plugin-snapshot-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules/dep"))
            .unwrap_or_else(|error| panic!("snapshot fixture root could not be created: {error}"));
        root
    }

    #[test]
    fn snapshot_pins_only_loadable_modules_and_changes_identity_on_any_module_change() {
        let root = root("identity");
        let write = |path: &str, bytes: &str| {
            std::fs::write(root.join(path), bytes)
                .unwrap_or_else(|error| panic!("fixture write failed: {error}"));
        };
        write("plugin.mjs", "export default {}");
        write("node_modules/dep/index.js", "export const one = 1");
        write("node_modules/dep/package.json", "{}");
        write("README.md", "not a module");
        let first = SourceSnapshot::capture(&root.join("plugin.mjs"))
            .unwrap_or_else(|error| panic!("snapshot failed: {error}"));
        assert_eq!(first.files().len(), 3);
        assert_eq!(first.entrypoint(), "plugin.mjs");
        write("README.md", "still not a module");
        let unchanged = SourceSnapshot::capture(&root.join("plugin.mjs"))
            .unwrap_or_else(|error| panic!("snapshot failed: {error}"));
        assert_eq!(first.digest(), unchanged.digest());
        write("node_modules/dep/index.js", "export const one = 2");
        let changed = SourceSnapshot::capture(&root.join("plugin.mjs"))
            .unwrap_or_else(|error| panic!("snapshot failed: {error}"));
        assert_ne!(first.digest(), changed.digest());
        let refused = first.verify();
        assert!(
            matches!(&refused, Err(SnapshotError::Read(detail)) if detail.contains("node_modules/dep/index.js")),
            "a substituted dependency was not refused: {refused:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_refuses_symbolic_links() {
        let root = root("link");
        std::fs::write(root.join("plugin.mjs"), "export default {}")
            .unwrap_or_else(|error| panic!("fixture write failed: {error}"));
        std::os::unix::fs::symlink("/etc/hostname", root.join("escape.js"))
            .unwrap_or_else(|error| panic!("fixture link failed: {error}"));
        let refused = SourceSnapshot::capture(&root.join("plugin.mjs"));
        assert!(
            matches!(refused, Err(SnapshotError::Link(_))),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
