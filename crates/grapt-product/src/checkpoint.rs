//! Product-owned synchronization boundary for workspace checkpoint operations.
//!
//! The product owns one checkpoint engine. The engine in turn owns the
//! provider, VCS observer, explicit-authority port, and durable lifecycle
//! journal; callers cannot reach the provider's destructive apply method.

use grapt_checkpoint::{
    CaptureReceipt, CheckpointEngine, CheckpointError, CheckpointInspection, CheckpointSelector,
    ForkReceipt, NegotiatedProvider, RestoreApplyOutcome, RestorePlanRequest,
    RestoreRecoveryOutcome,
};
use grapt_types::{
    CheckpointHeadId, CheckpointLaneId, CheckpointOperationId, CheckpointProviderVersion,
    PathIdentity, SettledRestorePlan, WorkspaceCheckpointRef,
};
use std::sync::Mutex;

#[derive(Debug)]
pub struct WorkspaceCheckpointProduct {
    engine: Mutex<CheckpointEngine>,
}

impl WorkspaceCheckpointProduct {
    pub fn new(engine: CheckpointEngine) -> Self {
        Self {
            engine: Mutex::new(engine),
        }
    }

    pub fn negotiate(
        &self,
        version: &CheckpointProviderVersion,
        lanes: Vec<CheckpointLaneId>,
    ) -> Result<NegotiatedProvider, CheckpointProductError> {
        self.with_engine(|engine| engine.negotiate(version, lanes))
    }

    pub fn capture(
        &self,
        operation: CheckpointOperationId,
        root: PathIdentity,
        lanes: Vec<CheckpointLaneId>,
    ) -> Result<CaptureReceipt, CheckpointProductError> {
        self.with_engine(|engine| engine.capture(operation, root, lanes))
    }

    pub fn resolve(
        &self,
        selector: CheckpointSelector,
    ) -> Result<WorkspaceCheckpointRef, CheckpointProductError> {
        self.with_engine(|engine| engine.resolve(selector))
    }

    pub fn inspect(
        &self,
        checkpoint: WorkspaceCheckpointRef,
    ) -> Result<CheckpointInspection, CheckpointProductError> {
        self.with_engine(|engine| engine.inspect(checkpoint))
    }

    pub fn fork(
        &self,
        operation: CheckpointOperationId,
        parent: WorkspaceCheckpointRef,
        head: CheckpointHeadId,
    ) -> Result<ForkReceipt, CheckpointProductError> {
        self.with_engine(|engine| engine.fork(operation, parent, head))
    }

    pub fn plan_restore(
        &self,
        request: RestorePlanRequest,
    ) -> Result<SettledRestorePlan, CheckpointProductError> {
        self.with_engine(|engine| engine.plan_restore(request))
    }

    pub fn apply_restore(
        &self,
        plan: SettledRestorePlan,
    ) -> Result<RestoreApplyOutcome, CheckpointProductError> {
        self.with_engine(|engine| engine.apply_restore(plan))
    }

    /// Keep planning, the caller's exact grant, rescue capture and application
    /// within the existing owner's one engine lock.
    pub fn plan_and_apply_restore(
        &self,
        request: RestorePlanRequest,
        grant: impl FnOnce(&SettledRestorePlan),
    ) -> Result<RestoreApplyOutcome, CheckpointProductError> {
        self.with_engine(|engine| {
            let plan = engine.plan_restore(request)?;
            grant(&plan);
            engine.apply_restore(plan)
        })
    }

    pub fn recover(
        &self,
        operation: &CheckpointOperationId,
    ) -> Result<RestoreRecoveryOutcome, CheckpointProductError> {
        self.with_engine(|engine| engine.recover(operation))
    }

    fn with_engine<T>(
        &self,
        operation: impl FnOnce(&mut CheckpointEngine) -> Result<T, CheckpointError>,
    ) -> Result<T, CheckpointProductError> {
        let mut engine = self
            .engine
            .lock()
            .map_err(|_| CheckpointProductError::OwnerUnavailable)?;
        operation(&mut engine).map_err(CheckpointProductError::Checkpoint)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointProductError {
    #[error("workspace checkpoint owner is unavailable")]
    OwnerUnavailable,
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
}
