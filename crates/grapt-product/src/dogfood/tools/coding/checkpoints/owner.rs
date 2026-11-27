//! Live checkpoint composition uses the existing product owner and journal.
use super::{attribution::UndoSelection, manifest, observer::Observer, provider};
use crate::checkpoint::{CheckpointProductError, WorkspaceCheckpointProduct};
use grapt_artifact::FilesystemArtifactStore;
use grapt_checkpoint::{
    AuthorityEvidence, CheckpointEngine, FileRestoreJournal, RestoreApplyOutcome,
    RestoreAuthorityDecision, RestoreAuthorityPort, RestorePlanRequest, RestoreRecoveryOutcome,
};
use grapt_types::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct Approved {
    operation: CheckpointOperationId,
    capability: CheckpointRestoreCapabilityId,
    permission: PermissionId,
}

#[derive(Debug, Clone)]
struct HumanAuthority(Arc<Mutex<Option<Approved>>>);

impl RestoreAuthorityPort for HumanAuthority {
    fn decide(&mut self, plan: &SettledRestorePlan) -> RestoreAuthorityDecision {
        let Ok(mut held) = self.0.lock() else {
            return RestoreAuthorityDecision::Refused;
        };
        let Some(approved) = held.take() else {
            return RestoreAuthorityDecision::Refused;
        };
        if &approved.operation != plan.operation()
            || &approved.capability != plan.required_capability()
        {
            return RestoreAuthorityDecision::Refused;
        }
        RestoreAuthorityDecision::Granted {
            capability: approved.capability,
            evidence: AuthorityEvidence::new(approved.permission),
        }
    }
}

pub(in crate::dogfood) struct Workspace {
    root: PathBuf,
    artifacts: Arc<FilesystemArtifactStore>,
    product: WorkspaceCheckpointProduct,
    authority: HumanAuthority,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
}

impl core::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Workspace")
            .field("root", &self.root)
            .field("product", &self.product)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::dogfood) enum RestoreFailure {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Unknown(String),
}

fn restore_error_message(error: &CheckpointProductError) -> String {
    match error {
        CheckpointProductError::Checkpoint(grapt_checkpoint::CheckpointError::Provider(
            grapt_checkpoint::ProviderRefusal::PreconditionsChanged,
        )) => "File undo refused: files changed after the recorded tool call. Inspect your edits before retrying.".into(),
        _ => error.to_string(),
    }
}

impl Workspace {
    pub fn open(
        service: &Path,
        root: &Path,
        artifacts: Arc<FilesystemArtifactStore>,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
    ) -> Result<Self, String> {
        let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
        let service = std::fs::canonicalize(service).map_err(|error| error.to_string())?;
        if service.starts_with(&root) {
            return Err("file undo requires its service profile outside the workspace".into());
        }
        let root_text = root.to_str().ok_or("checkpoint root is not UTF-8")?;
        let journal_root = service
            .join("restore-journal")
            .join(manifest::hash(root_text.as_bytes()));
        std::fs::create_dir_all(&journal_root).map_err(|error| error.to_string())?;
        let journal = FileRestoreJournal::open(journal_root).map_err(|error| error.to_string())?;
        let authority = HumanAuthority(Arc::new(Mutex::new(None)));
        let engine = CheckpointEngine::new(
            Box::new(provider::Provider {
                artifacts: Arc::clone(&artifacts),
                root: root.clone(),
                planned: None,
            }),
            Box::new(Observer { root: root.clone() }),
            journal,
            Box::new(authority.clone()),
            Arc::clone(&clock),
            ids,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            root,
            artifacts,
            product: WorkspaceCheckpointProduct::new(engine),
            authority,
            clock,
        })
    }

    pub fn select(
        &self,
        store: &grapt_store::EventStore,
        session: &SessionId,
    ) -> Result<UndoSelection, String> {
        super::receipts::select(store, &self.artifacts, &self.root, session)
    }

    pub fn select_input(
        &self,
        store: &grapt_store::EventStore,
        session: &SessionId,
        input: &MessageId,
    ) -> Result<UndoSelection, String> {
        super::receipts::select_input(store, &self.artifacts, &self.root, session, input)
    }

    pub(super) fn capture(
        &self,
        input: &MessageId,
        paths: &[PathBuf],
    ) -> Result<(WorkspaceCheckpointRef, super::origin::Origin), String> {
        let manifest =
            manifest::capture_files(&self.artifacts, self.root.clone(), input.clone(), paths)?;
        let checkpoint = manifest::save(&self.artifacts, &manifest)?;
        let operation = CheckpointOperationId::new(format!("capture_{}", checkpoint.as_str()))
            .ok_or("checkpoint capture identity is invalid")?;
        let receipt = self
            .product
            .capture(
                operation,
                self.root_identity()?,
                vec![provider::lane().map_err(|error| format!("{error:?}"))?],
            )
            .map_err(|error| error.to_string())?;
        match receipt.outcome() {
            CheckpointCaptureOutcome::Captured { anchor, .. } if anchor.fidelity().is_exact() => Ok((anchor.checkpoint().clone(), super::origin::Origin::anchor(anchor.as_ref()))),
            _ => Err("automatic patch checkpoint could not capture this workspace exactly; file undo is unavailable for this call".into()),
        }
    }

    fn root_identity(&self) -> Result<PathIdentity, String> {
        self.root
            .to_str()
            .map(PathIdentity::canonical)
            .ok_or_else(|| "checkpoint root is not UTF-8".into())
    }

    fn request(&self, selection: &UndoSelection) -> Result<RestorePlanRequest, String> {
        let operation = CheckpointOperationId::new(format!("undo_{}", selection.target.as_str()))
            .ok_or("undo identity is invalid")?;
        let reference = WorkspaceCheckpointRef::new(
            CheckpointProviderId::new("native-patch-artifacts").ok_or("provider identity")?,
            CheckpointProviderVersion::new("1").ok_or("provider version")?,
            selection.target.clone(),
        );
        let expires = self
            .clock
            .now()
            .as_millis()
            .checked_add(60_000)
            .ok_or("undo expiry exceeds clock range")?;
        RestorePlanRequest::new(
            operation,
            reference,
            self.root_identity()?,
            vec![provider::lane().map_err(|error| format!("{error:?}"))?],
            Timestamp::from_millis(expires),
            RealizedEnforcement::HarnessOnly,
        )
        .map_err(|error| error.to_string())
    }

    fn recover(&self, request: &RestorePlanRequest) -> Result<Option<RestoreApplyOutcome>, String> {
        match self.product.recover(request.operation()) {
            Ok(RestoreRecoveryOutcome::Applied(receipt)) => {
                return Ok(Some(RestoreApplyOutcome::Applied(receipt)))
            }
            Ok(RestoreRecoveryOutcome::Quarantined(receipt)) => {
                return Ok(Some(RestoreApplyOutcome::Quarantined(receipt)))
            }
            Ok(RestoreRecoveryOutcome::Refused(receipt)) => {
                let grapt_checkpoint::ReconciledState::Refused(reason) = receipt.state() else {
                    return Err("restore journal refusal has invalid state".into());
                };
                return Ok(Some(RestoreApplyOutcome::Refused(
                    grapt_checkpoint::RestoreApplicationRefusal::Provider(reason.clone()),
                )));
            }
            Err(CheckpointProductError::Checkpoint(
                grapt_checkpoint::CheckpointError::Journal(
                    grapt_checkpoint::JournalError::Missing
                    | grapt_checkpoint::JournalError::Incomplete,
                ),
            )) => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(None)
    }

    pub fn preview(&self, selection: &UndoSelection) -> Result<RestoreConsequences, String> {
        match self.recover(&self.request(selection)?)? {
            Some(RestoreApplyOutcome::Applied(_)) => return Err("This turn’s patch files were already restored. Conversation history is unchanged.".into()),
            Some(RestoreApplyOutcome::Quarantined(_)) => return Err("File undo has uncertain partial effects; inspect the files. The journal prevents this operation from applying again.".into()),
            Some(_) => return Err("This file undo was refused; no new restore will run for the same target.".into()),
            None => {},
        }
        self.product
            .plan_restore(self.request(selection)?)
            .map(|plan| plan.consequences().clone())
            .map_err(|error| restore_error_message(&error))
    }

    pub fn restore(
        &self,
        selection: &UndoSelection,
        idle: &crate::dogfood::execution::WorkspaceIdleReservation,
        approval: &crate::dogfood::undo_route::ApprovedUndo,
    ) -> Result<RestoreApplyOutcome, RestoreFailure> {
        if !idle.matches(&self.root) || !approval.permits(&self.root, &selection.target) {
            return Err(RestoreFailure::Refused(
                "file undo lacks exact native approval or workspace custody".into(),
            ));
        }
        let request = self.request(selection).map_err(RestoreFailure::Refused)?;
        if let Some(outcome) = self.recover(&request).map_err(RestoreFailure::Unknown)? {
            return Ok(outcome);
        }
        let authority = self.authority.clone();
        // Lock the existing engine once, plan, invoke
        // this grant callback, then apply through CheckpointEngine. Rescue
        // scope and exact authority cannot interleave with another operation.
        self.product
            .plan_and_apply_restore(
                self.request(selection).map_err(RestoreFailure::Refused)?,
                |plan| {
                    if let (Ok(mut held), Some(permission)) =
                        (authority.0.lock(), approval.permission(plan))
                    {
                        *held = Some(Approved {
                            operation: plan.operation().clone(),
                            capability: plan.required_capability().clone(),
                            permission,
                        });
                    }
                },
            )
            .map_err(|error| match error {
                CheckpointProductError::Checkpoint(grapt_checkpoint::CheckpointError::Journal(
                    _,
                )) => RestoreFailure::Unknown(error.to_string()),
                _ => RestoreFailure::Refused(restore_error_message(&error)),
            })
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "durability_tests.rs"]
mod durability_tests;
