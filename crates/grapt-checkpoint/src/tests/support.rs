use crate::{
    AuthorityEvidence, CheckpointError, CheckpointProvider, CheckpointSelector, FileRestoreJournal,
    ProviderApplyOutcome, ProviderApplyReceipt, ProviderCaptureOutcome, ProviderContract,
    ProviderForkOutcome, ProviderInspection, ProviderLane, ProviderOperationState, ProviderPlan,
    ProviderRefusal, ProviderResolveOutcome, RescueSupport, RestoreAuthorityDecision,
    RestoreAuthorityPort, VcsObserver, WorkspaceObservation,
};
use grapt_types::{
    BranchName, BranchState, CheckpointFidelity, CheckpointHeadId, CheckpointLaneId,
    CheckpointLineage, CheckpointOperationId, CheckpointProviderId, CheckpointProviderVersion,
    CheckpointRestoreCapabilityId, ClockAuthority, GitCommitId, IdAuthority, IdToken, PathIdentity,
    PermissionId, RestoreApplicationAdmission, RestoreConsequences, RestorePrecondition,
    SettledRestorePlan, Timestamp, VcsBaseline, WorkspaceCheckpointId, WorkspaceCheckpointRef,
    WorktreeIdentity,
};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(super) const HEAD_A: &str = "0123456789abcdef0123456789abcdef01234567";
pub(super) const HEAD_B: &str = "fedcba9876543210fedcba9876543210fedcba98";

#[derive(Debug)]
pub(super) struct TestRoot(PathBuf);

impl TestRoot {
    pub(super) fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let target = match std::env::var_os("CARGO_TARGET_DIR") {
            Some(target) => PathBuf::from(target),
            None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        };
        if !target.is_absolute() {
            return Err("checkpoint test target must be absolute".into());
        }
        let path = target.join("checkpoint-tests").join(name);
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }

    pub(super) fn journal(&self) -> Result<FileRestoreJournal, crate::JournalError> {
        let root = self.0.join("journal");
        std::fs::create_dir_all(&root).map_err(|_| crate::JournalError::Unavailable)?;
        FileRestoreJournal::open(root)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone)]
pub(super) struct TestClock(Arc<AtomicI64>);

impl TestClock {
    pub(super) fn new(now: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now)))
    }

    pub(super) fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl ClockAuthority for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

#[derive(Debug, Default)]
pub(super) struct TestIds(AtomicU64);

impl IdAuthority for TestIds {
    fn mint(&self) -> IdToken {
        let ordinal = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        match IdToken::new(format!("wcp-{ordinal}")) {
            Some(token) => token,
            None => panic!("deterministic checkpoint identity construction failed"),
        }
    }
}

#[derive(Debug)]
pub(super) struct ScriptedObserver {
    observations: VecDeque<WorkspaceObservation>,
    last: WorkspaceObservation,
}

impl ScriptedObserver {
    pub(super) fn repeated(observation: WorkspaceObservation) -> Self {
        Self {
            observations: VecDeque::new(),
            last: observation,
        }
    }

    pub(super) fn sequence(
        observations: Vec<WorkspaceObservation>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let observations = VecDeque::from(observations);
        let last = observations
            .back()
            .cloned()
            .ok_or("observer sequence is empty")?;
        Ok(Self { observations, last })
    }
}

impl VcsObserver for ScriptedObserver {
    fn observe(&mut self, _root: &PathIdentity) -> WorkspaceObservation {
        match self.observations.pop_front() {
            Some(observation) => {
                self.last = observation.clone();
                observation
            }
            None => self.last.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureMode {
    Exact,
    Degraded,
    Skip,
    Refuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApplyMode {
    Complete,
    InterruptAfterApply,
    Ambiguous,
    Refuse,
    MismatchedReceipt,
}

#[derive(Debug)]
pub(super) struct ProviderState {
    pub(super) capture_mode: CaptureMode,
    pub(super) apply_mode: ApplyMode,
    pub(super) reconcile_to: ProviderOperationState,
    pub(super) capture_count: u64,
    pub(super) echo_vcs_precondition: bool,
    operations: BTreeMap<String, ProviderOperationState>,
    checkpoints: BTreeMap<String, ProviderInspection>,
    pub(super) secret_payload: String,
}

impl ProviderState {
    pub(super) fn new() -> Self {
        Self {
            capture_mode: CaptureMode::Exact,
            apply_mode: ApplyMode::Complete,
            reconcile_to: ProviderOperationState::Ambiguous,
            capture_count: 0,
            echo_vcs_precondition: true,
            operations: BTreeMap::new(),
            checkpoints: BTreeMap::new(),
            secret_payload: "SECRET-CHECKPOINT-PAYLOAD".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct FakeProvider {
    state: Arc<Mutex<ProviderState>>,
    rescue: RescueSupport,
}

impl FakeProvider {
    pub(super) fn new(state: Arc<Mutex<ProviderState>>, rescue: RescueSupport) -> Self {
        Self { state, rescue }
    }

    fn lanes() -> Result<Vec<ProviderLane>, CheckpointError> {
        Ok(vec![
            ProviderLane::new(lane("files")?, true, true),
            ProviderLane::new(lane("metadata")?, true, false),
        ])
    }
}

impl CheckpointProvider for FakeProvider {
    fn contract(&self) -> Result<ProviderContract, CheckpointError> {
        ProviderContract::new(
            provider_id()?,
            provider_version()?,
            Self::lanes()?,
            self.rescue,
        )
    }

    fn capture(
        &mut self,
        _operation: &CheckpointOperationId,
        _root: &WorktreeIdentity,
        lanes: &[CheckpointLaneId],
    ) -> ProviderCaptureOutcome {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return ProviderCaptureOutcome::Refused {
                    reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
                };
            }
        };
        match state.capture_mode {
            CaptureMode::Skip => ProviderCaptureOutcome::Skipped {
                reason: grapt_types::CaptureSkipReason::PolicyDeclined,
            },
            CaptureMode::Refuse => ProviderCaptureOutcome::Refused {
                reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
            },
            CaptureMode::Exact | CaptureMode::Degraded => {
                state.capture_count += 1;
                let raw = format!("checkpoint-{}", state.capture_count);
                let checkpoint = match WorkspaceCheckpointId::new(&raw) {
                    Some(checkpoint) => checkpoint,
                    None => {
                        return ProviderCaptureOutcome::Refused {
                            reason: grapt_types::CaptureRefusalReason::ProviderUnavailable,
                        };
                    }
                };
                let inspection = ProviderInspection::new(
                    checkpoint.clone(),
                    lanes.to_vec(),
                    CheckpointFidelity::Exact,
                    CheckpointLineage::Root,
                );
                state.checkpoints.insert(raw, inspection);
                let fidelity = if state.capture_mode == CaptureMode::Exact {
                    CheckpointFidelity::Exact
                } else {
                    CheckpointFidelity::Degraded {
                        reason: grapt_types::FidelityLoss::PolicyExcluded,
                    }
                };
                ProviderCaptureOutcome::Captured {
                    checkpoint,
                    lineage: CheckpointLineage::Root,
                    fidelity,
                }
            }
        }
    }

    fn resolve(&mut self, selector: &CheckpointSelector) -> ProviderResolveOutcome {
        match selector {
            CheckpointSelector::Immutable(reference) => ProviderResolveOutcome::Resolved {
                checkpoint: reference.checkpoint().clone(),
            },
            CheckpointSelector::Head { .. } => match self.state.lock() {
                Ok(state) => match state.checkpoints.keys().next_back() {
                    Some(raw) => match WorkspaceCheckpointId::new(raw) {
                        Some(checkpoint) => ProviderResolveOutcome::Resolved { checkpoint },
                        None => ProviderResolveOutcome::Refused {
                            reason: ProviderRefusal::CheckpointUnknown,
                        },
                    },
                    None => ProviderResolveOutcome::Refused {
                        reason: ProviderRefusal::HeadUnknown,
                    },
                },
                Err(_) => ProviderResolveOutcome::Refused {
                    reason: ProviderRefusal::Unavailable,
                },
            },
        }
    }

    fn inspect(
        &mut self,
        checkpoint: &WorkspaceCheckpointRef,
    ) -> Result<ProviderInspection, ProviderRefusal> {
        let state = self
            .state
            .lock()
            .map_err(|_| ProviderRefusal::Unavailable)?;
        state
            .checkpoints
            .get(checkpoint.checkpoint().as_str())
            .cloned()
            .ok_or(ProviderRefusal::CheckpointUnknown)
    }

    fn fork(
        &mut self,
        _operation: &CheckpointOperationId,
        parent: &WorkspaceCheckpointRef,
        head: &CheckpointHeadId,
    ) -> ProviderForkOutcome {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return ProviderForkOutcome::Refused {
                    reason: ProviderRefusal::Unavailable,
                };
            }
        };
        let child =
            match WorkspaceCheckpointId::new(format!("{}-fork", parent.checkpoint().as_str())) {
                Some(child) => child,
                None => {
                    return ProviderForkOutcome::Refused {
                        reason: ProviderRefusal::CheckpointUnknown,
                    };
                }
            };
        state.checkpoints.insert(
            child.as_str().to_owned(),
            ProviderInspection::new(
                child.clone(),
                vec![match lane("files") {
                    Ok(lane) => lane,
                    Err(_) => {
                        return ProviderForkOutcome::Refused {
                            reason: ProviderRefusal::LaneUnknown,
                        };
                    }
                }],
                CheckpointFidelity::Exact,
                CheckpointLineage::ForkedFrom {
                    checkpoint: parent.checkpoint().clone(),
                    head: head.clone(),
                },
            ),
        );
        ProviderForkOutcome::Forked {
            checkpoint: child,
            head: head.clone(),
        }
    }

    fn plan_restore(
        &mut self,
        _operation: &CheckpointOperationId,
        checkpoint: &WorkspaceCheckpointRef,
        _root: &WorktreeIdentity,
        _lanes: &[CheckpointLaneId],
    ) -> Result<ProviderPlan, ProviderRefusal> {
        let state = self
            .state
            .lock()
            .map_err(|_| ProviderRefusal::Unavailable)?;
        if !state
            .checkpoints
            .contains_key(checkpoint.checkpoint().as_str())
        {
            return Err(ProviderRefusal::CheckpointUnknown);
        }
        ProviderPlan::new(RestoreConsequences::new(2, 1, 64), Vec::new(), Vec::new())
            .map_err(|_| ProviderRefusal::Conflict)
    }

    fn observe_preconditions(
        &mut self,
        plan: &SettledRestorePlan,
    ) -> Result<crate::ProviderPreconditionObservation, ProviderRefusal> {
        let satisfied = match self.state.lock() {
            Ok(state) if !state.echo_vcs_precondition => plan
                .preconditions()
                .iter()
                .filter(|value| !matches!(value, RestorePrecondition::VcsBaseline { .. }))
                .cloned()
                .collect(),
            Ok(_) | Err(_) => plan.preconditions().to_vec(),
        };
        crate::ProviderPreconditionObservation::new(satisfied)
            .map_err(|_| ProviderRefusal::Conflict)
    }

    fn apply_restore(&mut self, admission: RestoreApplicationAdmission) -> ProviderApplyOutcome {
        let operation = admission.plan().operation().clone();
        let receipt =
            ProviderApplyReceipt::new(operation.clone(), admission.plan().consequences().clone());
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return ProviderApplyOutcome::Refused(ProviderRefusal::Unavailable),
        };
        match state.apply_mode {
            ApplyMode::Complete => {
                state.operations.insert(
                    operation.as_str().to_owned(),
                    ProviderOperationState::Applied(receipt.clone()),
                );
                ProviderApplyOutcome::Applied(receipt)
            }
            ApplyMode::InterruptAfterApply => {
                state.operations.insert(
                    operation.as_str().to_owned(),
                    ProviderOperationState::Applied(receipt),
                );
                ProviderApplyOutcome::Interrupted
            }
            ApplyMode::Ambiguous => {
                state.operations.insert(
                    operation.as_str().to_owned(),
                    ProviderOperationState::Ambiguous,
                );
                ProviderApplyOutcome::Interrupted
            }
            ApplyMode::Refuse => {
                let reason = ProviderRefusal::PreconditionsChanged;
                state.operations.insert(
                    operation.as_str().to_owned(),
                    ProviderOperationState::Refused(reason.clone()),
                );
                ProviderApplyOutcome::Refused(reason)
            }
            ApplyMode::MismatchedReceipt => {
                let mismatched = ProviderApplyReceipt::new(
                    operation.clone(),
                    RestoreConsequences::new(
                        receipt.consequences().writes(),
                        receipt.consequences().deletes(),
                        receipt.consequences().bytes() + 1,
                    ),
                );
                state.operations.insert(
                    operation.as_str().to_owned(),
                    ProviderOperationState::Applied(mismatched.clone()),
                );
                ProviderApplyOutcome::Applied(mismatched)
            }
        }
    }

    fn inspect_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState {
        match self.state.lock() {
            Ok(state) => state
                .operations
                .get(operation.as_str())
                .cloned()
                .unwrap_or(ProviderOperationState::Missing),
            Err(_) => ProviderOperationState::Ambiguous,
        }
    }

    fn reconcile_operation(&mut self, operation: &CheckpointOperationId) -> ProviderOperationState {
        match self.state.lock() {
            Ok(mut state) => {
                let reconciled = state.reconcile_to.clone();
                state
                    .operations
                    .insert(operation.as_str().to_owned(), reconciled.clone());
                reconciled
            }
            Err(_) => ProviderOperationState::Ambiguous,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum AuthorityMode {
    Current,
    Refuse,
    Fixed(CheckpointRestoreCapabilityId),
}

#[derive(Debug, Clone)]
pub(super) struct FakeAuthority(pub(super) Arc<Mutex<AuthorityMode>>);

impl RestoreAuthorityPort for FakeAuthority {
    fn decide(&mut self, plan: &SettledRestorePlan) -> RestoreAuthorityDecision {
        let mode = match self.0.lock() {
            Ok(mode) => mode.clone(),
            Err(_) => return RestoreAuthorityDecision::Refused,
        };
        let capability = match mode {
            AuthorityMode::Current => plan.required_capability().clone(),
            AuthorityMode::Fixed(capability) => capability,
            AuthorityMode::Refuse => return RestoreAuthorityDecision::Refused,
        };
        let permission = match PermissionId::from_str("per_checkpoint") {
            Ok(permission) => permission,
            Err(_) => return RestoreAuthorityDecision::Refused,
        };
        RestoreAuthorityDecision::Granted {
            capability,
            evidence: AuthorityEvidence::new(permission),
        }
    }
}

pub(super) fn provider_id() -> Result<CheckpointProviderId, CheckpointError> {
    CheckpointProviderId::new("fixture-provider").ok_or(CheckpointError::InvalidProviderContract)
}

pub(super) fn provider_version() -> Result<CheckpointProviderVersion, CheckpointError> {
    CheckpointProviderVersion::new("1").ok_or(CheckpointError::InvalidProviderContract)
}

pub(super) fn lane(raw: &str) -> Result<CheckpointLaneId, CheckpointError> {
    CheckpointLaneId::new(raw).ok_or(CheckpointError::IdentityOverBound)
}

pub(super) fn operation(raw: &str) -> Result<CheckpointOperationId, CheckpointError> {
    CheckpointOperationId::new(raw).ok_or(CheckpointError::IdentityOverBound)
}

pub(super) fn symbolic_observation(
    root: &str,
    head: &str,
) -> Result<WorkspaceObservation, CheckpointError> {
    let root_id = PathIdentity::canonical(root);
    let commit = GitCommitId::new(head).ok_or(CheckpointError::InvalidRoot)?;
    let branch = BranchName::new("main").ok_or(CheckpointError::InvalidRoot)?;
    Ok(WorkspaceObservation::new(
        WorktreeIdentity::Git {
            root: root_id,
            common_dir: PathIdentity::canonical(format!("{root}/.git-common")),
            git_dir: PathIdentity::canonical(format!("{root}/.git-worktree")),
        },
        VcsBaseline::Git {
            head: commit,
            branch: BranchState::Symbolic(branch),
        },
    ))
}

pub(super) fn detached_observation(root: &str) -> Result<WorkspaceObservation, CheckpointError> {
    let commit = GitCommitId::new(HEAD_A).ok_or(CheckpointError::InvalidRoot)?;
    Ok(WorkspaceObservation::new(
        WorktreeIdentity::Git {
            root: PathIdentity::canonical(root),
            common_dir: PathIdentity::canonical(format!("{root}/.git-common")),
            git_dir: PathIdentity::canonical(format!("{root}/.git-worktree")),
        },
        VcsBaseline::Git {
            head: commit,
            branch: BranchState::Detached,
        },
    ))
}

pub(super) fn plain_observation(root: &str) -> WorkspaceObservation {
    WorkspaceObservation::new(
        WorktreeIdentity::Plain {
            root: PathIdentity::canonical(root),
        },
        VcsBaseline::NotVersioned,
    )
}

pub(super) fn engine(
    state: Arc<Mutex<ProviderState>>,
    rescue: RescueSupport,
    observer: ScriptedObserver,
    journal: FileRestoreJournal,
    authority: FakeAuthority,
    clock: TestClock,
    ids: Arc<TestIds>,
) -> Result<crate::CheckpointEngine, CheckpointError> {
    crate::CheckpointEngine::new(
        Box::new(FakeProvider::new(state, rescue)),
        Box::new(observer),
        journal,
        Box::new(authority),
        Arc::new(clock),
        ids,
    )
}
