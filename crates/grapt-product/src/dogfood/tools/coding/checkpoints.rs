//! Per-prompt file capture and undo through the existing checkpoint owner.
mod attribution;
mod manifest;
mod observer;
mod origin;
mod owner;
mod provider;
mod receipts;

pub(in crate::dogfood) use owner::{RestoreFailure, Workspace};

use super::super::mcp_call::EffectFailure;
use super::patch;
use grapt_artifact::FilesystemArtifactStore;
use grapt_inbox::Inbox;
use grapt_types::{
    ClockAuthority, IdAuthority, MessageId, SessionId, ToolCallId, ToolFailureReason,
};
use std::path::Path;
use std::sync::{Arc, Mutex};

// Serialize the synchronous mutation/receipt interval across runtime workers.
// Formatting awaits always finish before this owner is acquired.
static FILE_PUBLICATION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

pub(in crate::dogfood::tools) struct FileContext<'a> {
    pub service: &'a Path,
    pub artifacts: &'a Arc<FilesystemArtifactStore>,
    pub inbox: &'a Mutex<Inbox>,
    pub clock: Arc<dyn ClockAuthority + Send + Sync>,
    pub ids: Arc<dyn IdAuthority + Send + Sync>,
    pub session: &'a SessionId,
    pub input: &'a MessageId,
    pub call: &'a ToolCallId,
}

/// Mark a completed intentional file effect without opening a checkpoint owner,
/// storing before-images, recording a patch receipt, or opening a disk journal.
/// Ephemeral callers cannot offer file undo, even during the current process.
pub(in crate::dogfood::tools) fn without_undo(output: Vec<u8>) -> Result<Vec<u8>, EffectFailure> {
    let annotate = || -> Result<Vec<u8>, String> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&output).map_err(|error| error.to_string())?;
        value
            .as_object_mut()
            .ok_or("file effect output is not an object")?
            .insert(
                "undoUnavailable".into(),
                "ephemeral session: automatic checkpoints and file undo are disabled".into(),
            );
        serde_json::to_vec(&value).map_err(|error| error.to_string())
    };
    annotate().map_err(|error| EffectFailure {
        reason: ToolFailureReason::EffectUnknown,
        detail: format!(
            "File effect completed, but its undo-unavailable annotation failed: {error}. \
             Inspect files before retrying; file undo is unavailable in ephemeral sessions."
        ),
    })
}

#[cfg(test)]
pub(in crate::dogfood::tools) fn run(
    context: FileContext<'_>,
    root: &Path,
    plan: &patch::PatchPlan,
    input: &grapt_runtime::BoundedToolInput,
) -> Result<Vec<u8>, EffectFailure> {
    let planned = patch::planned_files(root, plan, input)?;
    run_planned(context, root, &planned, |modes| {
        patch::run_observed(root, plan, input, modes)
    })
}

#[cfg(test)]
pub(in crate::dogfood::tools) fn run_file(
    context: FileContext<'_>,
    root: &Path,
    file: super::file_effects::PreparedFile,
) -> Result<Vec<u8>, EffectFailure> {
    let planned = file.planned(root)?;
    run_planned(context, root, &planned, |modes| file.run(root, modes))
}

#[cfg(test)]
fn run_planned(
    context: FileContext<'_>,
    root: &Path,
    planned: &patch::PlannedSteps,
    effect: impl FnOnce(
        Option<&std::collections::BTreeMap<std::path::PathBuf, u32>>,
    ) -> Result<patch::FileExecution, String>,
) -> Result<Vec<u8>, EffectFailure> {
    let captured = capture(&context, root, planned)?;
    let modes = captured.as_ref().ok().map(|(_, _, _, _, modes)| modes);
    let execution = effect(modes)?;
    finish(
        context,
        planned,
        execution,
        captured,
        &std::collections::BTreeMap::new(),
    )
}

pub(in crate::dogfood::tools) async fn run_formatted(
    context: Option<FileContext<'_>>,
    root: &Path,
    plan: &patch::PatchPlan,
    input: &grapt_runtime::BoundedToolInput,
    formatter: super::super::formatting::Context<'_>,
    diagnostics: super::super::diagnostics::Context<'_>,
) -> Result<Vec<u8>, EffectFailure> {
    let planned = patch::planned_files(root, plan, input)?;
    run_planned_formatted(
        context,
        root,
        &planned,
        |modes| patch::run_observed(root, plan, input, modes),
        formatter,
        diagnostics,
    )
    .await
}

pub(in crate::dogfood::tools) async fn run_file_formatted(
    context: Option<FileContext<'_>>,
    root: &Path,
    file: super::file_effects::PreparedFile,
    formatter: super::super::formatting::Context<'_>,
    diagnostics: super::super::diagnostics::Context<'_>,
) -> Result<Vec<u8>, EffectFailure> {
    let planned = file.planned(root)?;
    run_planned_formatted(
        context,
        root,
        &planned,
        |modes| file.run(root, modes),
        formatter,
        diagnostics,
    )
    .await
}

async fn run_planned_formatted(
    context: Option<FileContext<'_>>,
    root: &Path,
    planned: &patch::PlannedSteps,
    effect: impl FnOnce(
        Option<&std::collections::BTreeMap<std::path::PathBuf, u32>>,
    ) -> Result<patch::FileExecution, String>,
    formatter: super::super::formatting::Context<'_>,
    diagnostics: super::super::diagnostics::Context<'_>,
) -> Result<Vec<u8>, EffectFailure> {
    // Formatting awaits finish before publication. Diagnostics await only after
    // the synchronous mutation/receipt interval has released its owner.
    let formatter = formatter.prepare(planned).await;
    let (result, images) = {
        let _publication = FILE_PUBLICATION.acquire().await.map_err(|_| {
            EffectFailure::from(
                "file publication owner is unavailable; restart the service before retrying"
                    .to_owned(),
            )
        })?;
        let captured = context
            .as_ref()
            .map(|context| capture(context, root, planned))
            .transpose()?;
        let modes = captured
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|(_, _, _, _, modes)| modes);
        let mut execution = effect(modes)?;
        let formatted = formatter.apply(root, planned, &mut execution);
        let images = super::super::diagnostics::Images::published(
            planned,
            &formatted,
            &diagnostics.settings,
        );
        let result = match (context, captured) {
            (Some(context), Some(captured)) => {
                finish(context, planned, execution, captured, &formatted)
            }
            _ => execution.output.map_err(EffectFailure::from),
        };
        (result, images)
    };
    match result {
        Ok(output) => Ok(diagnostics.annotate(output, images).await),
        Err(failure) => Err(failure),
    }
}

type Capture = Result<
    (
        grapt_types::WorkspaceCheckpointId,
        manifest::Manifest,
        manifest::Manifest,
        origin::Origin,
        std::collections::BTreeMap<std::path::PathBuf, u32>,
    ),
    String,
>;

fn capture(
    context: &FileContext<'_>,
    root: &Path,
    planned: &patch::PlannedSteps,
) -> Result<Capture, EffectFailure> {
    grapt_event::session_patch::PatchReceiptFact::validate_coordinates(
        context.session,
        context.input,
        context.call,
    )
    .map_err(|error| {
        EffectFailure::from(format!(
            "File effect refused before changing files: {error}"
        ))
    })?;
    // All postimages were derived before capture, never sampled after effects.
    let paths = planned.paths();
    let captured = (|| -> Result<_, String> {
        let workspace = Workspace::open(
            context.service,
            root,
            Arc::clone(context.artifacts),
            Arc::clone(&context.clock),
            Arc::clone(&context.ids),
        )?;
        let (reference, origin) = workspace.capture(context.input, &paths)?;
        let before = manifest::load(context.artifacts, reference.checkpoint())?;
        if !before.files.iter().map(|file| &file.path).eq(&paths) {
            return Err("file checkpoint selected different paths".into());
        }
        let after = postcondition(
            context.artifacts,
            &before,
            &planned.postimages(usize::MAX, &[]),
        )?;
        // Prove the largest original receipt and this call's restore target
        // fit before effects. Actual created-file modes need at most two extra
        // decimal digits each; a partial receipt's false adds one byte.
        let target = manifest::Manifest {
            root: before.root.clone(),
            input: before.input.clone(),
            origin: Some(origin.clone()),
            files: before.files.clone(),
            expected: Some(after.files.clone()),
        };
        let reserve = (2 * after.files.len()) as u64;
        if (manifest::encoded(&target)?.len() as u64).saturating_add(reserve)
            > manifest::MAX_MANIFEST_BYTES
        {
            return Err("file undo target exceeds its manifest byte bound".into());
        }
        let maximum = attribution::AppliedPatch {
            input: context.input.clone(),
            before: Some(reference.checkpoint().clone()),
            after: Some(reference.checkpoint().clone()),
            origin: Some(origin.clone()),
            applied: paths.clone(),
            complete: true,
            unavailable: None,
        };
        if (serde_json::to_vec(&maximum.encode()?)
            .map_err(|error| error.to_string())?
            .len() as u64)
            .saturating_add(1)
            > grapt_event::session_patch::MAX_RECEIPT_BYTES
        {
            return Err("original file receipt exceeds its byte bound".into());
        }
        // Recheck the complete capture immediately before entering the effect.
        for file in &before.files {
            let current = manifest::read_file(root, &file.path)?;
            if current.as_ref().map(|(bytes, _)| manifest::hash(bytes)) != file.hash
                || current.as_ref().map_or(0, |(_, mode)| *mode) != file.mode
            {
                return Err("files changed during automatic file capture".into());
            }
        }
        let modes = before
            .files
            .iter()
            .map(|file| (file.path.clone(), file.mode))
            .collect::<std::collections::BTreeMap<_, _>>();
        Ok((reference.checkpoint().clone(), before, after, origin, modes))
    })();
    Ok(captured)
}

fn finish(
    context: FileContext<'_>,
    planned: &patch::PlannedSteps,
    execution: patch::FileExecution,
    captured: Capture,
    formatted: &std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
) -> Result<Vec<u8>, EffectFailure> {
    let complete = execution.output.is_ok();
    let restore = captured.and_then(|(reference, before, after, origin, _)| {
        // A partial patch's restore target is what its applied steps wrote.
        let after = if complete {
            after
        } else if execution.uncertain {
            return Err("a failed step created a file whose bytes are uncertain".into());
        } else {
            postcondition(
                context.artifacts,
                &before,
                &planned.postimages(execution.applied_steps, &execution.partial),
            )?
        };
        let replacements = formatted
            .iter()
            .map(|(path, bytes)| (path.as_path(), Some(bytes.as_slice())))
            .collect();
        let after = postcondition(context.artifacts, &after, &replacements)?;
        Ok((reference, after, origin))
    });
    let (before, after, origin, unavailable) = match restore {
        Ok((before, mut after, origin)) => {
            for file in &mut after.files {
                if let Some(mode) = execution.applied_modes.get(&file.path) {
                    file.mode = *mode;
                }
            }
            match manifest::save(context.artifacts, &after) {
                Ok(after) => (Some(before), Some(after), Some(origin), None),
                Err(error) => (None, None, None, Some(bounded_reason(&error))),
            }
        }
        Err(error) => (None, None, None, Some(bounded_reason(&error))),
    };
    let receipt = attribution::AppliedPatch {
        input: context.input.clone(),
        before,
        after,
        origin,
        applied: if unavailable.is_none() {
            execution.applied
        } else {
            Vec::new()
        },
        complete,
        unavailable,
    };
    // The original receipt commits before any plugin presentation hook runs.
    if let Err(error) = receipts::record(&context, &receipt) {
        return Err(EffectFailure { reason:ToolFailureReason::EffectUnknown,
            detail:format!("File effects may have applied, but their original receipt could not commit: {error}. Inspect files before continuing.") });
    }
    let output = execution.output.map_err(EffectFailure::from)?;
    match receipt.unavailable {
        Some(reason) => {
            let mut value: serde_json::Value = serde_json::from_slice(&output)
                .map_err(|error| EffectFailure::from(error.to_string()))?;
            value["undoUnavailable"] = reason.into();
            serde_json::to_vec(&value).map_err(|error| error.to_string().into())
        }
        None => Ok(output),
    }
}

fn bounded_reason(reason: &str) -> String {
    reason.chars().take(256).collect()
}

/// The capture's files with each planned postimage; untouched files keep
/// their before-image.
fn postcondition(
    artifacts: &FilesystemArtifactStore,
    before: &manifest::Manifest,
    images: &std::collections::BTreeMap<&Path, Option<&[u8]>>,
) -> Result<manifest::Manifest, String> {
    let mut after = before.clone();
    for file in &mut after.files {
        match images.get(file.path.as_path()) {
            None => {}
            Some(None) => {
                file.content = None;
                file.hash = None;
                file.mode = 0;
            }
            Some(Some(bytes)) => {
                file.content = Some(manifest::capture(
                    artifacts,
                    bytes,
                    manifest::MAX_FILE_BYTES,
                )?);
                file.hash = Some(manifest::hash(bytes));
            }
        }
    }
    Ok(after)
}

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod uncertain_tests;
