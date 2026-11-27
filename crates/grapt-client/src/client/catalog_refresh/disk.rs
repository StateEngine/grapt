//! Profile-local last-known catalogs, partitioned by project and directory.
//! Cache-first loading (Apache-2.0).
// Copyright 2025 OpenAI. Modified for Grapt's versioned catalog DTOs.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::{Choice, Snapshot};
use crate::wire::{ProjectRow, ProjectWireId};

const FILE: &str = "tui-catalog.json";
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    project: ProjectWireId,
    directory: String,
    agents: serde_json::Value,
    models: serde_json::Value,
    default: serde_json::Value,
    choice: Option<Choice>,
}

pub(super) fn load(
    root: &Path,
    project: &ProjectRow,
    bound: usize,
) -> Result<Option<Snapshot>, String> {
    let Some(doc) = read(root, bound)? else {
        return Ok(None);
    };
    if doc.version != 2 || doc.project != project.id || doc.directory != project.worktree {
        return Ok(None);
    }
    snapshot(doc, bound).map(Some)
}

fn read(root: &Path, bound: usize) -> Result<Option<Document>, String> {
    let path = root.join(FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err("catalog cache is not a regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("catalog cache metadata: {:?}", error.kind())),
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("catalog cache read: {:?}", error.kind())),
    };
    let limit = bound.saturating_mul(3).saturating_add(32_768);
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err("catalog cache exceeds its byte bound".into());
    }
    let doc: Document = serde_json::from_slice(&bytes).map_err(|_| "catalog cache is invalid")?;
    Ok(Some(doc))
}

fn snapshot(doc: Document, bound: usize) -> Result<Snapshot, String> {
    let snapshot = Snapshot {
        agents: serde_json::to_vec(&doc.agents).map_err(|_| "catalog cache is invalid")?,
        models: serde_json::to_vec(&doc.models).map_err(|_| "catalog cache is invalid")?,
        default: serde_json::to_vec(&doc.default).map_err(|_| "catalog cache is invalid")?,
        choice: doc.choice,
    };
    snapshot.validate(bound)?;
    Ok(snapshot)
}

pub(super) fn save(
    root: &Path,
    project: &ProjectRow,
    snapshot: &Snapshot,
    bound: usize,
) -> Result<(), String> {
    snapshot.validate(bound)?;
    let doc = Document {
        version: 2,
        project: project.id.clone(),
        directory: project.worktree.clone(),
        agents: serde_json::from_slice(&snapshot.agents).map_err(|_| "invalid agent catalog")?,
        models: serde_json::from_slice(&snapshot.models).map_err(|_| "invalid model catalog")?,
        default: serde_json::from_slice(&snapshot.default).map_err(|_| "invalid default model")?,
        choice: snapshot.choice.clone(),
    };
    let bytes = serde_json::to_vec(&doc).map_err(|_| "catalog cache encoding failed")?;
    let number = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| "catalog cache write count exhausted")?;
    let temporary = root.join(format!(".tui-catalog-{}-{number}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("catalog cache create: {:?}", error.kind()))?;
    let outcome = (|| {
        file.write_all(&bytes)?;
        file.flush()?;
        drop(file);
        // Windows rename cannot replace an existing destination. Removing this
        // expendable cache name never truncates a symlink or hardlink target;
        // a reader in the short gap takes the ordinary cache-miss path.
        #[cfg(windows)]
        match std::fs::remove_file(root.join(FILE)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::rename(&temporary, root.join(FILE))
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome.map_err(|error| format!("catalog cache write: {:?}", error.kind()))
}
