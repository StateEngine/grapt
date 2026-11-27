//! Scoped patch checkpoints implement the existing provider/restore protocol.
use super::manifest::{self, FileState, Manifest};
use grapt_artifact::FilesystemArtifactStore;
use grapt_checkpoint::*;
use grapt_types::*;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) struct Provider {
    pub artifacts: Arc<FilesystemArtifactStore>,
    pub root: PathBuf,
    // Plan and apply run under the product's existing engine mutex. This is
    // used only for the engine's automatically minted rescue capture.
    pub planned: Option<(WorkspaceCheckpointId, Manifest)>,
}

impl core::fmt::Debug for Provider {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Captured hashes and lengths describe secret-bearing before-images.
        formatter
            .debug_struct("PatchCheckpointProvider")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

pub(super) fn lane() -> Result<CheckpointLaneId, ProviderRefusal> {
    CheckpointLaneId::new("patch-files").ok_or(ProviderRefusal::LaneUnknown)
}

impl Provider {
    fn load(&self, checkpoint: &WorkspaceCheckpointId) -> Result<Manifest, ProviderRefusal> {
        let manifest = manifest::load(&self.artifacts, checkpoint)
            .map_err(|_| ProviderRefusal::CheckpointUnknown)?;
        if manifest.root != self.root {
            return Err(ProviderRefusal::WrongRoot);
        }
        Ok(manifest)
    }

    fn current_matches(&self, files: &[FileState]) -> Result<bool, ProviderRefusal> {
        for file in files {
            let current = manifest::read_file(&self.root, &file.path)
                .map_err(|_| ProviderRefusal::Unavailable)?;
            match (&current, &file.hash) {
                (None, None) => {}
                (Some((bytes, mode)), Some(hash))
                    if manifest::hash(bytes) == *hash && *mode == file.mode => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn consequences(manifest: &Manifest) -> Result<RestoreConsequences, ProviderRefusal> {
        let expected = manifest
            .expected
            .as_ref()
            .ok_or(ProviderRefusal::Conflict)?;
        let mut writes = 0;
        let mut deletes = 0;
        let mut bytes = 0;
        for (before, after) in manifest.files.iter().zip(expected) {
            if before.hash == after.hash && before.mode == after.mode {
                continue;
            }
            match &before.content {
                Some(content) => {
                    writes += 1;
                    bytes += content.length();
                }
                None if after.content.is_some() => deletes += 1,
                None => {}
            }
        }
        Ok(RestoreConsequences::new(writes, deletes, bytes))
    }

    fn apply(&self, plan: &SettledRestorePlan) -> Result<(), String> {
        let manifest = self
            .load(plan.checkpoint().checkpoint())
            .map_err(|error| format!("{error:?}"))?;
        let expected = manifest
            .expected
            .as_ref()
            .ok_or("undo target has no post-patch preconditions")?;
        if !self
            .current_matches(expected)
            .map_err(|error| format!("{error:?}"))?
        {
            return Err("files changed after the undo preview".into());
        }
        // Verify every before-image before the first filesystem effect.
        let payloads = manifest
            .files
            .iter()
            .map(|file| {
                file.content
                    .as_ref()
                    .map(|reference| {
                        manifest::fetch(&self.artifacts, reference, manifest::MAX_FILE_BYTES)
                    })
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (file, bytes) in manifest.files.iter().zip(&payloads) {
            if bytes.as_ref().map(|bytes| manifest::hash(bytes)) != file.hash {
                return Err("checkpoint before-image hash differs from its receipt".into());
            }
        }
        for (index, ((before, expected), bytes)) in manifest
            .files
            .iter()
            .zip(expected)
            .zip(payloads)
            .enumerate()
        {
            if before.hash == expected.hash && before.mode == expected.mode {
                continue;
            }
            let path = manifest::safe_path(&self.root, &before.path)?;
            let check = || -> Result<(), String> {
                if self
                    .current_matches(std::slice::from_ref(expected))
                    .map_err(|error| format!("{error:?}"))?
                {
                    Ok(())
                } else {
                    Err("file changed while undo was applying".into())
                }
            };
            match bytes {
                Some(bytes) => {
                    if expected.hash.is_none() {
                        check()?;
                        super::super::patch::create_bytes(
                            &self.root,
                            &path,
                            &bytes,
                            Some(before.mode),
                        )?;
                    } else {
                        let staged = super::super::patch::stage_with_mode(
                            &self.root,
                            &path,
                            &bytes,
                            "undo",
                            plan.operation().as_str(),
                            before.mode,
                        )?;
                        staged.commit(&check)?;
                    }
                }
                None => {
                    check()?;
                    manifest::safe_path(&self.root, &before.path)?;
                    std::fs::remove_file(&path).map_err(|error| error.to_string())?;
                }
            }
            sync_parent(path.parent().ok_or("undo target has no parent")?)?;
            #[cfg(feature = "loopback-provider-testkit")]
            pause_after_first(&self.root, index)?;
            #[cfg(not(feature = "loopback-provider-testkit"))]
            let _ = index;
        }
        Ok(())
    }
}

fn sync_parent(parent: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| error.to_string())
    }
    #[cfg(windows)]
    {
        let _ = parent;
        Ok(()) // File::sync_all is the Windows boundary, as in FileRestoreJournal.
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Err("undo durability is unsupported on this platform".into())
    }
}

impl CheckpointProvider for Provider {
    fn contract(&self) -> Result<ProviderContract, CheckpointError> {
        ProviderContract::new(
            CheckpointProviderId::new("native-patch-artifacts")
                .ok_or(CheckpointError::InvalidProviderContract)?,
            CheckpointProviderVersion::new("1").ok_or(CheckpointError::InvalidProviderContract)?,
            vec![ProviderLane::new(
                lane().map_err(CheckpointError::Provider)?,
                true,
                true,
            )],
            RescueSupport::Supported,
        )
    }

    fn capture(
        &mut self,
        operation: &CheckpointOperationId,
        root: &WorktreeIdentity,
        lanes: &[CheckpointLaneId],
    ) -> ProviderCaptureOutcome {
        let captured = (|| -> Result<WorkspaceCheckpointId, String> {
            if root.root().as_canonical() != self.root.to_str()
                || lanes != [lane().map_err(|error| format!("{error:?}"))?]
            {
                return Err("checkpoint capture has a different root or lane".into());
            }
            if let Some(id) = operation
                .as_str()
                .strip_prefix("capture_")
                .and_then(WorkspaceCheckpointId::new)
            {
                let manifest = self.load(&id).map_err(|error| format!("{error:?}"))?;
                if manifest.expected.is_some()
                    || !self
                        .current_matches(&manifest.files)
                        .map_err(|error| format!("{error:?}"))?
                {
                    return Err("files changed during checkpoint capture".into());
                }
                return Ok(id);
            }
            let (_, target) = self
                .planned
                .as_ref()
                .ok_or("rescue capture has no exact restore target")?;
            let paths = target
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>();
            let rescue = manifest::capture_files(
                &self.artifacts,
                self.root.clone(),
                target.input.clone(),
                &paths,
            )?;
            manifest::save(&self.artifacts, &rescue)
        })();
        match captured {
            Ok(checkpoint) => ProviderCaptureOutcome::Captured {
                checkpoint,
                lineage: CheckpointLineage::Root,
                fidelity: CheckpointFidelity::Exact,
            },
            Err(_) => ProviderCaptureOutcome::Refused {
                reason: CaptureRefusalReason::ProviderUnavailable,
            },
        }
    }

    fn resolve(&mut self, selector: &CheckpointSelector) -> ProviderResolveOutcome {
        match selector {
            CheckpointSelector::Immutable(reference) => match self.load(reference.checkpoint()) {
                Ok(_) => ProviderResolveOutcome::Resolved {
                    checkpoint: reference.checkpoint().clone(),
                },
                Err(reason) => ProviderResolveOutcome::Refused { reason },
            },
            CheckpointSelector::Head { .. } => ProviderResolveOutcome::Refused {
                reason: ProviderRefusal::HeadUnknown,
            },
        }
    }

    fn inspect(
        &mut self,
        checkpoint: &WorkspaceCheckpointRef,
    ) -> Result<ProviderInspection, ProviderRefusal> {
        self.load(checkpoint.checkpoint())?;
        Ok(ProviderInspection::new(
            checkpoint.checkpoint().clone(),
            vec![lane()?],
            CheckpointFidelity::Exact,
            CheckpointLineage::Root,
        ))
    }

    fn fork(
        &mut self,
        _: &CheckpointOperationId,
        _: &WorkspaceCheckpointRef,
        _: &CheckpointHeadId,
    ) -> ProviderForkOutcome {
        ProviderForkOutcome::Refused {
            reason: ProviderRefusal::HeadUnknown,
        }
    }

    fn plan_restore(
        &mut self,
        operation: &CheckpointOperationId,
        checkpoint: &WorkspaceCheckpointRef,
        root: &WorktreeIdentity,
        lanes: &[CheckpointLaneId],
    ) -> Result<ProviderPlan, ProviderRefusal> {
        if root.root().as_canonical() != self.root.to_str() || lanes != [lane()?] {
            return Err(ProviderRefusal::WrongRoot);
        }
        if operation.as_str().strip_prefix("undo_") != Some(checkpoint.checkpoint().as_str()) {
            return Err(ProviderRefusal::Conflict);
        }
        let manifest = self.load(checkpoint.checkpoint())?;
        if !self.current_matches(
            manifest
                .expected
                .as_ref()
                .ok_or(ProviderRefusal::Conflict)?,
        )? {
            return Err(ProviderRefusal::PreconditionsChanged);
        }
        let origin = manifest
            .origin
            .as_ref()
            .ok_or(ProviderRefusal::CheckpointUnknown)?;
        if &origin.worktree != root {
            return Err(ProviderRefusal::WrongRoot);
        }
        let captured_baseline = RestorePrecondition::VcsBaseline {
            baseline: origin.baseline.clone(),
        };
        let consequences = Self::consequences(&manifest)?;
        self.planned = Some((checkpoint.checkpoint().clone(), manifest));
        ProviderPlan::new(consequences, Vec::new(), vec![captured_baseline])
            .map_err(|_| ProviderRefusal::Conflict)
    }

    fn observe_preconditions(
        &mut self,
        plan: &SettledRestorePlan,
    ) -> Result<ProviderPreconditionObservation, ProviderRefusal> {
        let (checkpoint, manifest) = self
            .planned
            .as_ref()
            .filter(|(checkpoint, _)| checkpoint == plan.checkpoint().checkpoint())
            .ok_or(ProviderRefusal::Conflict)?;
        if !self.current_matches(
            manifest
                .expected
                .as_ref()
                .ok_or(ProviderRefusal::Conflict)?,
        )? {
            return Err(ProviderRefusal::PreconditionsChanged);
        }
        let _ = checkpoint;
        // Caller custody prevents every product writer to this workspace until
        // apply returns. The independent observer checks the VCS baseline.
        ProviderPreconditionObservation::new(plan.preconditions().to_vec())
            .map_err(|_| ProviderRefusal::Conflict)
    }

    fn apply_restore(&mut self, admission: RestoreApplicationAdmission) -> ProviderApplyOutcome {
        let plan = admission.plan();
        match self.apply(plan) {
            Ok(()) => ProviderApplyOutcome::Applied(ProviderApplyReceipt::new(
                plan.operation().clone(),
                plan.consequences().clone(),
            )),
            Err(_) => ProviderApplyOutcome::Interrupted,
        }
    }

    fn inspect_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState {
        // The engine's journal owns successful durable receipts. If a crash
        // occurred after a file effect but before its journal terminal, bytes
        // alone cannot attribute the change to this operation. Quarantine it.
        let _ = operation;
        ProviderOperationState::Ambiguous
    }

    fn reconcile_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState {
        self.inspect_operation(operation)
    }
}

// Hold after an actual file effect, before the engine can settle its journal.
#[cfg(feature = "loopback-provider-testkit")]
fn pause_after_first(root: &std::path::Path, index: usize) -> Result<(), String> {
    use std::io::Write;
    if index != 0 || std::env::var_os("GRAPT_PU_UNDO_PAUSE") != Some("1".into()) {
        return Ok(());
    }
    let mut marker = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join(".grapt-pu-undo-entered"))
        .map_err(|error| error.to_string())?;
    marker
        .write_all(b"first-file-restored\n")
        .and_then(|()| marker.sync_all())
        .map_err(|error| error.to_string())?;
    for _ in 0..3_000 {
        if root
            .join(".grapt-pu-undo-release")
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("undo fixture exceeded 3000 polls of 10ms".into())
}
