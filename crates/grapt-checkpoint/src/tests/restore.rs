use super::support::*;
use crate::{
    ReconciledState, RescueSupport, RestoreApplicationRefusal, RestoreApplyOutcome,
    RestorePlanRequest, RestoreRecoveryOutcome,
};
use grapt_types::{
    CheckpointCaptureOutcome, PathIdentity, RealizedEnforcement, Timestamp, WorkspaceCheckpointRef,
};
use std::sync::{Arc, Mutex};

#[test]
fn provider_precondition_observation_is_bounded_before_admission(
) -> Result<(), Box<dyn std::error::Error>> {
    let precondition = grapt_types::RestorePrecondition::WorktreeQuiescent;
    let over_bound = vec![precondition; crate::MAX_RESTORE_PRECONDITIONS + 1];
    assert!(matches!(
        crate::ProviderPreconditionObservation::new(over_bound),
        Err(crate::CheckpointError::ProviderResultOverBound)
    ));
    Ok(())
}

fn capture_source(
    engine: &mut crate::CheckpointEngine,
    workspace: &str,
) -> Result<WorkspaceCheckpointRef, Box<dyn std::error::Error>> {
    let receipt = engine.capture(
        operation("capture-source")?,
        PathIdentity::canonical(workspace),
        vec![lane("files")?],
    )?;
    match receipt.outcome() {
        CheckpointCaptureOutcome::Captured { anchor, .. } => Ok(anchor.checkpoint().clone()),
        other => Err(format!("source capture failed: {other:?}").into()),
    }
}

fn plan(
    engine: &mut crate::CheckpointEngine,
    workspace: &str,
    checkpoint: WorkspaceCheckpointRef,
    operation_id: &str,
    expiry: i64,
) -> Result<grapt_types::SettledRestorePlan, Box<dyn std::error::Error>> {
    Ok(engine.plan_restore(RestorePlanRequest::new(
        operation(operation_id)?,
        checkpoint,
        PathIdentity::canonical(workspace),
        vec![lane("files")?],
        Timestamp::from_millis(expiry),
        RealizedEnforcement::HarnessOnly,
    )?)?)
}

#[test]
fn exact_plan_authority_rescue_apply_and_duplicate_refusal_are_one_path(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-complete")?;
    let workspace = "C:/disposable/restore-complete";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let authority = FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current)));
    let clock = TestClock::new(1_700_000_000_000);
    let ids = Arc::new(TestIds::default());
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        authority,
        clock,
        Arc::clone(&ids),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let planned = plan(
        &mut engine,
        workspace,
        checkpoint.clone(),
        "restore-one",
        1_700_000_060_000,
    )?;
    assert_eq!(planned.consequences().writes(), 2);
    assert_eq!(planned.consequences().deletes(), 1);
    assert!(planned
        .preconditions()
        .iter()
        .any(|value| matches!(value, grapt_types::RestorePrecondition::VcsBaseline { .. })));

    let applied = engine.apply_restore(planned)?;
    let receipt = match applied {
        RestoreApplyOutcome::Applied(receipt) => receipt,
        other => return Err(format!("restore did not apply: {other:?}").into()),
    };
    assert_eq!(receipt.operation().as_str(), "restore-one");
    assert_eq!(receipt.state(), &ReconciledState::Applied);

    let persisted = engine.recover(receipt.operation())?;
    assert!(matches!(persisted, RestoreRecoveryOutcome::Applied(_)));

    let duplicate_plan = plan(
        &mut engine,
        workspace,
        checkpoint,
        "restore-one",
        1_700_000_060_000,
    )?;
    assert!(matches!(
        engine.apply_restore(duplicate_plan)?,
        RestoreApplyOutcome::Refused(RestoreApplicationRefusal::DuplicateApply)
    ));
    assert_eq!(
        state.lock().map_err(|_| "state")?.capture_count,
        3,
        "source plus one rescue per admitted attempt; duplicate never reaches provider apply"
    );
    Ok(())
}

#[test]
fn wrong_authority_is_refused_before_rescue_or_destructive_effect(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-wrong-authority")?;
    let workspace = "C:/disposable/wrong-authority";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let authority_state = Arc::new(Mutex::new(AuthorityMode::Current));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::clone(&authority_state)),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let first = plan(
        &mut engine,
        workspace,
        checkpoint.clone(),
        "restore-a",
        1_000,
    )?;
    let foreign = first.required_capability().clone();
    let second = plan(&mut engine, workspace, checkpoint, "restore-b", 1_000)?;
    *authority_state.lock().map_err(|_| "authority")? = AuthorityMode::Fixed(foreign);
    assert!(matches!(
        engine.apply_restore(second)?,
        RestoreApplyOutcome::Refused(RestoreApplicationRefusal::AuthorityMismatch)
    ));
    assert_eq!(state.lock().map_err(|_| "state")?.capture_count, 1);
    Ok(())
}

#[test]
fn stale_plan_and_changed_vcs_baseline_are_pre_effect_refusals(
) -> Result<(), Box<dyn std::error::Error>> {
    let workspace = "C:/disposable/preconditions";

    let stale_root = TestRoot::new("restore-stale")?;
    let stale_state = Arc::new(Mutex::new(ProviderState::new()));
    let clock = TestClock::new(100);
    let mut stale_engine = engine(
        stale_state,
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        stale_root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        clock.clone(),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut stale_engine, workspace)?;
    let stale = plan(
        &mut stale_engine,
        workspace,
        checkpoint,
        "restore-stale",
        200,
    )?;
    clock.set(201);
    assert!(matches!(
        stale_engine.apply_restore(stale)?,
        RestoreApplyOutcome::Refused(RestoreApplicationRefusal::Admission(ref receipt))
            if matches!(receipt.reason(), grapt_types::RestoreRefusalReason::PlanExpired { .. })
    ));

    let changed_root = TestRoot::new("restore-changed-baseline")?;
    let changed_state = Arc::new(Mutex::new(ProviderState::new()));
    changed_state
        .lock()
        .map_err(|_| "state")?
        .echo_vcs_precondition = false;
    let observations = vec![
        symbolic_observation(workspace, HEAD_A)?,
        symbolic_observation(workspace, HEAD_A)?,
        symbolic_observation(workspace, HEAD_A)?,
        symbolic_observation(workspace, HEAD_B)?,
        symbolic_observation(workspace, HEAD_B)?,
        symbolic_observation(workspace, HEAD_B)?,
    ];
    let mut changed_engine = engine(
        changed_state,
        RescueSupport::Supported,
        ScriptedObserver::sequence(observations)?,
        changed_root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut changed_engine, workspace)?;
    let changed = plan(
        &mut changed_engine,
        workspace,
        checkpoint,
        "restore-changed",
        1_000,
    )?;
    assert!(matches!(
        changed_engine.apply_restore(changed)?,
        RestoreApplyOutcome::Refused(RestoreApplicationRefusal::Admission(ref receipt))
            if matches!(receipt.reason(), grapt_types::RestoreRefusalReason::PreconditionNotSatisfied {
                precondition: grapt_types::RestorePrecondition::VcsBaseline { .. }
            })
    ));
    Ok(())
}

#[test]
fn provider_cannot_self_assert_the_independent_vcs_precondition(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-provider-vcs-assertion")?;
    let workspace = "C:/disposable/provider-vcs-assertion";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let observations = vec![
        symbolic_observation(workspace, HEAD_A)?,
        symbolic_observation(workspace, HEAD_A)?,
        symbolic_observation(workspace, HEAD_A)?,
        symbolic_observation(workspace, HEAD_B)?,
        symbolic_observation(workspace, HEAD_B)?,
        symbolic_observation(workspace, HEAD_B)?,
    ];
    let mut engine = engine(
        state,
        RescueSupport::Supported,
        ScriptedObserver::sequence(observations)?,
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let planned = plan(
        &mut engine,
        workspace,
        checkpoint,
        "restore-provider-vcs-assertion",
        1_000,
    )?;
    assert!(
        matches!(
            engine.apply_restore(planned)?,
            RestoreApplyOutcome::Refused(RestoreApplicationRefusal::Admission(ref receipt))
                if matches!(receipt.reason(), grapt_types::RestoreRefusalReason::PreconditionNotSatisfied {
                    precondition: grapt_types::RestorePrecondition::VcsBaseline { .. }
                })
        ),
        "WCP04: the checkpoint provider cannot vouch for the independent VCS observer"
    );
    Ok(())
}

#[test]
fn missing_rescue_and_explicit_authority_refusal_never_apply(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-missing-rescue")?;
    let workspace = "C:/disposable/missing-rescue";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let authority_state = Arc::new(Mutex::new(AuthorityMode::Current));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::clone(&authority_state)),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let no_rescue = plan(
        &mut engine,
        workspace,
        checkpoint.clone(),
        "restore-no-rescue",
        1_000,
    )?;
    state.lock().map_err(|_| "state")?.capture_mode = CaptureMode::Skip;
    assert!(matches!(
        engine.apply_restore(no_rescue)?,
        RestoreApplyOutcome::Refused(RestoreApplicationRefusal::RescueUnavailable(_))
    ));

    state.lock().map_err(|_| "state")?.capture_mode = CaptureMode::Exact;
    *authority_state.lock().map_err(|_| "authority")? = AuthorityMode::Refuse;
    let denied = plan(&mut engine, workspace, checkpoint, "restore-denied", 1_000)?;
    assert!(matches!(
        engine.apply_restore(denied)?,
        RestoreApplyOutcome::Refused(RestoreApplicationRefusal::AuthorityRefused)
    ));
    Ok(())
}

#[test]
fn degraded_rescue_is_not_proven_rescue_coverage() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-degraded-rescue")?;
    let workspace = "C:/disposable/degraded-rescue";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let planned = plan(
        &mut engine,
        workspace,
        checkpoint,
        "restore-degraded-rescue",
        1_000,
    )?;
    state.lock().map_err(|_| "state")?.capture_mode = CaptureMode::Degraded;
    assert!(
        matches!(
            engine.apply_restore(planned)?,
            RestoreApplyOutcome::Refused(RestoreApplicationRefusal::RescueUnavailable(_))
        ),
        "WCP06: degraded capture cannot prove rescue coverage"
    );
    Ok(())
}

#[test]
fn interrupted_apply_is_reconciled_after_engine_reconstruction(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-interrupted-applied")?;
    let workspace = "C:/disposable/interrupted-applied";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    state.lock().map_err(|_| "state")?.apply_mode = ApplyMode::InterruptAfterApply;
    let ids = Arc::new(TestIds::default());
    let mut first = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::clone(&ids),
    )?;
    let checkpoint = capture_source(&mut first, workspace)?;
    let interrupted = plan(
        &mut first,
        workspace,
        checkpoint,
        "restore-interrupted",
        1_000,
    )?;
    assert!(matches!(
        first.apply_restore(interrupted)?,
        RestoreApplyOutcome::Ambiguous { .. }
    ));
    drop(first);

    let mut restarted = engine(
        state,
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Refuse))),
        TestClock::new(101),
        ids,
    )?;
    let operation = operation("restore-interrupted")?;
    let recovered = restarted.recover(&operation)?;
    assert!(matches!(recovered, RestoreRecoveryOutcome::Applied(_)));
    assert!(matches!(
        restarted.recover(&operation)?,
        RestoreRecoveryOutcome::Applied(_)
    ));
    Ok(())
}

#[test]
fn unresolved_interruption_is_quarantined_after_one_inspect_and_reconcile(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-interrupted-quarantine")?;
    let workspace = "C:/disposable/interrupted-quarantine";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    {
        let mut held = state.lock().map_err(|_| "state")?;
        held.apply_mode = ApplyMode::Ambiguous;
        held.reconcile_to = crate::ProviderOperationState::Ambiguous;
    }
    let ids = Arc::new(TestIds::default());
    let mut first = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::clone(&ids),
    )?;
    let checkpoint = capture_source(&mut first, workspace)?;
    let interrupted = plan(
        &mut first,
        workspace,
        checkpoint,
        "restore-quarantine",
        1_000,
    )?;
    assert!(matches!(
        first.apply_restore(interrupted)?,
        RestoreApplyOutcome::Ambiguous { .. }
    ));
    drop(first);

    let mut restarted = engine(
        state,
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Refuse))),
        TestClock::new(101),
        ids,
    )?;
    assert!(matches!(
        restarted.recover(&operation("restore-quarantine")?)?,
        RestoreRecoveryOutcome::Quarantined(_)
    ));
    Ok(())
}

#[test]
fn provider_without_rescue_support_cannot_plan_a_destructive_restore(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-rescue-unsupported")?;
    let workspace = "C:/disposable/rescue-unsupported";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Unsupported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let planned = plan(
        &mut engine,
        workspace,
        checkpoint,
        "restore-no-provider-rescue",
        1_000,
    );
    let refusal = planned
        .err()
        .ok_or("restore planning unexpectedly succeeded")?;
    assert!(matches!(
        refusal.downcast_ref::<crate::CheckpointError>(),
        Some(crate::CheckpointError::RescueUnsupported)
    ));
    assert_eq!(
        state.lock().map_err(|_| "state")?.capture_count,
        1,
        "planning refusal performs neither a fictional rescue nor a provider restore"
    );
    Ok(())
}

#[test]
fn provider_refusal_is_durable_and_recovers_as_refused() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("restore-provider-refusal")?;
    let workspace = "C:/disposable/restore-provider-refusal";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let planned = plan(
        &mut engine,
        workspace,
        checkpoint,
        "restore-provider-refusal",
        1_000,
    )?;
    state.lock().map_err(|_| "state")?.apply_mode = ApplyMode::Refuse;
    assert!(matches!(
        engine.apply_restore(planned)?,
        RestoreApplyOutcome::Refused(RestoreApplicationRefusal::Provider(
            crate::ProviderRefusal::PreconditionsChanged
        ))
    ));
    assert!(matches!(
        engine.recover(&operation("restore-provider-refusal")?)?,
        RestoreRecoveryOutcome::Refused(_)
    ));
    Ok(())
}

#[test]
fn mismatched_provider_receipt_is_quarantined_not_applied() -> Result<(), Box<dyn std::error::Error>>
{
    let root = TestRoot::new("restore-mismatched-receipt")?;
    let workspace = "C:/disposable/mismatched-receipt";
    let state = Arc::new(Mutex::new(ProviderState::new()));
    let mut engine = engine(
        Arc::clone(&state),
        RescueSupport::Supported,
        ScriptedObserver::repeated(symbolic_observation(workspace, HEAD_A)?),
        root.journal()?,
        FakeAuthority(Arc::new(Mutex::new(AuthorityMode::Current))),
        TestClock::new(100),
        Arc::new(TestIds::default()),
    )?;
    let checkpoint = capture_source(&mut engine, workspace)?;
    let planned = plan(
        &mut engine,
        workspace,
        checkpoint,
        "restore-mismatched-receipt",
        1_000,
    )?;
    state.lock().map_err(|_| "state")?.apply_mode = ApplyMode::MismatchedReceipt;
    assert!(
        matches!(
            engine.apply_restore(planned)?,
            RestoreApplyOutcome::Refused(RestoreApplicationRefusal::ProviderReceiptMismatch)
        ),
        "WCP09: a provider receipt with different consequences is not success"
    );
    assert!(matches!(
        engine.recover(&operation("restore-mismatched-receipt")?)?,
        RestoreRecoveryOutcome::Quarantined(_)
    ));
    Ok(())
}
