use crate::journal::{
    ApplyingRestore, BeginDisposition, FileRestoreJournal, RestoreJournal, RootKind, StoredRestore,
    TerminalRestore,
};
use crate::model::{
    anchor_from_capture, canonical_lanes, capture_refusal_for_baseline, checkpoint_reference,
    root_matches, validate_checkpoint, validate_identity, validate_inspection, validate_lanes,
    validate_lineage, validate_path, AuthorityEvidence, CaptureReceipt, CheckpointError,
    CheckpointInspection, CheckpointSelector, ForkReceipt, NegotiatedProvider,
    ProviderApplyOutcome, ProviderCaptureOutcome, ProviderContract, ProviderForkOutcome,
    ProviderOperationState, ProviderRefusal, ProviderResolveOutcome, ReconcileReceipt,
    ReconciledState, RescueSupport, RestoreApplicationRefusal, RestoreApplyOutcome,
    RestoreAuthorityDecision, RestorePlanRequest, RestoreRecoveryOutcome,
};
use crate::ports::{CheckpointProvider, RestoreAuthorityPort, VcsObserver};
use grapt_types::{
    CaptureRefusalReason, CheckpointCaptureOutcome, CheckpointHeadId, CheckpointLaneId,
    CheckpointOperationId, CheckpointRestoreAuthorityLease, CheckpointRestorePlanId,
    ClockAuthority, DeterministicAuthorities, IdAuthority, PathIdentity, RecoveryAnchorId,
    RestorePrecondition, RestoreRescue, SettledRestorePlan, SettledRestorePlanParts,
    WorkspaceCheckpointRef, WorktreeIdentity,
};
use std::sync::Arc;

pub struct CheckpointEngine {
    provider: Box<dyn CheckpointProvider>,
    observer: Box<dyn VcsObserver>,
    journal: FileRestoreJournal,
    authority: Box<dyn RestoreAuthorityPort>,
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    ids: Arc<dyn IdAuthority + Send + Sync>,
}

impl core::fmt::Debug for CheckpointEngine {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("CheckpointEngine")
            .field("provider", &"<external checkpoint provider>")
            .field("observer", &"<independent VCS observer>")
            .field("journal", &"<durable restore journal>")
            .field("authority", &"<explicit restore authority>")
            .field("clock", &"<injected clock>")
            .field("ids", &"<injected identities>")
            .finish()
    }
}

impl CheckpointEngine {
    pub fn new(
        provider: Box<dyn CheckpointProvider>,
        observer: Box<dyn VcsObserver>,
        journal: FileRestoreJournal,
        authority: Box<dyn RestoreAuthorityPort>,
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
    ) -> Result<Self, CheckpointError> {
        let engine = Self {
            provider,
            observer,
            journal,
            authority,
            clock,
            ids,
        };
        engine.current_contract()?;
        Ok(engine)
    }

    pub fn negotiate(
        &self,
        version: &grapt_types::CheckpointProviderVersion,
        lanes: Vec<CheckpointLaneId>,
    ) -> Result<NegotiatedProvider, CheckpointError> {
        validate_lanes(&lanes)?;
        let contract = self.current_contract()?;
        if contract.version() != version {
            return Err(CheckpointError::ProviderContractMismatch);
        }
        let lanes = canonical_lanes(lanes);
        for lane in &lanes {
            if contract.lane(lane).is_none() {
                return Err(CheckpointError::LaneUnavailable);
            }
        }
        Ok(NegotiatedProvider::new(contract, lanes))
    }

    pub fn capture(
        &mut self,
        operation: CheckpointOperationId,
        root: PathIdentity,
        lanes: Vec<CheckpointLaneId>,
    ) -> Result<CaptureReceipt, CheckpointError> {
        validate_identity(operation.as_str())?;
        validate_path(&root)?;
        validate_lanes(&lanes)?;
        let lanes = canonical_lanes(lanes);
        let contract = self.current_contract()?;
        for lane in &lanes {
            if contract
                .lane(lane)
                .is_none_or(|capability| !capability.supports_capture())
            {
                return Ok(self.capture_input_refusal(
                    operation,
                    root,
                    CaptureRefusalReason::UnknownLane { lane: lane.clone() },
                ));
            }
        }

        let before = self.observer.observe(&root);
        if !root_matches(&before, &root) {
            return Ok(CaptureReceipt::new(
                operation,
                before.clone(),
                before,
                CheckpointCaptureOutcome::Refused {
                    reason: CaptureRefusalReason::WrongRoot,
                },
            ));
        }
        if let Some(reason) = capture_refusal_for_baseline(before.baseline()) {
            return Ok(CaptureReceipt::new(
                operation,
                before.clone(),
                before,
                CheckpointCaptureOutcome::Refused { reason },
            ));
        }

        let provider_outcome = self.provider.capture(&operation, before.worktree(), &lanes);
        let after = self.observer.observe(&root);
        if before.worktree() != after.worktree() {
            return Ok(CaptureReceipt::new(
                operation,
                before,
                after,
                CheckpointCaptureOutcome::Refused {
                    reason: CaptureRefusalReason::WorktreeChangedDuringCapture,
                },
            ));
        }
        if before.baseline() != after.baseline() {
            return Ok(CaptureReceipt::new(
                operation,
                before,
                after,
                CheckpointCaptureOutcome::Refused {
                    reason: CaptureRefusalReason::HeadChangedDuringCapture,
                },
            ));
        }

        let outcome = match provider_outcome {
            ProviderCaptureOutcome::Captured {
                checkpoint,
                lineage,
                fidelity,
            } => {
                validate_identity(checkpoint.as_str())?;
                validate_lineage(&lineage)?;
                let reference = checkpoint_reference(&contract, checkpoint);
                let anchor_id = RecoveryAnchorId::new(self.authorities().mint().as_str())
                    .ok_or(CheckpointError::ProviderResultOverBound)?;
                match anchor_from_capture(anchor_id, reference, &before, &after, lanes, fidelity) {
                    Some(anchor) => CheckpointCaptureOutcome::Captured {
                        anchor: Box::new(anchor),
                        lineage,
                    },
                    None => CheckpointCaptureOutcome::Refused {
                        reason: CaptureRefusalReason::HeadChangedDuringCapture,
                    },
                }
            }
            ProviderCaptureOutcome::Skipped { reason } => {
                CheckpointCaptureOutcome::Skipped { reason }
            }
            ProviderCaptureOutcome::Refused { reason } => {
                CheckpointCaptureOutcome::Refused { reason }
            }
        };
        Ok(CaptureReceipt::new(operation, before, after, outcome))
    }

    pub fn resolve(
        &mut self,
        selector: CheckpointSelector,
    ) -> Result<WorkspaceCheckpointRef, CheckpointError> {
        let contract = self.current_contract()?;
        match &selector {
            CheckpointSelector::Immutable(reference) => {
                self.require_active_contract(reference, &contract)?;
            }
            CheckpointSelector::Head {
                provider,
                version,
                head,
            } => {
                validate_identity(head.as_str())?;
                if provider != contract.provider() || version != contract.version() {
                    return Err(CheckpointError::ProviderContractMismatch);
                }
            }
        }
        match self.provider.resolve(&selector) {
            ProviderResolveOutcome::Resolved { checkpoint } => {
                validate_identity(checkpoint.as_str())?;
                Ok(checkpoint_reference(&contract, checkpoint))
            }
            ProviderResolveOutcome::Refused { reason } => Err(CheckpointError::Provider(reason)),
        }
    }

    pub fn inspect(
        &mut self,
        reference: WorkspaceCheckpointRef,
    ) -> Result<CheckpointInspection, CheckpointError> {
        let contract = self.current_contract()?;
        self.require_active_contract(&reference, &contract)?;
        let inspection = self
            .provider
            .inspect(&reference)
            .map_err(CheckpointError::Provider)?;
        validate_inspection(&inspection, &reference)?;
        for lane in inspection.lanes() {
            if contract.lane(lane).is_none() {
                return Err(CheckpointError::ProviderResultOverBound);
            }
        }
        Ok(CheckpointInspection::new(reference, inspection))
    }

    pub fn fork(
        &mut self,
        operation: CheckpointOperationId,
        parent: WorkspaceCheckpointRef,
        head: CheckpointHeadId,
    ) -> Result<ForkReceipt, CheckpointError> {
        validate_identity(operation.as_str())?;
        validate_identity(head.as_str())?;
        let contract = self.current_contract()?;
        self.require_active_contract(&parent, &contract)?;
        match self.provider.fork(&operation, &parent, &head) {
            ProviderForkOutcome::Forked {
                checkpoint,
                head: returned_head,
            } => {
                validate_identity(checkpoint.as_str())?;
                if returned_head != head {
                    return Err(CheckpointError::ProviderResultOverBound);
                }
                let child = checkpoint_reference(&contract, checkpoint);
                Ok(ForkReceipt::new(operation, parent, child, returned_head))
            }
            ProviderForkOutcome::Refused { reason } => Err(CheckpointError::Provider(reason)),
        }
    }

    pub fn plan_restore(
        &mut self,
        request: RestorePlanRequest,
    ) -> Result<SettledRestorePlan, CheckpointError> {
        validate_checkpoint(request.checkpoint())?;
        let contract = self.current_contract()?;
        self.require_active_contract(request.checkpoint(), &contract)?;
        for lane in request.lanes() {
            if contract
                .lane(lane)
                .is_none_or(|capability| !capability.supports_restore())
            {
                return Err(CheckpointError::LaneUnavailable);
            }
        }
        if contract.rescue_support() != RescueSupport::Supported {
            return Err(CheckpointError::RescueUnsupported);
        }
        let observation = self.observer.observe(request.root());
        if !root_matches(&observation, request.root()) {
            return Err(CheckpointError::Provider(ProviderRefusal::WrongRoot));
        }
        let provider_plan = self
            .provider
            .plan_restore(
                request.operation(),
                request.checkpoint(),
                observation.worktree(),
                request.lanes(),
            )
            .map_err(CheckpointError::Provider)?;

        let unusual = provider_plan.unusual().to_vec();
        let mut preconditions = provider_plan.preconditions().to_vec();
        push_unique(
            &mut preconditions,
            RestorePrecondition::ProviderVersion {
                version: contract.version().clone(),
            },
        );
        for lane in request.lanes() {
            push_unique(
                &mut preconditions,
                RestorePrecondition::LaneNegotiated { lane: lane.clone() },
            );
        }
        push_unique(&mut preconditions, RestorePrecondition::WorktreeQuiescent);
        push_unique(
            &mut preconditions,
            RestorePrecondition::VcsBaseline {
                baseline: observation.baseline().clone(),
            },
        );

        let plan_id = CheckpointRestorePlanId::new(self.authorities().mint().as_str())
            .ok_or(CheckpointError::InvalidRestorePlan)?;
        let parts = SettledRestorePlanParts {
            plan: plan_id,
            operation: request.operation().clone(),
            checkpoint: request.checkpoint().clone(),
            root: observation.worktree().clone(),
            lanes: request.lanes().to_vec(),
            consequences: provider_plan.consequences().clone(),
            unusual,
            preconditions,
            enforcement: request.enforcement().clone(),
            expires_at: request.expires_at(),
        };
        SettledRestorePlan::settle_at_provider_boundary(parts, &self.authorities())
            .ok_or(CheckpointError::InvalidRestorePlan)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn apply_restore(
        &mut self,
        plan: SettledRestorePlan,
    ) -> Result<RestoreApplyOutcome, CheckpointError> {
        let authority = self.authority.decide(&plan);
        let (capability, evidence) = match authority {
            RestoreAuthorityDecision::Refused => {
                return Ok(RestoreApplyOutcome::Refused(
                    RestoreApplicationRefusal::AuthorityRefused,
                ));
            }
            RestoreAuthorityDecision::Granted {
                capability,
                evidence,
            } => (capability, evidence),
        };
        if &capability != plan.required_capability() {
            return Ok(RestoreApplyOutcome::Refused(
                RestoreApplicationRefusal::AuthorityMismatch,
            ));
        }

        let contract = self.current_contract()?;
        self.require_active_contract(plan.checkpoint(), &contract)?;
        let rescue = match contract.rescue_support() {
            RescueSupport::Supported => {
                let operation = CheckpointOperationId::new(self.authorities().mint().as_str())
                    .ok_or(CheckpointError::ProviderResultOverBound)?;
                let receipt =
                    self.capture(operation, plan.root().root().clone(), plan.lanes().to_vec())?;
                let rescue = match receipt.outcome() {
                    CheckpointCaptureOutcome::Captured { anchor, .. }
                        if anchor.fidelity().is_exact() =>
                    {
                        RestoreRescue::captured_at_provider_boundary(&plan, anchor)
                    }
                    _ => None,
                };
                match rescue {
                    Some(rescue) => rescue,
                    None => {
                        return Ok(RestoreApplyOutcome::Refused(
                            RestoreApplicationRefusal::RescueUnavailable(Box::new(receipt)),
                        ));
                    }
                }
            }
            RescueSupport::Unsupported => return Err(CheckpointError::RescueUnsupported),
        };

        let mut satisfied = self
            .provider
            .observe_preconditions(&plan)
            .map_err(CheckpointError::Provider)?
            .into_satisfied();
        satisfied.retain(|precondition| {
            !matches!(precondition, RestorePrecondition::VcsBaseline { .. })
        });
        let current = self.observer.observe(plan.root().root());
        if current.worktree() == plan.root() {
            for required in plan.preconditions() {
                if let RestorePrecondition::VcsBaseline { baseline } = required {
                    if current.baseline() == baseline {
                        push_unique(&mut satisfied, required.clone());
                    }
                }
            }
        }
        let observation = grapt_types::PlanPreconditionObservation::observe_at_provider_boundary(
            &plan,
            satisfied,
            &self.authorities(),
        );
        let lease = CheckpointRestoreAuthorityLease::issue_at_boundary(capability);
        let admission = match plan
            .apply(rescue, observation, lease, &self.authorities())
            .into_result()
        {
            Ok(admission) => admission,
            Err(refusal) => {
                return Ok(RestoreApplyOutcome::Refused(
                    RestoreApplicationRefusal::Admission(refusal),
                ));
            }
        };

        let record = applying_record(&admission, &evidence)?;
        if self.journal.begin(&record)? == BeginDisposition::AlreadyExists {
            return Ok(RestoreApplyOutcome::Refused(
                RestoreApplicationRefusal::DuplicateApply,
            ));
        }
        let expected_operation = admission.plan().operation().clone();
        let expected_consequences = admission.plan().consequences().clone();
        let outcome = self.provider.apply_restore(admission);
        match outcome {
            ProviderApplyOutcome::Applied(receipt)
                if receipt.operation() == &expected_operation
                    && receipt.consequences() == &expected_consequences =>
            {
                let terminal = TerminalRestore {
                    state: ReconciledState::Applied,
                };
                self.journal.settle(record.operation.as_str(), &terminal)?;
                Ok(RestoreApplyOutcome::Applied(receipt_from_record(
                    &record,
                    terminal.state,
                )?))
            }
            ProviderApplyOutcome::Applied(_) => {
                let terminal = TerminalRestore {
                    state: ReconciledState::Quarantined,
                };
                self.journal.settle(record.operation.as_str(), &terminal)?;
                Ok(RestoreApplyOutcome::Refused(
                    RestoreApplicationRefusal::ProviderReceiptMismatch,
                ))
            }
            ProviderApplyOutcome::Refused(reason) => {
                let terminal = TerminalRestore {
                    state: ReconciledState::Refused(reason.clone()),
                };
                self.journal.settle(record.operation.as_str(), &terminal)?;
                Ok(RestoreApplyOutcome::Refused(
                    RestoreApplicationRefusal::Provider(reason),
                ))
            }
            ProviderApplyOutcome::Interrupted => Ok(RestoreApplyOutcome::Ambiguous {
                operation: expected_operation,
            }),
        }
    }

    pub fn recover(
        &mut self,
        operation: &CheckpointOperationId,
    ) -> Result<RestoreRecoveryOutcome, CheckpointError> {
        validate_identity(operation.as_str())?;
        let stored = self
            .journal
            .read(operation.as_str())?
            .ok_or(crate::JournalError::Missing)?;
        match stored {
            StoredRestore::Terminal { applying, terminal } => {
                recovery_outcome(receipt_from_record(&applying, terminal.state.clone())?)
            }
            StoredRestore::Applying(applying) => {
                let contract = self.current_contract()?;
                let state = if applying.provider != contract.provider().as_str()
                    || applying.version != contract.version().as_str()
                {
                    ReconciledState::Quarantined
                } else {
                    let inspected = self.provider.inspect_operation(operation);
                    let settled = match inspected {
                        ProviderOperationState::Ambiguous | ProviderOperationState::Missing => {
                            self.provider.reconcile_operation(operation)
                        }
                        terminal => terminal,
                    };
                    reconciled_state(&applying, settled)
                };
                let terminal = TerminalRestore {
                    state: state.clone(),
                };
                self.journal.settle(operation.as_str(), &terminal)?;
                recovery_outcome(receipt_from_record(&applying, state)?)
            }
        }
    }

    fn current_contract(&self) -> Result<ProviderContract, CheckpointError> {
        self.provider.contract()
    }

    fn authorities(&self) -> DeterministicAuthorities<'_> {
        DeterministicAuthorities::new(self.clock.as_ref(), self.ids.as_ref())
    }

    fn require_active_contract(
        &self,
        reference: &WorkspaceCheckpointRef,
        contract: &ProviderContract,
    ) -> Result<(), CheckpointError> {
        validate_checkpoint(reference)?;
        if reference.provider() != contract.provider() || reference.version() != contract.version()
        {
            return Err(CheckpointError::ProviderContractMismatch);
        }
        Ok(())
    }

    fn capture_input_refusal(
        &mut self,
        operation: CheckpointOperationId,
        root: PathIdentity,
        reason: grapt_types::CaptureRefusalReason,
    ) -> CaptureReceipt {
        let observation = self.observer.observe(&root);
        CaptureReceipt::new(
            operation,
            observation.clone(),
            observation,
            CheckpointCaptureOutcome::Refused { reason },
        )
    }
}

fn push_unique(values: &mut Vec<RestorePrecondition>, value: RestorePrecondition) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn applying_record(
    admission: &grapt_types::RestoreApplicationAdmission,
    authority: &AuthorityEvidence,
) -> Result<ApplyingRestore, CheckpointError> {
    let plan = admission.plan();
    let (root_kind, root, common_dir, git_dir) = root_parts(plan.root())?;
    Ok(ApplyingRestore {
        operation: plan.operation().as_str().to_owned(),
        plan: plan.plan().as_str().to_owned(),
        provider: plan.checkpoint().provider().as_str().to_owned(),
        version: plan.checkpoint().version().as_str().to_owned(),
        checkpoint: plan.checkpoint().checkpoint().as_str().to_owned(),
        root_kind,
        root,
        common_dir,
        git_dir,
        lanes: plan
            .lanes()
            .iter()
            .map(|lane| lane.as_str().to_owned())
            .collect(),
        permission: authority.permission().as_str().to_owned(),
        admitted_at_ms: admission.admitted_at().as_millis(),
        writes: plan.consequences().writes(),
        deletes: plan.consequences().deletes(),
        bytes: plan.consequences().bytes(),
    })
}

type RootParts = (RootKind, String, Option<String>, Option<String>);

fn root_parts(root: &WorktreeIdentity) -> Result<RootParts, CheckpointError> {
    match root {
        WorktreeIdentity::Plain { root } => Ok((
            RootKind::Plain,
            canonical_path(root)?.to_owned(),
            None,
            None,
        )),
        WorktreeIdentity::Git {
            root,
            common_dir,
            git_dir,
        } => Ok((
            RootKind::Git,
            canonical_path(root)?.to_owned(),
            Some(canonical_path(common_dir)?.to_owned()),
            Some(canonical_path(git_dir)?.to_owned()),
        )),
    }
}

fn canonical_path(path: &PathIdentity) -> Result<&str, CheckpointError> {
    validate_path(path)?;
    path.as_canonical().ok_or(CheckpointError::InvalidRoot)
}

fn receipt_from_record(
    record: &ApplyingRestore,
    state: ReconciledState,
) -> Result<ReconcileReceipt, CheckpointError> {
    let operation = CheckpointOperationId::new(&record.operation)
        .ok_or(CheckpointError::ProviderResultOverBound)?;
    let plan = CheckpointRestorePlanId::new(&record.plan)
        .ok_or(CheckpointError::ProviderResultOverBound)?;
    let provider = grapt_types::CheckpointProviderId::new(&record.provider)
        .ok_or(CheckpointError::ProviderResultOverBound)?;
    let version = grapt_types::CheckpointProviderVersion::new(&record.version)
        .ok_or(CheckpointError::ProviderResultOverBound)?;
    let checkpoint = grapt_types::WorkspaceCheckpointId::new(&record.checkpoint)
        .ok_or(CheckpointError::ProviderResultOverBound)?;
    Ok(ReconcileReceipt::new(
        operation,
        plan,
        WorkspaceCheckpointRef::new(provider, version, checkpoint),
        state,
    ))
}

fn reconciled_state(record: &ApplyingRestore, state: ProviderOperationState) -> ReconciledState {
    match state {
        ProviderOperationState::Applied(receipt)
            if receipt.operation().as_str() == record.operation
                && receipt.consequences().writes() == record.writes
                && receipt.consequences().deletes() == record.deletes
                && receipt.consequences().bytes() == record.bytes =>
        {
            ReconciledState::Applied
        }
        ProviderOperationState::Refused(reason) => ReconciledState::Refused(reason),
        ProviderOperationState::Applied(_)
        | ProviderOperationState::Ambiguous
        | ProviderOperationState::Missing => ReconciledState::Quarantined,
    }
}

fn recovery_outcome(receipt: ReconcileReceipt) -> Result<RestoreRecoveryOutcome, CheckpointError> {
    match receipt.state() {
        ReconciledState::Applied => Ok(RestoreRecoveryOutcome::Applied(receipt)),
        ReconciledState::Refused(_) => Ok(RestoreRecoveryOutcome::Refused(receipt)),
        ReconciledState::Quarantined => Ok(RestoreRecoveryOutcome::Quarantined(receipt)),
    }
}
