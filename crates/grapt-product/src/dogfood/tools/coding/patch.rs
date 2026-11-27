//! One governed multi-file effect with full preflight and truthful partial receipts.

mod display;
pub(in crate::dogfood::tools) use display::metadata as diff_metadata;
mod receipt;
mod staging;
pub(in super::super) use receipt::{planned_files, run_observed};
pub(in super::super) use receipt::{FileExecution, PlannedSteps};
#[cfg(test)]
pub(in super::super) use staging::stage;
pub(in super::super) use staging::{atomic_replace, open_stage, stage_with_mode};

use std::collections::BTreeSet;
use std::io::Write;

use super::*;

pub(in crate::dogfood::tools) mod apply_patch;

pub(in super::super) const PATCH_DESCRIPTION: &str = "Apply 1–16 ordered create/update/delete/rename operations after preflight. Updates need a unique old_text anchor. Update/delete/rename need read_lines SHA-256. Partial failure names applied operations; stale retries refuse.";

tool_arguments! {
    pub(in super::super) struct PatchArguments => PATCH_TOOL {
        required operations: PatchOperations,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(in super::super) struct PatchOperations(Vec<PatchOperation>);

impl<'de> Deserialize<'de> for PatchOperations {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let operations = Vec::<PatchOperation>::deserialize(deserializer)?;
        if operations.is_empty() || operations.len() > 16 {
            return Err(D::Error::custom("patch requires 1–16 operations"));
        }
        Ok(Self(operations))
    }
}

impl ProviderFieldSchema for PatchOperations {
    fn provider_schema() -> serde_json::Value {
        serde_json::json!({
            "type":"array", "minItems":1, "maxItems":16,
            "items":{"oneOf":[
                {"type":"object","additionalProperties":false,"properties":{"op":{"const":"create"},"path":{"type":"string"},"content":{"type":"string"}},"required":["op","path","content"]},
                {"type":"object","additionalProperties":false,"properties":{"op":{"const":"update"},"path":{"type":"string"},"before_sha256":{"type":"string"},"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}},"required":["op","path","before_sha256","old_text","new_text"]},
                {"type":"object","additionalProperties":false,"properties":{"op":{"const":"delete"},"path":{"type":"string"},"before_sha256":{"type":"string"}},"required":["op","path","before_sha256"]},
                {"type":"object","additionalProperties":false,"properties":{"op":{"const":"rename"},"from":{"type":"string"},"to":{"type":"string"},"before_sha256":{"type":"string"}},"required":["op","from","to","before_sha256"]}
            ]}
        })
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum PatchOperation {
    Create {
        path: String,
        content: String,
    },
    Update {
        path: String,
        before_sha256: String,
        old_text: String,
        new_text: String,
    },
    Delete {
        path: String,
        before_sha256: String,
    },
    Rename {
        from: String,
        to: String,
        before_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct PatchPlan(Vec<PatchStep>, InputFormat);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputFormat {
    Operations,
    TextPatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatchStep {
    /// Text patch writes and moves bind every overwrite to its captured snapshot.
    TextPatchWrite {
        path: PathBuf,
        before_sha256: Option<String>,
        overwrite: bool,
        content: String,
    },
    TextPatchMove {
        from: PathBuf,
        to: PathBuf,
        before_sha256: String,
        destination_sha256: Option<String>,
        content: String,
    },
    MakeDirectory {
        path: PathBuf,
    },
    Create {
        path: PathBuf,
        content: String,
    },
    Update {
        path: PathBuf,
        before_sha256: String,
        old_text: String,
        new_text: String,
    },
    Delete {
        path: PathBuf,
        before_sha256: String,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
        before_sha256: String,
    },
}

impl PatchPlan {
    pub(in super::super) fn consequences(&self) -> Vec<grapt_permission::PermissionConsequence> {
        let mut consequences = Vec::new();
        for step in &self.0 {
            let (action, paths): (&str, Vec<&Path>) = match step {
                PatchStep::Create { path, .. } | PatchStep::MakeDirectory { path } => {
                    ("fs.create", vec![path])
                }
                PatchStep::Update { path, .. } => ("fs.modify", vec![path]),
                PatchStep::TextPatchWrite {
                    path,
                    before_sha256,
                    overwrite,
                    ..
                } => (
                    if *overwrite {
                        "fs.overwrite"
                    } else if before_sha256.is_some() {
                        "fs.modify"
                    } else {
                        "fs.create"
                    },
                    vec![path],
                ),
                PatchStep::TextPatchMove {
                    from,
                    to,
                    before_sha256,
                    destination_sha256,
                    content,
                } => {
                    for (action, path) in [
                        ("fs.move", from),
                        ("fs.move", to),
                        (
                            if destination_sha256.is_some() {
                                "fs.overwrite"
                            } else {
                                "fs.create"
                            },
                            to,
                        ),
                    ] {
                        consequences.push(grapt_permission::PermissionConsequence {
                            action: action.into(),
                            resource: path_text(path),
                        });
                    }
                    if sha256(content.as_bytes()) != *before_sha256 {
                        consequences.push(grapt_permission::PermissionConsequence {
                            action: "fs.modify".into(),
                            resource: path_text(from),
                        });
                    }
                    continue;
                }
                PatchStep::Delete { path, .. } => ("fs.delete", vec![path]),
                PatchStep::Rename { from, to, .. } => ("fs.move", vec![from, to]),
            };
            consequences.extend(paths.into_iter().map(|path| {
                grapt_permission::PermissionConsequence {
                    action: action.to_owned(),
                    resource: path_text(path),
                }
            }));
        }
        consequences
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let plan = plan(root, input)?;
    prepare_plan(root, plan)
}

fn prepare_plan(root: &Path, plan: PatchPlan) -> Result<PreparedEffect, String> {
    let mut paths = Vec::new();
    for step in &plan.0 {
        match step {
            PatchStep::Create { path, .. }
            | PatchStep::Update { path, .. }
            | PatchStep::TextPatchWrite { path, .. }
            | PatchStep::MakeDirectory { path }
            | PatchStep::Delete { path, .. } => paths.push(path.clone()),
            PatchStep::Rename { from, to, .. } | PatchStep::TextPatchMove { from, to, .. } => {
                paths.push(from.clone());
                paths.push(to.clone());
            }
        }
    }
    let resource = paths
        .iter()
        .map(|path| path_text(path))
        .collect::<Vec<_>>()
        .join(", ");
    let resolved = ResolvedCapability::new(
        EffectClass::FilesystemWrite,
        None,
        FilesystemAuthority::new(
            Vec::new(),
            paths
                .iter()
                .map(|path| PathIdentity::canonical(path_text(path)))
                .collect(),
            protected_paths(root),
        ),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        target: EffectTarget::Coding(CodingTarget::Patch(plan)),
        capability: CapabilityId::derive(&resolved),
        action: PATCH_TOOL.to_owned(),
        resource,
    })
}

fn plan(root: &Path, input: &BoundedToolInput) -> Result<PatchPlan, String> {
    let args: PatchArguments = decode_input(input)?;
    let mut touched = BTreeSet::new();
    let mut steps = Vec::new();
    for (index, operation) in args.operations.0.into_iter().enumerate() {
        let step = match operation {
            PatchOperation::Create { path, content } => {
                let target = destination(root, &path)?;
                PatchStep::Create {
                    path: target,
                    content,
                }
            }
            PatchOperation::Update {
                path,
                before_sha256,
                old_text,
                new_text,
            } => {
                let target = source(root, &path, &before_sha256)?;
                edited_content(&target, &before_sha256, &old_text, &new_text)?;
                PatchStep::Update {
                    path: target,
                    before_sha256,
                    old_text,
                    new_text,
                }
            }
            PatchOperation::Delete {
                path,
                before_sha256,
            } => {
                let target = source(root, &path, &before_sha256)?;
                PatchStep::Delete {
                    path: target,
                    before_sha256,
                }
            }
            PatchOperation::Rename {
                from,
                to,
                before_sha256,
            } => {
                let source = source(root, &from, &before_sha256)?;
                let target = destination(root, &to)?;
                PatchStep::Rename {
                    from: source,
                    to: target,
                    before_sha256,
                }
            }
        };
        for path in step.paths() {
            if !touched.insert(path.clone()) {
                return Err(format!(
                    "patch operation {} repeats path {}; each path may appear once",
                    index + 1,
                    path.display()
                ));
            }
        }
        steps.push(step);
    }
    Ok(PatchPlan(steps, InputFormat::Operations))
}

impl PatchStep {
    fn paths(&self) -> Vec<&PathBuf> {
        match self {
            Self::Create { path, .. }
            | Self::Update { path, .. }
            | Self::TextPatchWrite { path, .. }
            | Self::MakeDirectory { path }
            | Self::Delete { path, .. } => {
                vec![path]
            }
            Self::Rename { from, to, .. } | Self::TextPatchMove { from, to, .. } => vec![from, to],
        }
    }

    fn label(&self, root: &Path) -> String {
        self.paths()
            .iter()
            .filter_map(|path| path.strip_prefix(root).ok())
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" -> ")
    }
}

fn destination(root: &Path, requested: &str) -> Result<PathBuf, String> {
    if requested.len() > 256 {
        return Err("patch path exceeds 256 bytes".into());
    }
    create_target(root, requested)
}

fn source(root: &Path, requested: &str, expected: &str) -> Result<PathBuf, String> {
    if requested.len() > 256 {
        return Err("patch path exceeds 256 bytes".into());
    }
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("patch before_sha256 must be 64 hexadecimal characters".into());
    }
    let (path, bytes) = source_content(root, requested)?;
    let actual = sha256(&bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!(
            "patch stale source hash at {requested}; expected {expected}, actual {actual}"
        ));
    }
    Ok(path)
}

fn source_content(root: &Path, requested: &str) -> Result<(PathBuf, Vec<u8>), String> {
    if requested.len() > 256 {
        return Err("patch path exceeds 256 bytes".into());
    }
    let relative = relative_path(root, requested)?;
    reject_links(root, relative)?;
    let path = resolve_existing(root, requested, true)?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("patch source metadata unavailable: {error}"))?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("patch source must be a regular unlinked file".into());
    }
    if metadata.len() > FILE_EFFECT_MAX_BYTES {
        return Err("patch source file byte bound exceeded".into());
    }
    let bytes = read_bounded(&path, FILE_EFFECT_MAX_BYTES)
        .map_err(|error| format!("patch source: {error}"))?;
    if bytes.len() as u64 > FILE_EFFECT_MAX_BYTES {
        return Err("patch source file byte bound exceeded".into());
    }
    Ok((path, bytes))
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn edited_content(
    path: &Path,
    expected: &str,
    old_text: &str,
    new_text: &str,
) -> Result<String, String> {
    if old_text.is_empty() {
        return Err("patch update old_text must not be empty".into());
    }
    let bytes = read_bounded(path, FILE_EFFECT_MAX_BYTES)
        .map_err(|error| format!("patch update source: {error}"))?;
    if bytes.len() as u64 > FILE_EFFECT_MAX_BYTES || !sha256(&bytes).eq_ignore_ascii_case(expected)
    {
        return Err("patch update source changed after preflight".into());
    }
    let before = std::str::from_utf8(&bytes)
        .map_err(|error| format!("patch update source is not UTF-8: {error}"))?;
    match before.matches(old_text).count() {
        1 => {}
        0 => {
            return Err(
                "patch update old_text not found; reread the file and copy the exact text".into(),
            )
        }
        count => {
            return Err(format!(
            "patch update old_text occurs {count} times; add surrounding lines to make it unique"
        ))
        }
    }
    let after = before.replacen(old_text, new_text, 1);
    if after.len() as u64 > FILE_EFFECT_MAX_BYTES {
        return Err("patch update result byte bound exceeded".into());
    }
    Ok(after)
}

pub(super) fn run(
    root: &Path,
    expected: &PatchPlan,
    input: &BoundedToolInput,
) -> Result<Vec<u8>, String> {
    run_steps(
        root,
        &replan(root, expected, input)?,
        |root, step, index| apply(root, step, index, &mut Progress::default()),
    )
}

/// Repeat the entire preflight after permission. No operation starts if a
/// source changed or a destination appeared while the approval was pending.
fn replan(
    root: &Path,
    expected: &PatchPlan,
    input: &BoundedToolInput,
) -> Result<PatchPlan, String> {
    let fresh = match expected.1 {
        InputFormat::Operations => plan(root, input)?,
        InputFormat::TextPatch => apply_patch::plan(root, input)?,
    };
    if &fresh != expected {
        return Err("patch plan changed after authorization".into());
    }
    Ok(fresh)
}

fn run_steps(
    root: &Path,
    fresh: &PatchPlan,
    mut effect: impl FnMut(&Path, &PatchStep, usize) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    let display = display::prepare(root, fresh);
    let mut applied = Vec::new();
    for (index, step) in fresh.0.iter().enumerate() {
        #[cfg(feature = "loopback-provider-testkit")]
        interruption_barrier(root, index)?;
        let label = step.label(root);
        if let Err(error) = effect(root, step, index) {
            let recovery = match fresh.1 {
                InputFormat::Operations => "Inspect files and read_lines hashes before a new patch; unchanged retry refuses stale/occupied paths",
                InputFormat::TextPatch => "Inspect files before retrying; a new patch may overwrite completed additions",
            };
            return Err(format!("patch partial failure at operation {} ({label}): {error}; applied={applied:?}. {recovery}",index+1));
        }
        applied.push(label);
    }
    let mut output = serde_json::json!({"applied":applied,"complete":true});
    match display {
        Ok(files) => output["files"] = files.into(),
        Err(reason) => {
            output["diffUnavailable"] = reason.chars().take(256).collect::<String>().into()
        }
    }
    encode(&output)
}

// Feature-gated process fixture: hold operation two only after operation one
// has returned. The real runner's effects-started and recovery owner stay in use.
#[cfg(feature = "loopback-provider-testkit")]
fn interruption_barrier(root: &Path, index: usize) -> Result<(), String> {
    if index != 1 || std::env::var_os("GRAPT_NT_PATCH_INTERRUPTION") != Some("1".into()) {
        return Ok(());
    }
    let mut marker = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join(".grapt-nt-patch-entered"))
        .map_err(|error| format!("patch fixture marker unavailable: {error}"))?;
    marker
        .write_all(b"first-operation-applied\n")
        .and_then(|()| marker.sync_all())
        .map_err(|error| format!("patch fixture marker write failed: {error}"))?;
    for _ in 0..3_000 {
        if root
            .join(".grapt-nt-patch-release")
            .try_exists()
            .map_err(|error| format!("patch fixture release unavailable: {error}"))?
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("patch interruption fixture exceeded 3000 polls of 10ms".into())
}

#[cfg(test)]
pub(super) fn fail_after_first_for_test(
    root: &Path,
    input: &BoundedToolInput,
) -> Result<Vec<u8>, String> {
    let fresh = plan(root, input)?;
    run_steps(root, &fresh, |root, step, index| {
        if index == 1 {
            Err("injected filesystem failure".into())
        } else {
            apply(root, step, index, &mut Progress::default())
        }
    })
}

#[cfg(test)]
thread_local! {
    /// Fails the next create right after it opens its file, before any bytes.
    pub(in crate::dogfood::tools::coding) static CREATE_WRITE_FAULT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn injected_create_fault() -> Result<(), String> {
    if CREATE_WRITE_FAULT.with(|fault| fault.replace(false)) {
        return Err("injected create write failure".into());
    }
    Ok(())
}

/// What a failing step had already done to the workspace.
#[derive(Default)]
pub(in crate::dogfood::tools::coding) struct Progress {
    /// Files already holding this step's planned bytes.
    pub written: Vec<PathBuf>,
    /// A file was created but its bytes may be incomplete.
    pub uncertain: bool,
}

fn apply(
    root: &Path,
    step: &PatchStep,
    index: usize,
    progress: &mut Progress,
) -> Result<(), String> {
    match step {
        PatchStep::TextPatchWrite { .. }
        | PatchStep::TextPatchMove { .. }
        | PatchStep::MakeDirectory { .. } => {
            apply_patch::filesystem::apply(root, step, index, progress)
        }
        PatchStep::Create { path, content } => {
            create_observed(root, path, content.as_bytes(), None, progress).map(|_| ())
        }
        PatchStep::Update {
            path,
            before_sha256,
            old_text,
            new_text,
        } => {
            recheck_source(root, path, before_sha256)?;
            let content = edited_content(path, before_sha256, old_text, new_text)?;
            atomic_replace(
                root,
                path,
                content.as_bytes(),
                "patch",
                &format!("nt-{index}"),
                &|| Ok(()),
            )
        }
        PatchStep::Delete {
            path,
            before_sha256,
        } => {
            recheck_source(root, path, before_sha256)?;
            std::fs::remove_file(path).map_err(|error| format!("delete failed: {error}"))
        }
        PatchStep::Rename {
            from,
            to,
            before_sha256,
        } => {
            recheck_source(root, from, before_sha256)?;
            recheck_parent(root, to)?;
            // hard_link is create-new: unlike std::fs::rename on Unix it
            // cannot overwrite a destination that appears after preflight.
            // A crash or failed remove may leave both names; the next run
            // refuses the occupied destination and surfaces that state.
            std::fs::hard_link(from, to)
                .map_err(|error| format!("rename destination creation failed: {error}"))?;
            progress.written.push(to.clone());
            std::fs::remove_file(from).map_err(|error| {
                format!("rename source removal failed; both names may exist: {error}")
            })
        }
    }
}

fn recheck_source(root: &Path, path: &Path, hash: &str) -> Result<(), String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "patch source escaped workspace")?;
    source(
        root,
        relative.to_str().ok_or("patch source path is not UTF-8")?,
        hash,
    )
    .map(|_| ())
}

fn recheck_parent(root: &Path, path: &Path) -> Result<(), String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "patch target escaped workspace")?;
    let parent = relative.parent().ok_or("patch target has no parent")?;
    reject_links(root, parent)
}

pub(in crate::dogfood::tools::coding) fn mode(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        u32::from(metadata.permissions().readonly())
    }
}

fn set_mode(permissions: &mut std::fs::Permissions, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if mode > 0o777 {
            return Err("unsupported restored file mode".into());
        }
        permissions.set_mode(mode);
    }
    #[cfg(not(unix))]
    {
        if mode > 1 {
            return Err("unsupported restored file mode".into());
        }
        permissions.set_readonly(mode == 1);
    }
    Ok(())
}

/// Create-new keeps an occupied destination untouched. The mode receipt comes
/// from this effect's file handle before any bytes are written.
pub(in super::super) fn create_bytes(
    root: &Path,
    path: &Path,
    content: &[u8],
    restored_mode: Option<u32>,
) -> Result<u32, String> {
    create_observed(root, path, content, restored_mode, &mut Progress::default())
}

pub(in crate::dogfood::tools::coding) fn create_observed(
    root: &Path,
    path: &Path,
    content: &[u8],
    restored_mode: Option<u32>,
    progress: &mut Progress,
) -> Result<u32, String> {
    recheck_parent(root, path)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create refused: {error}"))?;
    progress.uncertain = true;
    #[cfg(test)]
    injected_create_fault()?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("created file metadata unavailable: {error}"))?;
    let mode = match restored_mode {
        Some(mode) => {
            let mut permissions = metadata.permissions();
            set_mode(&mut permissions, mode)?;
            file.set_permissions(permissions)
                .map_err(|error| format!("created file mode may be incomplete: {error}"))?;
            mode
        }
        None => mode(&metadata),
    };
    file.write_all(content)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("create may be incomplete: {error}"))?;
    progress.uncertain = false;
    Ok(mode)
}
