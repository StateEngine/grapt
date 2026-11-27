//! Select only files supported by committed applied-operation receipts.
// Upstream source, copyright and MIT license: see THIRD_PARTY_NOTICES.md.
// Original commit: 042d0832f5f3a73eaa039a58a7d565776f6d4290.
// Modified by Grapt: retain the earliest before-image and exact latest
// postcondition, refuse gaps, and leave conversation history unchanged.
use super::manifest::{self, FileState, Manifest};
use grapt_artifact::FilesystemArtifactStore;
use grapt_types::{MessageId, WorkspaceCheckpointId};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(super) struct AppliedPatch {
    pub input: MessageId,
    pub before: Option<WorkspaceCheckpointId>,
    pub after: Option<WorkspaceCheckpointId>,
    pub origin: Option<super::origin::Origin>,
    pub applied: Vec<PathBuf>,
    pub complete: bool,
    pub unavailable: Option<String>,
}

impl AppliedPatch {
    pub fn encode(&self) -> Result<Value, String> {
        Ok(
            json!({"origin":self.origin.as_ref().map(super::origin::Origin::encode).transpose()?,"version":1,"input":self.input.as_str(),"before":self.before.as_ref().map(WorkspaceCheckpointId::as_str),"after":self.after.as_ref().map(WorkspaceCheckpointId::as_str),"applied":self.applied,"complete":self.complete,"unavailable":self.unavailable}),
        )
    }

    pub fn decode(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .filter(|object| object.len() == 8)
            .ok_or("invalid applied patch receipt")?;
        if object.get("version").and_then(Value::as_u64) != Some(1) {
            return Err("unsupported patch receipt version".into());
        }
        let id = |key: &str| {
            object
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| value.len() <= 512)
                .ok_or("invalid patch receipt identity")
        };
        let input = id("input")?
            .parse::<MessageId>()
            .map_err(|error| error.to_string())?;
        let checkpoint = |key: &str| -> Result<Option<WorkspaceCheckpointId>, String> {
            match object.get(key) {
                Some(Value::Null) => Ok(None),
                Some(Value::String(value)) if value.len() <= 256 => {
                    WorkspaceCheckpointId::new(value)
                        .map(Some)
                        .ok_or_else(|| "invalid checkpoint identity".into())
                }
                _ => Err("invalid checkpoint reference".into()),
            }
        };
        let before = checkpoint("before")?;
        let after = checkpoint("after")?;
        let origin = match object.get("origin") {
            Some(Value::Null) => None,
            Some(value) => Some(super::origin::Origin::decode(value)?),
            None => return Err("patch receipt origin is missing".into()),
        };
        let unavailable = match object.get("unavailable") {
            Some(Value::Null) if before.is_some() && after.is_some() && origin.is_some() => None,
            Some(Value::String(reason))
                if !reason.is_empty()
                    && reason.len() <= 1024
                    && before.is_none()
                    && after.is_none()
                    && origin.is_none() =>
            {
                Some(reason.clone())
            }
            _ => return Err("invalid checkpoint availability".into()),
        };
        let complete = object
            .get("complete")
            .and_then(Value::as_bool)
            .ok_or("patch completeness is absent")?;
        let rows = object
            .get("applied")
            .and_then(Value::as_array)
            .filter(|rows| rows.len() <= manifest::MAX_FILES)
            .ok_or("applied patch path bound exceeded")?;
        let mut applied = Vec::with_capacity(rows.len());
        for row in rows {
            let path = PathBuf::from(
                row.as_str()
                    .filter(|path| path.len() <= 4096)
                    .ok_or("invalid applied patch path")?,
            );
            if applied.last().is_some_and(|last| last >= &path) {
                return Err("applied patch paths are duplicated or unsorted".into());
            }
            applied.push(path);
        }
        if applied.is_empty() && complete && unavailable.is_none() {
            return Err("a completed patch receipt has no applied paths".into());
        }
        Ok(Self {
            input,
            before,
            after,
            origin,
            applied,
            complete,
            unavailable,
        })
    }
}

#[derive(Debug)]
pub(in crate::dogfood) struct UndoSelection {
    pub input: MessageId,
    pub target: WorkspaceCheckpointId,
    pub paths: Vec<PathBuf>,
    pub incomplete_patches: usize,
}

pub(super) fn select(
    store: &FilesystemArtifactStore,
    root: &Path,
    input: &MessageId,
    receipts: &[AppliedPatch],
) -> Result<UndoSelection, String> {
    if receipts.is_empty() || receipts.len() > 128 {
        return Err("file undo requires 1–128 recorded patches".into());
    }
    let mut files: BTreeMap<PathBuf, (FileState, FileState)> = BTreeMap::new();
    let mut incomplete_patches = 0;
    let mut origin = None;
    for receipt in receipts {
        if &receipt.input != input {
            return Err("patch receipt belongs to another user prompt".into());
        }
        if let Some(reason) = &receipt.unavailable {
            return Err(format!("file undo is unavailable for this turn: {reason}"));
        }
        let captured_origin = receipt
            .origin
            .as_ref()
            .ok_or("patch receipt has no capture origin")?;
        if origin.as_ref().is_some_and(|held| held != captured_origin) {
            return Err("the worktree or Git baseline changed between this turn’s patches".into());
        }
        origin = Some(captured_origin.clone());
        let before = manifest::load(
            store,
            receipt.before.as_ref().ok_or("missing before-image")?,
        )?;
        let after = manifest::load(
            store,
            receipt.after.as_ref().ok_or("missing postcondition")?,
        )?;
        if before.root != root
            || after.root != root
            || &before.input != input
            || &after.input != input
            || before.expected.is_some()
            || after.expected.is_some()
            || before.origin.is_some()
            || after.origin.is_some()
        {
            return Err("patch checkpoint has foreign coordinates".into());
        }
        if !receipt.complete {
            incomplete_patches += 1;
        }
        for path in &receipt.applied {
            manifest::validate_relative(path)?;
            let before = before
                .files
                .iter()
                .find(|file| &file.path == path)
                .ok_or("applied path has no before-image")?;
            let after = after
                .files
                .iter()
                .find(|file| &file.path == path)
                .ok_or("applied path has no postcondition")?;
            match files.get_mut(path) {
                Some((_, held)) => {
                    if held.hash != before.hash || held.mode != before.mode {
                        return Err("file changed between the turn’s patches; automatic undo would discard another edit".into());
                    }
                    *held = after.clone();
                }
                None => {
                    if files.len() == manifest::MAX_FILES {
                        return Err("turn exceeds its 32-file undo bound".into());
                    }
                    files.insert(path.clone(), (before.clone(), after.clone()));
                }
            }
        }
    }
    files.retain(|_, (before, after)| before.hash != after.hash || before.mode != after.mode);
    if files.is_empty() {
        return Err("the turn has no net patch file changes".into());
    }
    let paths = files.keys().cloned().collect::<Vec<_>>();
    let (before, after): (Vec<_>, Vec<_>) = files.into_values().unzip();
    let target = manifest::save(
        store,
        &Manifest {
            root: root.to_path_buf(),
            input: input.clone(),
            origin,
            files: before,
            expected: Some(after),
        },
    )?;
    Ok(UndoSelection {
        input: input.clone(),
        target,
        paths,
        incomplete_patches,
    })
}
