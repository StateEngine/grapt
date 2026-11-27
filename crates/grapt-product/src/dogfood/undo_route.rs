//! Native file undo requires the exact reviewed immutable target and idle root.
use super::tools::coding::checkpoints;
use super::*;
use grapt_api::u3a::{
    SessionUndoApplicationError as Error, SessionUndoPreviewRequestDto,
    SessionUndoPreviewResponseDto, SessionUndoRequestDto, SessionUndoResponseDto,
};
use grapt_checkpoint::RestoreApplyOutcome;
use grapt_types::WorkspaceCheckpointId;
use std::path::PathBuf;

/// Only this native command boundary can mint a file restore approval.
pub(in crate::dogfood) struct ApprovedUndo {
    root: PathBuf,
    target: WorkspaceCheckpointId,
    _principal: SealedPrincipal,
}
impl ApprovedUndo {
    pub(in crate::dogfood) fn permission(
        &self,
        plan: &grapt_types::SettledRestorePlan,
    ) -> Option<PermissionId> {
        let principal = self._principal.principal();
        // This is the identity of a native command consent, not a model-tool
        // permission request. Bind the journal's evidence to its sealed actor
        // and the exact settled restore capability.
        let digest = grapt_event::canonical_hash_hex(&json!({
            "subject":principal.receipt().subject().as_str(),
            "policy":principal.receipt().policy().as_str(),
            "directoryGeneration":principal.generation().value(),
            "client":principal.client().as_str(),
            "recipient":principal.recipient().as_str(),
            "capability":plan.required_capability().as_str(),
        }));
        format!("per_native_undo_{digest}").parse().ok()
    }

    pub(in crate::dogfood) fn permits(&self, root: &Path, target: &WorkspaceCheckpointId) -> bool {
        self.root == root && &self.target == target
    }
}

impl grapt_api::u3a::SessionEditApplicationPort for DogfoodApplication {
    fn session_rewind_preview(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionRewindPreviewRequestDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionRewindPreviewResponseDto,
        grapt_api::u3a::SessionRewindApplicationError,
    > {
        ready(self.preview_rewind(session_id, request))
    }

    fn session_rewind(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: grapt_api::u3a::SessionRewindRequestDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionRewindResponseDto,
        grapt_api::u3a::SessionRewindApplicationError,
    > {
        ready(self.commit_rewind(session_id, request))
    }

    fn session_undo_preview(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        _request: SessionUndoPreviewRequestDto,
    ) -> ApplicationFuture<SessionUndoPreviewResponseDto, Error> {
        ready(self.preview_undo(session_id.into_inner()))
    }
    fn session_undo(
        &self,
        principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        request: SessionUndoRequestDto,
    ) -> ApplicationFuture<SessionUndoResponseDto, Error> {
        ready(self.commit_undo(principal, session_id.into_inner(), request))
    }
    fn session_reconcile(
        &self,
        _principal: SealedPrincipal,
        session_id: WireId<SessionId>,
        _request: grapt_api::u3a::SessionReconcileRequestDto,
    ) -> ApplicationFuture<
        grapt_api::u3a::SessionReconcileResponseDto,
        grapt_api::u3a::SessionReconcileApplicationError,
    > {
        ready(self.reconcile(session_id.into_inner()))
    }
}

fn lookup(application: &DogfoodApplication, session: &SessionId) -> Result<PathBuf, Error> {
    let record = context::session_record(application, session).map_err(|error| match error {
        SessionLookupApplicationError::Unavailable => Error::InvalidRequest(invalid(
            "History is temporarily unavailable; retry the request",
        )),
        SessionLookupApplicationError::SessionNotFound(value) => Error::SessionNotFound(value),
        SessionLookupApplicationError::InvalidRequest(value) => Error::InvalidRequest(value),
    })?;
    std::fs::canonicalize(record.placement.directory())
        .map_err(|error| Error::Conflict(conflict(error)))
}

impl DogfoodApplication {
    fn undo_workspace(&self, root: &Path) -> Result<checkpoints::Workspace, Error> {
        let provider_catalog = self.catalog_snapshot();
        let service = Path::new(&provider_catalog.directory);
        let artifacts = Arc::new(
            execution::open_checkpoint_artifacts(service)
                .map_err(|error| Error::Unknown(unknown(error)))?,
        );
        checkpoints::Workspace::open(
            service,
            root,
            artifacts,
            Arc::clone(&self.clock) as Arc<dyn grapt_types::ClockAuthority + Send + Sync>,
            Arc::clone(&self.ids) as Arc<dyn grapt_types::IdAuthority + Send + Sync>,
        )
        .map_err(|error| Error::Conflict(conflict(error)))
    }

    fn preview_undo(&self, session: SessionId) -> Result<SessionUndoPreviewResponseDto, Error> {
        let root = lookup(self, &session)?;
        let _idle = self
            .wake_slots
            .reserve_workspace_idle(&root)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let workspace = self.undo_workspace(&root)?;
        let inbox = self.lock_inbox();
        continuation::require_resolved_effects(inbox.store(), &session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let selection = workspace
            .select(inbox.store(), &session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let consequences = workspace
            .preview(&selection)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        decode(json!({"data":{
            "sessionID":session.as_str(),"inputID":selection.input.as_str(),"target":selection.target.as_str(),
            "paths":selection.paths,"incompletePatches":selection.incomplete_patches,
            "writes":consequences.writes(),"deletes":consequences.deletes(),"bytes":consequences.bytes(),
        }})).map_err(|error| Error::Unknown(unknown(error)))
    }

    fn commit_undo(
        &self,
        principal: SealedPrincipal,
        session: SessionId,
        request: SessionUndoRequestDto,
    ) -> Result<SessionUndoResponseDto, Error> {
        let root = lookup(self, &session)?;
        let idle = self
            .wake_slots
            .reserve_workspace_idle(&root)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let _session_idle = self
            .wake_slots
            .reserve_idle(&session)
            .map_err(|error| Error::Conflict(conflict(error)))?;
        let workspace = self.undo_workspace(&root)?;
        let request =
            serde_json::to_value(request).map_err(|error| Error::InvalidRequest(invalid(error)))?;
        let target = request
            .get("target")
            .and_then(Value::as_str)
            .filter(|value| value.len() <= 256)
            .and_then(WorkspaceCheckpointId::new)
            .ok_or_else(|| {
                Error::InvalidRequest(invalid("file undo requires its reviewed target"))
            })?;
        let input: MessageId = request
            .get("inputID")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidRequest(invalid("file undo needs its reviewed input")))?
            .parse()
            .map_err(|_| Error::InvalidRequest(invalid("file undo input identity is invalid")))?;
        let selection;
        {
            let inbox = self.lock_inbox();
            continuation::require_resolved_effects(inbox.store(), &session)
                .map_err(|error| Error::Conflict(conflict(error)))?;
            let pending: bool = inbox
                .store()
                .connection()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_pending WHERE session_id = ?1)",
                    [session.as_str()],
                    |row| row.get(0),
                )
                .map_err(|error| Error::Unknown(unknown(error)))?;
            if pending {
                return Err(Error::Conflict(conflict(
                    "settle pending input before file undo",
                )));
            }
            selection = workspace
                .select_input(inbox.store(), &session, &input)
                .map_err(|error| Error::Conflict(conflict(error)))?;
            if selection.target != target
                || request.get("inputID").and_then(Value::as_str) != Some(selection.input.as_str())
            {
                return Err(Error::Conflict(conflict(
                    "patch files changed since the undo preview; preview again",
                )));
            }
        }
        let approval = ApprovedUndo {
            root: root.clone(),
            target,
            _principal: principal,
        };
        let outcome =
            workspace
                .restore(&selection, &idle, &approval)
                .map_err(|error| match error {
                    checkpoints::RestoreFailure::Refused(reason) => {
                        Error::Conflict(conflict(reason))
                    }
                    checkpoints::RestoreFailure::Unknown(reason) => Error::Unknown(unknown(reason)),
                })?;
        let (status, message) = match outcome {
            RestoreApplyOutcome::Applied(_) => ("applied", "Patch files restored. Conversation history is unchanged."),
            RestoreApplyOutcome::Quarantined(_) => ("quarantined", "File undo has uncertain partial effects. Inspect files before further edits. This operation will not apply again."),
            RestoreApplyOutcome::Ambiguous {..} => return Err(Error::Unknown(unknown("File undo could not confirm its journal. Retry the same undo to reconcile before further edits."))),
            RestoreApplyOutcome::Refused(grapt_checkpoint::RestoreApplicationRefusal::ProviderReceiptMismatch) => ("quarantined", "File undo returned an inconsistent effect receipt. Inspect the files; its journal prevents replay."),
            RestoreApplyOutcome::Refused(_) => ("refused", "File undo refused before applying. Preview again after resolving the file or authority conflict."),
        };
        decode(json!({"data":{"sessionID":session.as_str(),"inputID":selection.input.as_str(),"target":selection.target.as_str(),"status":status,"message":message}}))
            .map_err(|error| Error::Unknown(unknown(error)))
    }
}
