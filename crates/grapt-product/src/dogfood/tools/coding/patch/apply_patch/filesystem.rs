//! Ordered file semantics through the existing snapshot-bound patch plan.
//! Copyright 2025 OpenAI.
//! Apache-2.0; see licenses/openai-codex. Modified by Grapt: preflight,
//! bounded virtual files, protected paths and explicit directory consequences.
use super::super::*;
use std::collections::BTreeMap;

pub(super) struct Planner<'a> {
    root: &'a Path,
    files: BTreeMap<PathBuf, Option<Vec<u8>>>,
    directories: BTreeSet<PathBuf>,
    read_only: BTreeSet<PathBuf>,
    steps: Vec<PatchStep>,
}
impl<'a> Planner<'a> {
    pub(super) fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: BTreeMap::new(),
            directories: BTreeSet::new(),
            read_only: BTreeSet::new(),
            steps: Vec::new(),
        }
    }
    pub(super) fn finish(self) -> Vec<PatchStep> {
        self.steps
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn location(&self, requested: &str) -> Result<(PathBuf, Vec<PathBuf>), String> {
        if requested.len() > 256 {
            return Err("patch path exceeds 256 bytes".into());
        }
        let relative = relative_path(self.root, requested)?;
        let relative: PathBuf = relative
            .components()
            .filter(|part| matches!(part, Component::Normal(_)))
            .collect();
        if relative.file_name().is_none() {
            return Err("patch path has no filename".into());
        }
        let first = relative.components().next().ok_or("patch path is empty")?;
        let top = path_text(&self.root.join(first));
        if crate::dogfood::tools::paths::profile_path(&relative)
            || crate::dogfood::tools::paths::instruction_path(&relative)
            || protected_paths(self.root).iter().any(|protected| {
                protected.as_canonical().is_some_and(|path| {
                    if cfg!(windows) {
                        path.eq_ignore_ascii_case(&top)
                    } else {
                        path == top
                    }
                })
            })
        {
            return Err("tool path names protected control metadata".into());
        }
        let parent = relative.parent().ok_or("patch path has no parent")?;
        let mut current = self.root.to_path_buf();
        let mut missing = Vec::new();
        for part in parent.components() {
            current.push(part);
            if let Some(known) = self
                .directories
                .iter()
                .find(|known| same_path(known, &current))
            {
                current = known.clone();
                continue;
            }
            let cached = self
                .files
                .iter()
                .find(|(known, _)| same_path(known, &current));
            if let Some((known, _)) = cached {
                current = known.clone();
            }
            match cached.map(|(_, bytes)| bytes) {
                Some(Some(_)) => return Err("patch parent is a file".into()),
                Some(None) => {
                    missing.push(current.clone());
                    continue;
                }
                None => {}
            }
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => {}
                Ok(_) => return Err("patch parent is not an unlinked directory".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(current.clone())
                }
                Err(error) => return Err(format!("patch parent metadata unreadable: {error}")),
            }
        }
        let planned_parent = self
            .directories
            .iter()
            .any(|parent| current.starts_with(parent));
        if missing.is_empty() && !planned_parent {
            let parent = current
                .strip_prefix(self.root)
                .map_err(|_| "patch parent escaped workspace")?;
            current = resolve_existing(
                self.root,
                parent.to_str().ok_or("patch parent is not UTF-8")?,
                true,
            )?;
        }
        let target = current.join(relative.file_name().ok_or("patch path has no filename")?);
        if let Some((known, _)) = self
            .files
            .iter()
            .find(|(known, _)| same_path(known, &target))
        {
            return Ok((known.clone(), missing));
        }
        let target = match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_file() && !is_link(&metadata) => {
                let requested = target
                    .strip_prefix(self.root)
                    .map_err(|_| "patch target escaped workspace")?;
                resolve_existing(
                    self.root,
                    requested.to_str().ok_or("patch path is not UTF-8")?,
                    true,
                )?
            }
            Ok(metadata) if is_link(&metadata) => {
                return Err("patch target is a symlink or reparse point".into())
            }
            Ok(_) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => target,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotADirectory
                    && (!missing.is_empty() || planned_parent) =>
            {
                target
            }
            Err(error) => return Err(format!("patch target metadata unreadable: {error}")),
        };
        Ok((target, missing))
    }

    fn file(&mut self, path: &Path, parent_absent: bool) -> Result<Option<Vec<u8>>, String> {
        if self.directories.contains(path) {
            return Err("patch target is a directory".into());
        }
        if let Some(bytes) = self.files.get(path) {
            return Ok(bytes.clone());
        }
        let bytes = if parent_absent
            || self
                .directories
                .iter()
                .any(|parent| path.starts_with(parent))
        {
            None
        } else {
            match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_file() && !is_link(&metadata) => {
                    if metadata.permissions().readonly() {
                        self.read_only.insert(path.to_path_buf());
                    }
                    let relative = path
                        .strip_prefix(self.root)
                        .map_err(|_| "patch source escaped workspace")?;
                    Some(
                        source_content(
                            self.root,
                            relative.to_str().ok_or("patch path is not UTF-8")?,
                        )?
                        .1,
                    )
                }
                Ok(_) => return Err("patch target must be a regular unlinked file".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(format!("patch target metadata unreadable: {error}")),
            }
        };
        self.files.insert(path.to_path_buf(), bytes.clone());
        Ok(bytes)
    }

    fn parents(&mut self, missing: Vec<PathBuf>) -> Result<(), String> {
        for path in missing {
            if self.directories.insert(path.clone()) {
                if self.directories.len() > 64 {
                    return Err("patch exceeds 64 new parent directories".into());
                }
                self.steps.push(PatchStep::MakeDirectory { path });
            }
        }
        Ok(())
    }
    pub(super) fn source(&mut self, requested: &str) -> Result<(PathBuf, Vec<u8>), String> {
        let (path, missing) = self.location(requested)?;
        let bytes = self
            .file(&path, !missing.is_empty())?
            .ok_or_else(|| format!("patch source {requested} does not exist"))?;
        Ok((path, bytes))
    }
    pub(super) fn add(&mut self, requested: &str, content: String) -> Result<(), String> {
        self.write(requested, content, true)
    }
    pub(super) fn update(&mut self, requested: &str, content: String) -> Result<(), String> {
        self.write(requested, content, false)
    }
    fn write(
        &mut self,
        requested: &str,
        content: String,
        replace_existing: bool,
    ) -> Result<(), String> {
        if content.len() as u64 > FILE_EFFECT_MAX_BYTES {
            return Err("patched file exceeds 1 MiB".into());
        }
        let (path, missing) = self.location(requested)?;
        let before = self.file(&path, !missing.is_empty())?;
        if before.is_some() && self.read_only.contains(&path) {
            return Err("patch overwrite refuses a read-only file".into());
        }
        self.parents(missing)?;
        self.steps.push(PatchStep::TextPatchWrite {
            path: path.clone(),
            before_sha256: before.as_ref().map(|bytes| sha256(bytes)),
            overwrite: replace_existing && before.is_some(),
            content: content.clone(),
        });
        self.files.insert(path, Some(content.into_bytes()));
        Ok(())
    }
    pub(super) fn delete(&mut self, requested: &str) -> Result<(), String> {
        let (path, bytes) = self.source(requested)?;
        self.steps.push(PatchStep::Delete {
            path: path.clone(),
            before_sha256: sha256(&bytes),
        });
        self.read_only.remove(&path);
        self.files.insert(path, None);
        Ok(())
    }
    pub(super) fn move_to(
        &mut self,
        from: PathBuf,
        before: &[u8],
        requested: &str,
        content: String,
    ) -> Result<(), String> {
        let (to, missing) = self.location(requested)?;
        if from == to {
            return self.update(requested, content);
        }
        let destination = self.file(&to, !missing.is_empty())?;
        if destination.is_some() && self.read_only.contains(&to) {
            return Err("patch overwrite refuses a read-only file".into());
        }
        self.parents(missing)?;
        self.steps.push(PatchStep::TextPatchMove {
            from: from.clone(),
            to: to.clone(),
            before_sha256: sha256(before),
            destination_sha256: destination.as_ref().map(|bytes| sha256(bytes)),
            content: content.clone(),
        });
        self.read_only.remove(&from);
        self.files.insert(from, None);
        self.files.insert(to, Some(content.into_bytes()));
        Ok(())
    }
}

pub(in crate::dogfood::tools::coding::patch) fn apply(
    root: &Path,
    step: &PatchStep,
    index: usize,
    progress: &mut Progress,
) -> Result<(), String> {
    match step {
        PatchStep::MakeDirectory { path } => {
            recheck_parent(root, path)?;
            std::fs::create_dir(path)
                .map_err(|error| format!("patch parent creation refused: {error}"))
        }
        PatchStep::TextPatchWrite {
            path,
            before_sha256,
            content,
            ..
        } => write(
            root,
            path,
            before_sha256.as_deref(),
            content,
            index,
            progress,
        ),
        PatchStep::TextPatchMove { .. } => move_effect(root, step, index, progress, |path| {
            std::fs::remove_file(path)
        }),
        _ => Err("patch executor received a non-text-patch step".into()),
    }
}
/// The remove seam lets the failure witness exercise a real destination write.
pub(super) fn move_effect(
    root: &Path,
    step: &PatchStep,
    index: usize,
    progress: &mut Progress,
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let PatchStep::TextPatchMove {
        from,
        to,
        before_sha256,
        destination_sha256,
        content,
    } = step
    else {
        return Err("patch move executor received a non-move step".into());
    };
    recheck_source(root, from, before_sha256)?;
    write(
        root,
        to,
        destination_sha256.as_deref(),
        content,
        index,
        progress,
    )?;
    progress.written.push(to.clone());
    recheck_source(root, from, before_sha256)
        .and_then(|()| remove(from).map_err(|error| error.to_string()))
        .map_err(|error| {
            format!(
                "move destination {} was written; source {} could not be removed: {error}",
                to.display(),
                from.display()
            )
        })
}
fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        path_text(left).eq_ignore_ascii_case(&path_text(right))
    } else {
        left == right
    }
}

fn write(
    root: &Path,
    path: &Path,
    before: Option<&str>,
    content: &str,
    index: usize,
    progress: &mut Progress,
) -> Result<(), String> {
    if let Some(hash) = before {
        recheck_source(root, path, hash)?;
        writable(path)?;
        atomic_replace(
            root,
            path,
            content.as_bytes(),
            "patch",
            &format!("text-patch-{index}"),
            &|| recheck_source(root, path, hash),
        )
    } else {
        recheck_parent(root, path)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("patch creation refused: {error}"))?;
        progress.uncertain = true;
        #[cfg(test)]
        super::super::injected_create_fault()?;
        file.write_all(content.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("patch creation may be incomplete: {error}"))?;
        progress.uncertain = false;
        Ok(())
    }
}

fn writable(path: &Path) -> Result<(), String> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.permissions().readonly() => {
            Err("patch overwrite refuses a read-only file".into())
        }
        Ok(_) => Ok(()),
        Err(error) => Err(format!("patch overwrite metadata unreadable: {error}")),
    }
}
