use super::support::*;
use super::*;

#[test]
fn an_admission_never_claims_the_external_effect_happened() -> Result<(), Box<dyn std::error::Error>>
{
    // ITEM 4. There used to be `RestoreInFlight::applied(anchor, ..)`, which
    // minted an `Applied` terminal from a caller assertion — "the restore
    // landed" was whatever the caller said — and the in-flight token could
    // be dropped with no receipt at all despite the doc promising exactly
    // one. Both are gone. What remains is honest: an admission records what
    // this crate established and says nothing about the provider.
    let plan = settled_plan().ok_or("plan")?;
    let rescue = rescue_for(&plan).ok_or("rescue")?;
    let lease = lease_for(plan.required_capability()).ok_or("lease")?;

    let admitted = apply_at(plan, rescue, lease, SETTLED_MS);
    let admission = admitted.admission().ok_or("admission")?;

    assert_eq!(admission.plan().plan().as_str(), "plan-1");
    assert_eq!(admission.admitted_at(), Timestamp::from_millis(SETTLED_MS));
    assert!(matches!(
        admission.rescue(),
        RestoreRescueEvidence::Captured { .. }
    ));

    // The effect is ambiguous from here, which is what stops a replay.
    assert!(matches!(
        admission.external_effect(),
        Some(ExternalEffectState::Ambiguous { .. })
    ));
    // And there is no method that would say otherwise: `applied()`,
    // `interrupted()` and `quarantined()` do not exist on this type.
    Ok(())
}

#[test]
fn a_precondition_the_plan_requires_must_be_observed() -> Result<(), Box<dyn std::error::Error>> {
    // ITEM 3. `WorktreeQuiescent` is bound into every test plan. An
    // observation that does not cover it is refused BEFORE admission.
    let plan = settled_plan().ok_or("plan")?;
    let rescue = rescue_for(&plan).ok_or("rescue")?;
    let lease = lease_for(plan.required_capability()).ok_or("lease")?;

    let clock = FixedClock(SETTLED_MS);
    let ids = FixedIds;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let empty =
        PlanPreconditionObservation::observe_at_provider_boundary(&plan, Vec::new(), &authorities);

    let admission = apply_with(plan, rescue, empty, lease, SETTLED_MS);
    assert!(admission.admission().is_none());
    assert_eq!(
        admission.refusal().ok_or("refusal")?.reason(),
        &RestoreRefusalReason::PreconditionNotSatisfied {
            precondition: RestorePrecondition::WorktreeQuiescent,
        }
    );
    Ok(())
}

#[test]
fn an_observation_for_another_plan_never_admits_this_one() -> Result<(), Box<dyn std::error::Error>>
{
    let plan = settled_plan().ok_or("plan")?;
    let other = settled_plan_with(
        "plan-other",
        work_root(),
        vec![CheckpointLaneId::new("files").ok_or("lane")?],
        Vec::new(),
        SETTLED_MS + TTL_MS,
        SETTLED_MS,
    )
    .ok_or("plan")?;
    let rescue = rescue_for(&plan).ok_or("rescue")?;
    let lease = lease_for(plan.required_capability()).ok_or("lease")?;
    let foreign = observation_for(&other, SETTLED_MS);

    let admission = apply_with(plan, rescue, foreign, lease, SETTLED_MS);
    assert_eq!(
        admission.refusal().ok_or("refusal")?.reason(),
        &RestoreRefusalReason::PreconditionsObservedForAnotherPlan
    );
    Ok(())
}

#[test]
fn a_stale_observation_never_admits_a_restore() -> Result<(), Box<dyn std::error::Error>> {
    // An observation taken before the plan existed describes a world that
    // predates the plan.
    let plan = settled_plan().ok_or("plan")?;
    let rescue = rescue_for(&plan).ok_or("rescue")?;
    let lease = lease_for(plan.required_capability()).ok_or("lease")?;
    let stale = observation_for(&plan, SETTLED_MS - 1);

    let admission = apply_with(plan, rescue, stale, lease, SETTLED_MS);
    assert_eq!(
        admission.refusal().ok_or("refusal")?.reason(),
        &RestoreRefusalReason::PreconditionObservationOutsidePlanWindow {
            observed_at: Timestamp::from_millis(SETTLED_MS - 1),
        }
    );
    Ok(())
}
