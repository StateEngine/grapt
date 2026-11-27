//! Immutable checkpoint manifests use the existing artifact store.
use grapt_artifact::{
    ArtifactKind, ArtifactStore, CaptureBound, FilesystemArtifactStore, ReadBudget, Sensitivity,
};
use grapt_types::{ArtifactId, ArtifactRef, CaptureDisposition, MessageId, WorkspaceCheckpointId};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub(super) const MAX_FILES: usize = 32;
pub(super) const MAX_FILE_BYTES: u64 = 1_048_576;
pub(super) const MAX_MANIFEST_BYTES: u64 = 128 * 1024;

#[derive(Clone)]
pub(super) struct FileState {
    pub path: PathBuf,
    pub content: Option<ArtifactRef>,
    pub hash: Option<String>,
    pub mode: u32,
}

#[derive(Clone)]
pub(super) struct Manifest {
    pub root: PathBuf,
    pub input: MessageId,
    pub origin: Option<super::origin::Origin>,
    pub files: Vec<FileState>,
    // None is a capture. A restore target binds the exact post-patch states;
    // the immutable target identity therefore also binds its preconditions.
    pub expected: Option<Vec<FileState>>,
}

pub(super) fn validate_relative(relative: &Path) -> Result<(), String> {
    checked_path(None, relative).map(|_| ())
}

pub(super) fn safe_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    checked_path(Some(root), relative)
}

fn checked_path(root: Option<&Path>, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty() || relative.as_os_str().len() > 4096 {
        return Err("undo path is empty or too long".into());
    }
    let mut path = root.map(Path::to_path_buf).unwrap_or_default();
    let mut depth = 0usize;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err("undo path escapes the workspace".into());
        };
        if name.to_str().is_none_or(|name| {
            crate::dogfood::legacy_names::PROJECT_DIRECTORIES
                .iter()
                .any(|project| name.eq_ignore_ascii_case(project))
                || matches!(
                    name.to_ascii_lowercase().as_str(),
                    ".git" | ".stateengine" | ".codex" | "agents.md" | "claude.md"
                )
        }) {
            return Err("undo path names protected control metadata".into());
        }
        depth += 1;
        if depth > 32 {
            return Err("undo path exceeds its depth bound".into());
        }
        path.push(name);
        if root.is_none() {
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if linked(&metadata) => {
                return Err("undo refuses links and reparse points".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("undo path is unreadable: {error}")),
        }
    }
    Ok(path)
}

pub(super) fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn hash(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn read_file(root: &Path, relative: &Path) -> Result<Option<(Vec<u8>, u32)>, String> {
    use std::io::Read;
    let path = safe_path(root, relative)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("undo file is unreadable: {error}")),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err("undo capture refuses files with multiple hard links".into());
        }
    }
    if !metadata.is_file() || linked(&metadata) || metadata.len() > MAX_FILE_BYTES {
        return Err("undo supports regular unlinked files up to 1 MiB".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::fs::File::open(&path)
        .map_err(|error| error.to_string())?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("undo file grew beyond its bound".into());
    }
    // The effect path rechecks this hash immediately before its own writes.
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = u32::from(metadata.permissions().readonly());
    Ok(Some((bytes, mode)))
}

pub(super) fn capture_files(
    store: &FilesystemArtifactStore,
    root: PathBuf,
    input: MessageId,
    paths: &[PathBuf],
) -> Result<Manifest, String> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err("undo capture requires 1–32 paths".into());
    }
    let mut seen = BTreeSet::new();
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let key = if cfg!(windows) {
            PathBuf::from(
                path.to_str()
                    .ok_or("undo path is not UTF-8")?
                    .to_lowercase(),
            )
        } else {
            path.clone()
        };
        if !seen.insert(key) {
            return Err("undo capture repeats a path".into());
        }
        let (content, hash, mode) = match read_file(&root, path)? {
            None => (None, None, 0),
            Some((bytes, mode)) => {
                let digest = hash(&bytes);
                (
                    Some(capture(store, &bytes, MAX_FILE_BYTES)?),
                    Some(digest),
                    mode,
                )
            }
        };
        files.push(FileState {
            path: path.clone(),
            content,
            hash,
            mode,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Manifest {
        root,
        input,
        origin: None,
        files,
        expected: None,
    })
}

pub(super) fn capture(
    store: &FilesystemArtifactStore,
    bytes: &[u8],
    limit: u64,
) -> Result<ArtifactRef, String> {
    let bound = CaptureBound::new(limit, 8192).ok_or("invalid checkpoint capture bound")?;
    let captured = store
        .capture(
            bytes,
            bound,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|error| error.to_string())?;
    if !captured.disposition().is_complete() {
        return Err("checkpoint capture was truncated".into());
    }
    Ok(captured.reference().clone())
}

fn file_json(file: &FileState) -> Value {
    json!({"path":file.path, "content":file.content.as_ref().map(|reference| json!({"id":reference.id().as_str(), "length":reference.length(), "digest":reference.digest().as_str()})),"sha256":file.hash,"mode":file.mode})
}

pub(super) fn save(
    store: &FilesystemArtifactStore,
    manifest: &Manifest,
) -> Result<WorkspaceCheckpointId, String> {
    let bytes = encoded(manifest)?;
    WorkspaceCheckpointId::new(capture(store, &bytes, MAX_MANIFEST_BYTES)?.id().as_str())
        .ok_or_else(|| "checkpoint identity is invalid".into())
}

pub(super) fn encoded(manifest: &Manifest) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(&json!({"version":1,"root":manifest.root,"input":manifest.input.as_str(),
        "origin":manifest.origin.as_ref().map(super::origin::Origin::encode).transpose()?,
        "files":manifest.files.iter().map(file_json).collect::<Vec<_>>(),
        "expected":manifest.expected.as_ref().map(|files| files.iter().map(file_json).collect::<Vec<_>>())})).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("checkpoint manifest exceeds its bound".into());
    }
    Ok(bytes)
}

pub(super) fn fetch(
    store: &FilesystemArtifactStore,
    reference: &ArtifactRef,
    limit: u64,
) -> Result<Vec<u8>, String> {
    if reference.length() > limit {
        return Err("checkpoint payload exceeds its bound".into());
    }
    let metadata = store
        .stat(reference.id())
        .map_err(|error| error.to_string())?;
    if metadata.reference() != *reference
        || metadata.disposition() != CaptureDisposition::Complete
        || !metadata.sensitivity().is_secret_bearing()
    {
        return Err("checkpoint artifact metadata differs from its capture".into());
    }
    let budget = ReadBudget::new(limit.max(1)).ok_or("invalid checkpoint read bound")?;
    let mut bytes = Vec::with_capacity(reference.length() as usize);
    let mut offset = 0;
    for _ in 0..256 {
        let page = store
            .fetch(reference, offset, budget)
            .map_err(|error| error.to_string())?;
        if (bytes.len() as u64).saturating_add(page.bytes().len() as u64) > limit {
            return Err("checkpoint read exceeded its bound".into());
        }
        bytes.extend_from_slice(page.bytes());
        match page.next_offset() {
            Some(next) if next > offset => offset = next,
            Some(_) => return Err("checkpoint paging did not advance".into()),
            None => return Ok(bytes),
        }
    }
    Err("checkpoint exceeded its page bound".into())
}

pub(super) fn load(
    store: &FilesystemArtifactStore,
    checkpoint: &WorkspaceCheckpointId,
) -> Result<Manifest, String> {
    let id = ArtifactId::new(checkpoint.as_str()).ok_or("invalid checkpoint artifact identity")?;
    let reference = store
        .stat(&id)
        .map_err(|error| error.to_string())?
        .reference();
    let bytes = fetch(store, &reference, MAX_MANIFEST_BYTES)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let object = value
        .as_object()
        .filter(|object| object.len() == 6)
        .ok_or("invalid checkpoint manifest object")?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported checkpoint manifest".into());
    }
    let root = object
        .get("root")
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 4096)
        .map(PathBuf::from)
        .ok_or("invalid checkpoint root")?;
    if !root.is_absolute() {
        return Err("checkpoint root is not absolute".into());
    }
    let input = object
        .get("input")
        .and_then(Value::as_str)
        .filter(|value| value.len() <= 512)
        .ok_or("missing or oversized checkpoint input")?
        .parse::<MessageId>()
        .map_err(|error| error.to_string())?;
    let origin = match object.get("origin") {
        Some(Value::Null) => None,
        Some(value) => Some(super::origin::Origin::decode(value)?),
        None => return Err("checkpoint origin field is missing".into()),
    };
    if origin
        .as_ref()
        .is_some_and(|origin| origin.worktree.root().as_canonical() != root.to_str())
    {
        return Err("checkpoint origin names another root".into());
    }
    let files = decode_files(
        &root,
        object.get("files").ok_or("missing checkpoint files")?,
    )?;
    let expected = match object.get("expected") {
        Some(Value::Null) => None,
        Some(value) => Some(decode_files(&root, value)?),
        None => return Err("missing checkpoint preconditions".into()),
    };
    if expected.as_ref().is_some_and(|expected| {
        expected.len() != files.len() || !expected.iter().zip(&files).all(|(a, b)| a.path == b.path)
    }) {
        return Err("checkpoint preconditions select different paths".into());
    }
    Ok(Manifest {
        root,
        input,
        origin,
        files,
        expected,
    })
}

fn decode_files(_root: &Path, value: &Value) -> Result<Vec<FileState>, String> {
    let rows = value
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= MAX_FILES)
        .ok_or("checkpoint path bound exceeded")?;
    let mut files = Vec::with_capacity(rows.len());
    for row in rows {
        let object = row
            .as_object()
            .filter(|object| object.len() == 4)
            .ok_or("invalid checkpoint file")?;
        let path = PathBuf::from(
            object
                .get("path")
                .and_then(Value::as_str)
                .ok_or("missing checkpoint path")?,
        );
        validate_relative(&path)?;
        if files
            .last()
            .is_some_and(|last: &FileState| last.path >= path)
        {
            return Err("checkpoint paths are duplicated or unsorted".into());
        }
        let content = match object.get("content") {
            Some(Value::Null) => None,
            Some(Value::Object(blob)) if blob.len() == 3 => {
                let id = blob
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(ArtifactId::new)
                    .ok_or("invalid checkpoint blob identity")?;
                let length = blob
                    .get("length")
                    .and_then(Value::as_u64)
                    .filter(|length| *length <= MAX_FILE_BYTES)
                    .ok_or("invalid checkpoint blob length")?;
                let digest = blob
                    .get("digest")
                    .and_then(Value::as_str)
                    .and_then(grapt_types::ArtifactDigest::new)
                    .ok_or("invalid checkpoint digest")?;
                Some(ArtifactRef::new(id, length, digest))
            }
            _ => return Err("invalid checkpoint content reference".into()),
        };
        let hash = match object.get("sha256") {
            Some(Value::Null) if content.is_none() => None,
            Some(Value::String(hash))
                if content.is_some()
                    && hash.len() == 64
                    && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
            {
                Some(hash.clone())
            }
            _ => return Err("invalid checkpoint file hash".into()),
        };
        let mode = object
            .get("mode")
            .and_then(Value::as_u64)
            .filter(|mode| *mode <= 0o777)
            .ok_or("invalid checkpoint mode")? as u32;
        files.push(FileState {
            path,
            content,
            hash,
            mode,
        });
    }
    Ok(files)
}
