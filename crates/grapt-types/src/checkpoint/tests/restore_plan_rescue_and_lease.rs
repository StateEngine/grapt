use super::support::*;

#[test]
fn unusual_conditions_dedupe_even_when_not_adjacent() -> Result<(), Box<dyn std::error::Error>> {
    // `Vec::dedup` alone removes only CONSECUTIVE duplicates, so a planner
    // reporting `[A, B, A]` would leave two `A`s in the settled plan. The
    // vocabulary is deliberately not `Ord`, so it cannot be sorted first.
    let plan = settled_plan_with(
        "plan-dedupe",
        work_root(),
        vec![CheckpointLaneId::new("files").ok_or("lane")?],
        vec![
            UnusualRestoreCondition::ProviderRescueUnsupported,
            UnusualRestoreCondition::UncommittedChangesPresent,
            UnusualRestoreCondition::ProviderRescueUnsupported,
        ],
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("plan")?;

    assert_eq!(
        plan.unusual_conditions().len(),
        2,
        "a repeated condition is one condition, wherever it appears"
    );
    // First-occurrence order is preserved: the receipt reads in the order
    // the planner reported.
    assert_eq!(
        plan.unusual_conditions()[0],
        UnusualRestoreCondition::ProviderRescueUnsupported
    );
    assert!(plan.records_unusual(&UnusualRestoreCondition::UncommittedChangesPresent));
    Ok(())
}

#[test]
fn a_plan_that_selects_no_lane_is_not_a_plan() {
    assert!(
        settled_plan_with(
            "plan-1",
            work_root(),
            Vec::new(),
            Vec::new(),
            SETTLED_MS + TTL_MS,
            SETTLED_MS,
        )
        .is_none(),
        "an empty lane selection is not an exact consequence set"
    );
}

#[test]
fn a_plan_born_expired_is_unbuildable() -> Result<(), Box<dyn std::error::Error>> {
    let lanes = vec![CheckpointLaneId::new("files").ok_or("lane")?];
    assert!(
        settled_plan_with(
            "plan-1",
            work_root(),
            lanes.clone(),
            Vec::new(),
            SETTLED_MS,
            SETTLED_MS,
        )
        .is_none(),
        "an expiry that is already now is not an expiry"
    );
    assert!(settled_plan_with(
        "plan-1",
        work_root(),
        lanes,
        Vec::new(),
        SETTLED_MS - 1,
        SETTLED_MS,
    )
    .is_none());
    Ok(())
}

#[test]
fn the_required_capability_binds_the_whole_plan_not_just_its_root(
) -> Result<(), Box<dyn std::error::Error>> {
    // ITEM 2. The derivation used to be `FilesystemWrite + root +
    // enforcement`, so every plan touching one root shared one capability:
    // a lease approved for a small restore authorized a plan that deleted
    // the tree. Authority for a destructive effect must name the exact
    // effect.
    let plan = settled_plan().ok_or("plan")?;

    // Same root, same enforcement, materially different plan.
    let different_lanes = settled_plan_with(
        "plan-1",
        work_root(),
        vec![
            CheckpointLaneId::new("files").ok_or("lane")?,
            CheckpointLaneId::new("env").ok_or("lane")?,
        ],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("plan")?;
    assert_ne!(
        plan.required_capability(),
        different_lanes.required_capability(),
        "another lane selection is another governed effect on the same root"
    );

    // Same root and lanes, different plan identity.
    let other_plan_id = settled_plan_with(
        "plan-2",
        work_root(),
        vec![CheckpointLaneId::new("files").ok_or("lane")?],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("plan")?;
    assert_ne!(
        plan.required_capability(),
        other_plan_id.required_capability(),
        "a different plan is a different authorization"
    );

    // Another root remains a different capability, as before.
    let elsewhere = settled_plan_with(
        "plan-3",
        WorktreeIdentity::Plain {
            root: PathIdentity::canonical("/other"),
        },
        vec![CheckpointLaneId::new("files").ok_or("lane")?],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("plan")?;
    assert_ne!(plan.required_capability(), elsewhere.required_capability());
    // Ordinary effect capabilities and restore-plan capabilities are now
    // nominally different; probe 54 makes substitution a compile error.
    Ok(())
}

#[test]
fn a_lease_for_a_sibling_plan_on_the_same_root_is_refused() -> Result<(), Box<dyn std::error::Error>>
{
    // The same-root/different-plan refusal review asked to witness end to
    // end: authority approved for one plan cannot apply its sibling.
    let approved = settled_plan().ok_or("plan")?;
    let sibling = settled_plan_with(
        "plan-sibling",
        work_root(),
        vec![
            CheckpointLaneId::new("files").ok_or("lane")?,
            CheckpointLaneId::new("env").ok_or("lane")?,
        ],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("plan")?;

    let lease = lease_for(approved.required_capability()).ok_or("lease")?;
    let rescue = rescue_for(&sibling).ok_or("rescue")?;
    let admission = apply_at(sibling, rescue, lease, SETTLED_MS);

    assert!(admission.admission().is_none());
    assert_eq!(
        admission.refusal().ok_or("refusal")?.reason(),
        &RestoreRefusalReason::LeaseNamesAnotherCapability
    );
    Ok(())
}

#[test]
fn a_lease_for_another_capability_never_applies_a_restore() -> Result<(), Box<dyn std::error::Error>>
{
    let plan = settled_plan().ok_or("plan")?;
    let other = settled_plan_with(
        "plan-other-capability",
        work_root(),
        vec![CheckpointLaneId::new("files").ok_or("lane")?],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("other plan")?;
    let rescue = rescue_for(&plan).ok_or("rescue")?;
    let lease = lease_for(other.required_capability()).ok_or("lease")?;

    let admission = apply_at(plan, rescue, lease, SETTLED_MS);
    assert!(admission.admission().is_none());
    let receipt = admission.refusal().ok_or("refusal receipt")?;
    assert_eq!(
        receipt.reason(),
        &RestoreRefusalReason::LeaseNamesAnotherCapability
    );
    assert_eq!(
        receipt.external_effect(),
        ExternalEffectState::NoneInitiated,
        "a refusal entered no external effect"
    );
    // The lease was moved into `apply` and consumed there, so the refused
    // caller cannot retry under the same authorization.
    Ok(())
}

#[test]
fn an_expired_plan_refuses_at_the_governed_entry() -> Result<(), Box<dyn std::error::Error>> {
    let plan = settled_plan().ok_or("plan")?;
    let rescue = rescue_for(&plan).ok_or("rescue")?;
    let lease = lease_for(plan.required_capability()).ok_or("lease")?;

    let admission = apply_at(plan, rescue, lease, SETTLED_MS + TTL_MS + 1);
    let receipt = admission.refusal().ok_or("refusal receipt")?;
    assert_eq!(
        receipt.reason(),
        &RestoreRefusalReason::PlanExpired {
            expired_at: Timestamp::from_millis(SETTLED_MS + TTL_MS),
            observed: Timestamp::from_millis(SETTLED_MS + TTL_MS + 1),
        }
    );
    Ok(())
}

#[test]
fn a_rescue_proven_for_another_plan_never_admits_this_one() -> Result<(), Box<dyn std::error::Error>>
{
    let plan = settled_plan().ok_or("plan")?;
    let other = settled_plan_with(
        "plan-2",
        work_root(),
        vec![CheckpointLaneId::new("files").ok_or("lane")?],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("other plan")?;
    let rescue = rescue_for(&other).ok_or("rescue")?;
    let lease = lease_for(plan.required_capability()).ok_or("lease")?;

    let admission = apply_at(plan, rescue, lease, SETTLED_MS);
    let receipt = admission.refusal().ok_or("refusal receipt")?;
    assert_eq!(
        receipt.reason(),
        &RestoreRefusalReason::RescueProvenForAnotherPlan
    );
    Ok(())
}

#[test]
fn a_rescue_is_never_the_checkpoint_being_restored() -> Result<(), Box<dyn std::error::Error>> {
    let plan = settled_plan().ok_or("plan")?;
    let source = rescue_anchor_for(plan.root().clone(), "ckpt-1").ok_or("anchor")?;
    assert_eq!(source.checkpoint(), plan.checkpoint());
    assert!(
        RestoreRescue::captured_at_provider_boundary(&plan, &source).is_none(),
        "the restore source is not a rescue of the state it overwrites"
    );
    Ok(())
}

#[test]
fn a_rescue_of_another_worktree_never_covers_this_root() -> Result<(), Box<dyn std::error::Error>> {
    let plan = settled_plan().ok_or("plan")?;
    let elsewhere = rescue_anchor_for(
        WorktreeIdentity::Plain {
            root: PathIdentity::canonical("/other"),
        },
        "ckpt-rescue",
    )
    .ok_or("anchor")?;
    assert!(RestoreRescue::captured_at_provider_boundary(&plan, &elsewhere).is_none());
    Ok(())
}

#[test]
fn rescue_is_waivable_only_when_the_plan_records_it_unsupported(
) -> Result<(), Box<dyn std::error::Error>> {
    let ordinary = settled_plan().ok_or("plan")?;
    assert!(
        RestoreRescue::waived_as_unsupported(&ordinary).is_none(),
        "an agent cannot waive rescue for a plan whose authority never saw the condition"
    );

    let unsupported = settled_plan_with(
        "plan-3",
        work_root(),
        vec![CheckpointLaneId::new("files").ok_or("lane")?],
        vec![UnusualRestoreCondition::ProviderRescueUnsupported],
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("plan")?;
    let waiver = RestoreRescue::waived_as_unsupported(&unsupported).ok_or("waiver")?;
    assert_eq!(
        waiver.evidence(),
        &RestoreRescueEvidence::WaivedProviderUnsupported
    );

    let lease = lease_for(unsupported.required_capability()).ok_or("lease")?;
    let admission = apply_at(unsupported, waiver, lease, SETTLED_MS);
    assert!(admission.admission().is_some());
    Ok(())
}
