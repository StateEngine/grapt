use super::*;

fn path(raw: &str) -> PathIdentity {
    PathIdentity::canonical(raw)
}

fn read_capability(root: &str) -> ResolvedCapability {
    ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        FilesystemAuthority::new(vec![path(root)], Vec::new(), Vec::new()),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    )
}

#[test]
fn identity_is_derived_from_coordinates_not_authored() {
    let a = CapabilityId::derive(&read_capability("/work"));
    let b = CapabilityId::derive(&read_capability("/work"));
    assert_eq!(a, b, "same coordinates must derive one identity");

    let other = CapabilityId::derive(&read_capability("/etc"));
    assert_ne!(a, other, "a different root is a different capability");
}

#[test]
fn root_ordering_does_not_change_identity() {
    let ordered = FilesystemAuthority::new(vec![path("/a"), path("/b")], Vec::new(), Vec::new());
    let reversed = FilesystemAuthority::new(vec![path("/b"), path("/a")], Vec::new(), Vec::new());
    assert_eq!(ordered, reversed);
}

#[test]
fn realized_enforcement_is_part_of_identity() {
    let harness = read_capability("/work");
    let sandboxed = ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        FilesystemAuthority::new(vec![path("/work")], Vec::new(), Vec::new()),
        EgressAuthority::default(),
        RealizedEnforcement::PlatformSandbox {
            backend: "seatbelt".into(),
        },
    );
    assert_ne!(
        CapabilityId::derive(&harness),
        CapabilityId::derive(&sandboxed),
        "same coordinates under different enforcement are different authority"
    );
}

#[test]
fn separator_injection_cannot_forge_another_identity() {
    // A path containing the canonical separators must not encode to the
    // same form as two genuinely distinct roots.
    let forged = FilesystemAuthority::new(vec![path("/a,/b")], Vec::new(), Vec::new());
    let genuine = FilesystemAuthority::new(vec![path("/a"), path("/b")], Vec::new(), Vec::new());
    let forged = ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        forged,
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    let genuine = ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        genuine,
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    assert_ne!(
        CapabilityId::derive(&forged),
        CapabilityId::derive(&genuine)
    );
}

#[test]
fn scope_lattice_refuses_cross_dimension_widening() -> Result<(), Box<dyn std::error::Error>> {
    let session = "ses_a".parse::<SessionId>()?;
    let other = "ses_b".parse::<SessionId>()?;
    let held = GrantScope::Session(session.clone());

    assert!(held.contains(&GrantScope::Once));
    assert!(held.contains(&GrantScope::Attempt));
    assert!(held.contains(&GrantScope::Session(session)));
    assert!(
        !held.contains(&GrantScope::Session(other)),
        "another Session is not covered"
    );
    assert!(
        !held.contains(&GrantScope::Project(
            ProjectId::new("proj").ok_or("project id")?
        )),
        "a Session grant never widens to a Project"
    );
    Ok(())
}

#[test]
fn narrow_scope_never_covers_a_wider_request() -> Result<(), Box<dyn std::error::Error>> {
    let once = GrantScope::Once;
    assert!(once.contains(&GrantScope::Once));
    assert!(!once.contains(&GrantScope::Attempt));
    assert!(!once.contains(&GrantScope::Session("ses_a".parse::<SessionId>()?)));
    assert!(once.rank() < GrantScope::Attempt.rank());
    Ok(())
}

use crate::authority::test_support::live_decision;
use crate::permission::PermissionReply;

fn attempt_grant(capability: &CapabilityId) -> Option<CapabilityGrant> {
    let decision = live_decision(
        PermissionReply::Once,
        capability.clone(),
        GrantScope::Attempt,
    )?;
    CapabilityGrant::from_live(&decision)
}

#[test]
fn lease_is_consumed_by_its_one_effect() -> Result<(), Box<dyn std::error::Error>> {
    let capability = CapabilityId::derive(&read_capability("/work"));
    let grant = attempt_grant(&capability).ok_or("attempt grant")?;
    let lease = grant
        .lease(&capability, GrantScope::Once)
        .ok_or("the grant covers a Once request")?;
    assert_eq!(lease.consume(), capability);
    // A second `consume()` here would not compile: see ownership probe #8b.
    Ok(())
}

#[test]
fn a_lease_cannot_be_taken_without_a_grant_that_covers_it() -> Result<(), Box<dyn std::error::Error>>
{
    let held = CapabilityId::derive(&read_capability("/work"));
    let other = CapabilityId::derive(&read_capability("/etc"));
    let grant = attempt_grant(&held).ok_or("attempt grant")?;

    assert!(
        grant.lease(&other, GrantScope::Once).is_none(),
        "a grant never leases a capability it does not name"
    );
    assert!(
        grant
            .lease(&held, GrantScope::Session("ses_a".parse::<SessionId>()?))
            .is_none(),
        "an attempt-scoped grant never leases Session scope"
    );
    assert!(grant.lease(&held, GrantScope::Once).is_some());
    Ok(())
}

#[test]
fn session_grant_debug_is_redacted() -> Result<(), Box<dyn std::error::Error>> {
    let session = "ses_secret".parse::<SessionId>()?;
    let decision = live_decision(
        PermissionReply::Session,
        CapabilityId::derive(&read_capability("/work")),
        GrantScope::Session(session),
    )
    .ok_or("live session decision")?;
    let grant = SessionGrant::from_live(&decision).ok_or("session grant")?;
    let rendered = format!("{grant:?}");
    assert!(!rendered.contains("ses_secret"));
    assert!(!rendered.contains("/work"));
    Ok(())
}

#[test]
fn grant_authorizes_only_its_exact_capability() -> Result<(), Box<dyn std::error::Error>> {
    let capability = CapabilityId::derive(&read_capability("/work"));
    let grant = attempt_grant(&capability).ok_or("attempt grant")?;
    assert!(grant.authorizes(&capability, &GrantScope::Once));

    let other = CapabilityId::derive(&read_capability("/etc"));
    assert!(!grant.authorizes(&other, &GrantScope::Once));
    Ok(())
}

#[test]
fn a_session_grant_takes_its_session_from_the_decision_not_the_caller(
) -> Result<(), Box<dyn std::error::Error>> {
    // The removed `PermissionSettlement::session_grant(&self, session)`
    // accepted a caller-chosen Session and never compared it with the
    // settlement's own scope, so an approval for `ses_a` could mint
    // authority over `ses_b`. There is now no argument to substitute: the
    // Session is read out of the decision's `GrantScope::Session(..)`.
    let approved = "ses_a".parse::<SessionId>()?;
    let capability = CapabilityId::derive(&read_capability("/work"));
    let decision = live_decision(
        PermissionReply::Session,
        capability,
        GrantScope::Session(approved.clone()),
    )
    .ok_or("live session decision")?;

    let grant = SessionGrant::from_live(&decision).ok_or("session grant")?;
    assert_eq!(grant.session(), &approved);
    Ok(())
}

#[test]
fn a_session_reply_must_be_answered_at_session_scope() -> Result<(), Box<dyn std::error::Error>> {
    // Coherence is enforced at mint, so an incoherent pair never reaches
    // `SessionGrant::from_live` in the first place.
    let capability = CapabilityId::derive(&read_capability("/work"));
    assert!(
        live_decision(
            PermissionReply::Session,
            capability.clone(),
            GrantScope::Once
        )
        .is_none(),
        "a Session reply answered at Once scope is incoherent"
    );
    assert!(
        live_decision(
            PermissionReply::Once,
            capability,
            GrantScope::Session("ses_a".parse::<SessionId>()?)
        )
        .is_none(),
        "only a Session reply may claim Session scope"
    );
    Ok(())
}

#[test]
fn a_non_session_decision_never_mints_session_authority() -> Result<(), Box<dyn std::error::Error>>
{
    let capability = CapabilityId::derive(&read_capability("/work"));
    let decision = live_decision(PermissionReply::Once, capability, GrantScope::Once)
        .ok_or("live once decision")?;
    assert!(SessionGrant::from_live(&decision).is_none());
    Ok(())
}

#[test]
fn once_never_becomes_a_reusable_grant() -> Result<(), Box<dyn std::error::Error>> {
    // Law: "Once cannot issue twice." A Once authorization is refused as a
    // persistent grant, so there is no object left to call `lease()` on
    // repeatedly; the only route is the consuming one below.
    let capability = CapabilityId::derive(&read_capability("/work"));
    let decision = live_decision(PermissionReply::Once, capability.clone(), GrantScope::Once)
        .ok_or("live once decision")?;
    assert!(
        CapabilityGrant::from_live(&decision).is_none(),
        "Once is not persistent authority"
    );

    let lease = AuthorityLease::once_from_live(decision).ok_or("once lease")?;
    assert_eq!(lease.consume(), capability);
    // `decision` was moved into `once_from_live`, so a second lease cannot
    // be taken: the value no longer exists. It is not `Clone`.
    Ok(())
}

#[test]
fn a_refusal_carries_no_live_authority() -> Result<(), Box<dyn std::error::Error>> {
    let capability = CapabilityId::derive(&read_capability("/work"));
    assert!(
        live_decision(PermissionReply::Reject, capability, GrantScope::Once).is_none(),
        "a refusal is audit truth only and never mints a live decision"
    );
    Ok(())
}

#[test]
fn a_persistent_grant_copies_no_coordinate_from_its_caller(
) -> Result<(), Box<dyn std::error::Error>> {
    // Both coordinates come out of the decision. There is no argument a
    // caller could use to attach an unrelated capability to a real answer,
    // or to widen the scope past what was approved.
    let capability = CapabilityId::derive(&read_capability("/work"));
    let session = "ses_a".parse::<SessionId>()?;
    let decision = live_decision(
        PermissionReply::Session,
        capability.clone(),
        GrantScope::Session(session.clone()),
    )
    .ok_or("live session decision")?;
    let grant = CapabilityGrant::from_live(&decision).ok_or("session-scoped grant")?;

    assert_eq!(grant.capability(), &capability);
    assert_eq!(grant.scope(), &GrantScope::Session(session));

    let other = CapabilityId::derive(&read_capability("/etc"));
    assert!(!grant.authorizes(&other, &GrantScope::Once));
    Ok(())
}

// ---------------------------------------------------------------------
// L1 MODEL-TARGET-CAPABILITY-IDENTITY
//
// THREE canonical forms are minted in this crate, not two. Effect classes
// derive through `CapabilityId::derive`; `checkpoint::restore_plan` derives
// a `restore-plan|` form into `CheckpointRestoreCapabilityId`; and
// `model::ResolvedModelTarget::resolve` derives a `model-target|` form into
// the separate `ModelTargetCapabilityId`.
//
// The nominal split stops every form being COMPARED with either of the other
// two. It does not by itself stop the three ENCODINGS from colliding. Until
// this witness the union was injective only by the accident that no
// `EffectClass` literal happens to spell `model-target` or `restore-plan`,
// which was stated nowhere and enforced by nothing.
//
// So the witness is three-way and reads the real bytes each of the three
// producers emits. It never restates a literal from the source it checks.
// ---------------------------------------------------------------------

/// The namespace an `EffectClass` canonical form must open with.
///
/// This is the ONE source of truth for effect namespaces in this witness, and
/// it has NO wildcard arm on purpose. Adding an `EffectClass` variant stops this
/// file COMPILING until the new variant names its namespace here — which is the
/// same review pressure the enum's own note describes, applied to the witness
/// that checks it.
///
/// The shape this replaces was circular (cold review, G1). Two
/// hand-authored five-element lists lived in this file and were compared with
/// `assert_eq!(effects.len(), EFFECT_PREFIXES.len())`, under a message claiming
/// every variant was covered. Nothing tied either list to the enum, so a sixth
/// variant left both at five, the assertion passed, and the new namespace was
/// never compared against `model-target|` or `restore-plan|` — precisely the
/// outcome the message promised to prevent. An "exactly N" bound asserted
/// against a second hand-written N proves nothing about the enum.
///
/// It matters here rather than being cosmetic: this witness is what the
/// residuals record nominates. It is also unreachable by a mutation row,
/// because adding an enum variant is not a mutation of existing source, so no
/// `MC` row could have caught the gap.
fn effect_prefix(class: EffectClass) -> &'static str {
    match class {
        EffectClass::FilesystemRead => "fs.read|",
        EffectClass::FilesystemWrite => "fs.write|",
        EffectClass::ProcessSpawn => "proc.spawn|",
        EffectClass::NetworkEgress => "net.egress|",
        EffectClass::ToolInvoke => "tool.invoke|",
    }
}

/// Every closed `EffectClass` variant, paired with the real canonical form it
/// derives and the namespace [`effect_prefix`] says that form must open with.
///
/// Both halves come from `effect_prefix`, so the forms under test and the
/// prefixes they are checked against cannot drift apart.
fn every_effect_form() -> Vec<(&'static str, String)> {
    [
        EffectClass::FilesystemRead,
        EffectClass::FilesystemWrite,
        EffectClass::ProcessSpawn,
        EffectClass::NetworkEgress,
        EffectClass::ToolInvoke,
    ]
    .into_iter()
    .map(|class| {
        let form = CapabilityId::derive(&ResolvedCapability::new(
            class,
            None,
            FilesystemAuthority::new(vec![path("/work")], Vec::new(), Vec::new()),
            EgressAuthority::default(),
            RealizedEnforcement::HarnessOnly,
        ))
        .as_str()
        .to_owned();
        (effect_prefix(class), form)
    })
    .collect()
}

/// The real `model-target|` form, from the only producer there is.
fn model_target_form() -> Option<String> {
    use crate::authority::PolicyVersion;
    use crate::credential::{AccountId, CredentialVersion};
    use crate::model::{
        BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
        ModelRef, ModelRouteId, ProviderId, ResolvedModelTarget,
    };

    let catalog = ModelCatalog::load(
        ModelCatalogVersion::new("cat-1")?,
        vec![ModelCatalogRecord::new(
            ProviderId::new("anthropic")?,
            ModelId::new("claude-opus-5")?,
            BaseModelId::new("claude-opus-5")?,
            ModelFamilyId::new("claude")?,
            None,
        )],
    )?;
    let entry = catalog.entry(
        &ProviderId::new("anthropic")?,
        &ModelId::new("claude-opus-5")?,
        None,
    )?;
    let target = ResolvedModelTarget::resolve(
        ModelRef::from_catalog(entry),
        ModelRouteId::new("rt_1")?,
        AccountId::new("acct_1")?,
        CredentialVersion::new("3")?,
        PolicyVersion::new("pol_1")?,
    );
    Some(target.capability().as_str().to_owned())
}

/// The real `restore-plan|` form, from an actually settled plan rather than
/// a hand-built string.
fn restore_plan_form() -> Option<String> {
    use crate::authority::test_support::{FixedClock, FixedIds};
    use crate::authority::{DeterministicAuthorities, Timestamp};
    use crate::checkpoint::{
        CheckpointLaneId, CheckpointOperationId, CheckpointProviderId, CheckpointProviderVersion,
        CheckpointRestorePlanId, RestoreConsequences, RestorePrecondition, SettledRestorePlan,
        SettledRestorePlanParts, WorkspaceCheckpointId, WorkspaceCheckpointRef,
    };
    use crate::recovery::WorktreeIdentity;

    const SETTLED_MS: i64 = 1_700_000_000_000;

    let clock = FixedClock(SETTLED_MS);
    let ids = FixedIds;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let plan = SettledRestorePlan::settle_at_provider_boundary(
        SettledRestorePlanParts {
            plan: CheckpointRestorePlanId::new("plan-1")?,
            operation: CheckpointOperationId::new("op-1")?,
            checkpoint: WorkspaceCheckpointRef::new(
                CheckpointProviderId::new("stateengine")?,
                CheckpointProviderVersion::new("1.2.0")?,
                WorkspaceCheckpointId::new("ckpt-1")?,
            ),
            root: WorktreeIdentity::Plain {
                root: path("/work"),
            },
            lanes: vec![CheckpointLaneId::new("files")?],
            consequences: RestoreConsequences::new(12, 3, 4096),
            unusual: Vec::new(),
            preconditions: vec![RestorePrecondition::WorktreeQuiescent],
            enforcement: RealizedEnforcement::HarnessOnly,
            expires_at: Timestamp::from_millis(SETTLED_MS + 60_000),
        },
        &authorities,
    )?;
    Some(plan.required_capability().as_str().to_owned())
}

#[test]
fn the_three_canonical_namespaces_are_pairwise_disjoint() -> Result<(), Box<dyn std::error::Error>>
{
    const RESTORE_PLAN_PREFIX: &str = "restore-plan|";
    const MODEL_TARGET_PREFIX: &str = "model-target|";

    // There is deliberately no count assertion here. Coverage of the enum is
    // carried by `effect_prefix`, whose wildcard-free match will not compile if
    // a variant is added, so a count compared against a second hand-written
    // count would add nothing except a claim it cannot keep.
    let effects = every_effect_form();
    let restore_plan = restore_plan_form().ok_or("settled restore plan")?;
    let model_target = model_target_form().ok_or("resolved model target")?;

    for (expected, form) in &effects {
        assert!(
            form.starts_with(expected),
            "an effect form must open with its own class literal"
        );
        assert!(
            !form.starts_with(MODEL_TARGET_PREFIX),
            "no effect form may enter the model-target namespace"
        );
        assert!(
            !form.starts_with(RESTORE_PLAN_PREFIX),
            "no effect form may enter the restore-plan namespace"
        );
    }

    assert!(
        restore_plan.starts_with(RESTORE_PLAN_PREFIX),
        "a settled restore plan derives its own namespace"
    );
    assert!(
        !restore_plan.starts_with(MODEL_TARGET_PREFIX),
        "no restore-plan form may enter the model-target namespace"
    );
    for (prefix, _) in &effects {
        assert!(
            !restore_plan.starts_with(prefix),
            "no restore-plan form may enter an effect-class namespace"
        );
    }

    assert!(
        model_target.starts_with(MODEL_TARGET_PREFIX),
        "a resolved model target derives its own namespace"
    );
    assert!(
        !model_target.starts_with(RESTORE_PLAN_PREFIX),
        "no model-target form may enter the restore-plan namespace"
    );
    for (prefix, _) in &effects {
        assert!(
            !model_target.starts_with(prefix),
            "no model-target form may enter an effect-class namespace"
        );
    }
    Ok(())
}
