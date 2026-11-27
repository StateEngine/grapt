//! Original patch effects and planned postimages, before presentation hooks.
use super::*;
use std::collections::BTreeMap;

pub(in crate::dogfood::tools) struct PlannedFile {
    pub path: PathBuf,
    pub content: Option<Vec<u8>>,
}

pub(in crate::dogfood::tools) struct FileExecution {
    pub output: Result<Vec<u8>, String>,
    pub applied: Vec<PathBuf>,
    pub applied_modes: BTreeMap<PathBuf, u32>,
    pub applied_steps: usize,
    /// Files the failing step had already given their planned bytes.
    pub partial: Vec<PathBuf>,
    /// The failing step created a file whose bytes may be incomplete.
    pub uncertain: bool,
}

/// Each step's file postimages in plan order; directory steps have none.
pub(in crate::dogfood::tools) struct PlannedSteps(Vec<Vec<PlannedFile>>);

impl PlannedSteps {
    pub(in crate::dogfood::tools) fn single(path: PathBuf, content: Vec<u8>) -> Self {
        Self(vec![vec![PlannedFile {
            path,
            content: Some(content),
        }]])
    }

    /// Every file any step touches, sorted and unique.
    pub(in crate::dogfood::tools) fn paths(&self) -> Vec<PathBuf> {
        self.0
            .iter()
            .flatten()
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Each file's bytes after the first `count` steps, from the last of those
    /// steps that touched it, plus the `partial` files the next step had
    /// already written. Text patches may touch one path more than once.
    pub(in crate::dogfood::tools) fn postimages(
        &self,
        count: usize,
        partial: &[PathBuf],
    ) -> BTreeMap<&Path, Option<&[u8]>> {
        let mut files = BTreeMap::new();
        for file in self.0.iter().take(count).flatten() {
            files.insert(file.path.as_path(), file.content.as_deref());
        }
        for file in self.0.get(count).into_iter().flatten() {
            if partial.contains(&file.path) {
                files.insert(file.path.as_path(), file.content.as_deref());
            }
        }
        files
    }
}

/// Read the authorized sources before the first effect. Postimage bytes are
/// derived from the patch, never attributed by sampling the filesystem later.
pub(in crate::dogfood::tools) fn planned_files(
    root: &Path,
    expected: &PatchPlan,
    input: &BoundedToolInput,
) -> Result<PlannedSteps, String> {
    let fresh = replan(root, expected, input)?;
    planned_images(root, &fresh)
}

pub(super) fn planned_images(root: &Path, fresh: &PatchPlan) -> Result<PlannedSteps, String> {
    let file = |path: &Path, content: Option<Vec<u8>>| {
        path.strip_prefix(root)
            .map(|path| PlannedFile {
                path: path.to_path_buf(),
                content,
            })
            .map_err(|_| "patch escaped workspace".to_owned())
    };
    let mut steps = Vec::with_capacity(fresh.0.len());
    for step in &fresh.0 {
        steps.push(match step {
            PatchStep::MakeDirectory { .. } => Vec::new(),
            PatchStep::Create { path, content }
            | PatchStep::TextPatchWrite { path, content, .. } => {
                vec![file(path, Some(content.as_bytes().to_vec()))?]
            }
            PatchStep::Update {
                path,
                before_sha256,
                old_text,
                new_text,
            } => vec![file(
                path,
                Some(edited_content(path, before_sha256, old_text, new_text)?.into_bytes()),
            )?],
            PatchStep::Delete { path, .. } => vec![file(path, None)?],
            PatchStep::TextPatchMove {
                from, to, content, ..
            } => vec![
                file(from, None)?,
                file(to, Some(content.as_bytes().to_vec()))?,
            ],
            PatchStep::Rename {
                from,
                to,
                before_sha256,
            } => {
                let bytes = read_bounded(from, FILE_EFFECT_MAX_BYTES)?;
                if !sha256(&bytes).eq_ignore_ascii_case(before_sha256) {
                    return Err("rename source changed before capture".into());
                }
                vec![file(from, None)?, file(to, Some(bytes))?]
            }
        });
    }
    Ok(PlannedSteps(steps))
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(in crate::dogfood::tools) fn run_observed(
    root: &Path,
    expected: &PatchPlan,
    input: &BoundedToolInput,
    expected_modes: Option<&BTreeMap<PathBuf, u32>>,
) -> Result<FileExecution, String> {
    let fresh = replan(root, expected, input)?;
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .map(Path::to_path_buf)
            .map_err(|_| "patch escaped workspace".to_owned())
    };
    let mut applied = BTreeSet::new();
    let mut applied_modes = BTreeMap::new();
    let mut applied_steps = 0;
    let mut partial = Vec::new();
    let mut uncertain = false;
    let output = run_steps(root, &fresh, |root, step, index| {
        if let Some(modes) = expected_modes {
            // A file's first touch meets its captured mode; a later text hunk
            // meets the mode this patch's previous step left.
            let sources = match step {
                PatchStep::Create { .. }
                | PatchStep::MakeDirectory { .. }
                | PatchStep::TextPatchWrite {
                    before_sha256: None,
                    ..
                } => Vec::new(),
                PatchStep::Update { path, .. }
                | PatchStep::Delete { path, .. }
                | PatchStep::TextPatchWrite { path, .. } => vec![path],
                PatchStep::Rename { from, .. } => vec![from],
                PatchStep::TextPatchMove {
                    from,
                    to,
                    destination_sha256,
                    ..
                } => match destination_sha256 {
                    Some(_) => vec![from, to],
                    None => vec![from],
                },
            };
            for source in sources {
                let path = relative(source)?;
                let expected = if applied.contains(&path) {
                    applied_modes.get(&path)
                } else {
                    modes.get(&path)
                }
                .ok_or("patch source has no captured mode")?;
                let metadata =
                    std::fs::symlink_metadata(source).map_err(|error| error.to_string())?;
                if !metadata.is_file() || is_link(&metadata) || mode(&metadata) != *expected {
                    return Err("patch source mode changed after automatic capture".into());
                }
            }
        }
        let written = |path: &Path| -> Result<u32, String> {
            std::fs::symlink_metadata(path)
                .map(|metadata| mode(&metadata))
                .map_err(|error| format!("patched file metadata unavailable: {error}"))
        };
        // Create and rename report the mode they gave their destination.
        let mut progress = Progress::default();
        let effect = match step {
            PatchStep::Create { path, content } => {
                create_observed(root, path, content.as_bytes(), None, &mut progress).map(Some)
            }
            PatchStep::Rename { from, .. } => std::fs::metadata(from)
                .map_err(|error| error.to_string())
                .and_then(|source| {
                    apply(root, step, index, &mut progress).map(|()| Some(mode(&source)))
                }),
            _ => apply(root, step, index, &mut progress).map(|()| None),
        };
        let given = match effect {
            Ok(given) => given,
            Err(error) => {
                uncertain = progress.uncertain;
                for path in &progress.written {
                    let path_mode = written(path)?;
                    let path = relative(path)?;
                    applied.insert(path.clone());
                    applied_modes.insert(path.clone(), path_mode);
                    partial.push(path);
                }
                return Err(error);
            }
        };
        match step {
            PatchStep::Create { path, .. } => {
                applied_modes.insert(relative(path)?, given.ok_or("create reported no mode")?);
            }
            PatchStep::Rename { from, to, .. } => {
                applied_modes.remove(&relative(from)?);
                applied_modes.insert(relative(to)?, given.ok_or("rename reported no mode")?);
            }
            PatchStep::TextPatchWrite { path, .. } => {
                applied_modes.insert(relative(path)?, written(path)?);
            }
            PatchStep::TextPatchMove { from, to, .. } => {
                applied_modes.remove(&relative(from)?);
                applied_modes.insert(relative(to)?, written(to)?);
            }
            PatchStep::Delete { path, .. } => {
                applied_modes.remove(&relative(path)?);
            }
            PatchStep::Update { .. } | PatchStep::MakeDirectory { .. } => {}
        }
        if !matches!(step, PatchStep::MakeDirectory { .. }) {
            for path in step.paths() {
                applied.insert(relative(path)?);
            }
        }
        applied_steps += 1;
        Ok(())
    });
    Ok(FileExecution {
        output,
        applied: applied.into_iter().collect(),
        applied_modes,
        applied_steps,
        partial,
        uncertain,
    })
}
