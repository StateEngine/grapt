//! Planned ordinary file effects share patch capture and publication primitives.
use super::*;
use std::collections::BTreeMap;

pub(in crate::dogfood::tools) struct PreparedFile {
    path: PathBuf,
    content: Vec<u8>,
    output: Vec<u8>,
    check: Check,
}

enum Check {
    Edit {
        requested: String,
        before: Vec<u8>,
    },
    Create {
        requested: String,
    },
    Overwrite {
        requested: String,
        before: FileSnapshot,
    },
}

impl PreparedFile {
    pub(in crate::dogfood::tools) fn edit(
        path: PathBuf,
        requested: String,
        before: Vec<u8>,
        content: Vec<u8>,
        output: Vec<u8>,
    ) -> Self {
        Self {
            path,
            content,
            output,
            check: Check::Edit { requested, before },
        }
    }

    pub(in crate::dogfood::tools) fn planned(
        &self,
        root: &Path,
    ) -> Result<patch::PlannedSteps, String> {
        let relative = self
            .path
            .strip_prefix(root)
            .map_err(|_| "file effect escaped workspace")?;
        Ok(patch::PlannedSteps::single(
            relative.to_path_buf(),
            self.content.clone(),
        ))
    }

    fn verify(&self, root: &Path) -> Result<(), String> {
        match &self.check {
            Check::Edit { requested, before } => {
                if resolve_existing(root, requested, true)? != self.path
                    || read_bounded(&self.path, FILE_EFFECT_MAX_BYTES)? != *before
                {
                    return Err("edit target changed before replacement; reread the file".into());
                }
            }
            Check::Create { requested } => {
                if create_target(root, requested)? != self.path {
                    return Err("write target changed after authorization".into());
                }
            }
            Check::Overwrite { requested, before } => {
                overwrite_still_approved(root, requested, &self.path, before)?;
            }
        }
        Ok(())
    }

    pub(in crate::dogfood::tools) fn run(
        self,
        root: &Path,
        expected_modes: Option<&BTreeMap<PathBuf, u32>>,
    ) -> Result<patch::FileExecution, String> {
        let relative = self
            .path
            .strip_prefix(root)
            .map_err(|_| "file effect escaped workspace")?
            .to_path_buf();
        let mut progress = patch::Progress::default();
        let mut given_mode = None;
        let output = (|| {
            self.verify(root)?;
            let create = matches!(self.check, Check::Create { .. });
            let mode = if create {
                None
            } else {
                Some(patch::mode(
                    &std::fs::symlink_metadata(&self.path).map_err(|error| error.to_string())?,
                ))
            };
            if let (Some(modes), Some(mode)) = (expected_modes, mode) {
                if modes.get(&relative) != Some(&mode) {
                    return Err("file mode changed after automatic capture".into());
                }
            }
            let verify = || {
                self.verify(root)?;
                if let Some(mode) = mode {
                    let current =
                        std::fs::symlink_metadata(&self.path).map_err(|error| error.to_string())?;
                    if patch::mode(&current) != mode {
                        return Err("file mode changed before replacement".into());
                    }
                }
                Ok(())
            };
            given_mode = if create {
                verify()?;
                Some(patch::create_observed(
                    root,
                    &self.path,
                    &self.content,
                    None,
                    &mut progress,
                )?)
            } else {
                let owner = if matches!(self.check, Check::Edit { .. }) {
                    "edit"
                } else {
                    "write"
                };
                patch::atomic_replace(root, &self.path, &self.content, owner, owner, &verify)?;
                mode
            };
            Ok(self.output)
        })();
        let complete = output.is_ok();
        Ok(patch::FileExecution {
            output,
            applied: if complete {
                vec![relative.clone()]
            } else {
                Vec::new()
            },
            applied_modes: given_mode
                .map(|mode| (relative, mode))
                .into_iter()
                .collect(),
            applied_steps: usize::from(complete),
            partial: Vec::new(),
            uncertain: progress.uncertain,
        })
    }
}

pub(in crate::dogfood::tools) fn prepare_write(
    root: &Path,
    target: &CodingTarget,
    input: &BoundedToolInput,
) -> Result<PreparedFile, String> {
    let args: WriteArguments = decode_input(input)?;
    let (path, check, output) = match target {
        CodingTarget::Write(expected) => {
            let path = create_target(root, args.path.as_str())?;
            if path != *expected {
                return Err("write target changed after authorization".into());
            }
            let output = encode(
                &serde_json::json!({"path":args.path.as_str(),"bytes":args.content.len(),"created":true}),
            )?;
            (
                path,
                Check::Create {
                    requested: args.path.as_str().to_owned(),
                },
                output,
            )
        }
        CodingTarget::Overwrite { path, before } => {
            let old = overwrite_still_approved(root, args.path.as_str(), path, before)?;
            let file = match std::str::from_utf8(&old) {
                Ok(old) => super::super::effects::edit_file_metadata(
                    args.path.as_str(),
                    old,
                    old,
                    &args.content,
                )?,
                Err(_) => serde_json::json!({"type":"update","relativePath":args.path.as_str(),
                    "filePath":args.path.as_str(),"patch":"","additions":null,"deletions":null,
                    "countsAvailable":false,"patchOmitted":true,"omittedReason":"binary content replaced; no diff"}),
            };
            let output = encode(
                &serde_json::json!({"path":args.path.as_str(),"bytes":args.content.len(),
                "created":false,"overwritten":true,"beforeBytes":old.len(),"files":[file]}),
            )?;
            (
                path.clone(),
                Check::Overwrite {
                    requested: args.path.as_str().to_owned(),
                    before: before.clone(),
                },
                output,
            )
        }
        _ => return Err("file effect received a non-write target".into()),
    };
    Ok(PreparedFile {
        path,
        content: args.content.into_bytes(),
        output,
        check,
    })
}
