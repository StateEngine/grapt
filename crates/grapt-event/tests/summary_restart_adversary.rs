//! The restart adversary for a provider-bound compaction summary.
//!
//! The admission names one control this lane must EXECUTE rather than argue:
//! after a restart that destroys the catalog the fact was authored under,
//! provider/model COMPATIBILITY DATA must survive while catalog ISSUANCE
//! AUTHORITY must not — and no path may report the original issuance restored.
//!
//! The two halves are proven by different instruments and both are needed:
//!
//! * the STRUCTURAL half — that nothing reconstituted from persisted bytes can
//!   speak issuance at all — is a compile-time law, and a runtime test cannot
//!   witness it, because the code that would witness it does not compile. It
//!   lives in `grapt-types/tests/probes/49_reconstituted_issuance_authority.rs`.
//! * the BEHAVIOURAL half is here, executed end to end across a real durability
//!   boundary: encode, drop the catalog, load a DIFFERENT catalog whose version
//!   string is copied but whose sealed claims differ, decode, and redeem.
//!
//! The adversary is hostile on purpose. The replacement catalog copies the
//! original's version string — the one coordinate an attacker can trivially
//! forge — precisely so that a rule which consulted it would look green here
//! and be wrong. `an_audit_version_change_alone_still_redeems` is the witness
//! that pins the version OUT of the redemption rule in the other direction.

use grapt_event::{decode, encode};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, BaseModelId, CompactionDescriptor,
    CompactionDescriptorParts, CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch,
    ContextFailure, ContextWindowId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion,
    ModelFamilyId, ModelId, ModelRef, NextRequestRef, ProviderId, RetainedHistory,
    RetentionPolicyVersion, StepNumber, SummaryCompatibility,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn artifact(id: &str) -> Option<ArtifactRef> {
    Some(ArtifactRef::new(
        ArtifactId::new(id)?,
        7,
        ArtifactDigest::new("digest")?,
    ))
}

/// A catalog whose version string and row coordinates are caller-chosen, so a
/// replacement can copy the original's *audit* identity while its sealed
/// claims — base model and family, which no public read path exposes — differ.
fn catalog(version: &str, base: &str, family: &str) -> Option<ModelCatalog> {
    ModelCatalog::load(
        ModelCatalogVersion::new(version)?,
        vec![
            ModelCatalogRecord::new(
                ProviderId::new("anthropic")?,
                ModelId::new("claude-opus-5")?,
                BaseModelId::new(base)?,
                ModelFamilyId::new(family)?,
                None,
            ),
            ModelCatalogRecord::new(
                ProviderId::new("openai")?,
                ModelId::new("gpt-x")?,
                BaseModelId::new("gpt-x")?,
                ModelFamilyId::new("gpt")?,
                None,
            ),
        ],
    )
}

fn issued(catalog: &ModelCatalog, provider: &str, model: &str) -> Option<ModelRef> {
    Some(ModelRef::from_catalog(catalog.entry(
        &ProviderId::new(provider)?,
        &ModelId::new(model)?,
        None,
    )?))
}

fn bound_descriptor(target: &ModelRef) -> Option<CompactionDescriptor> {
    CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new("cmp-restart")?,
        source_session: "ses_033601c26ffe".parse().ok()?,
        source_epoch: ContextEpoch::new(1),
        target_window: ContextWindowId::new("cwn-restart")?,
        target_epoch: ContextEpoch::new(2),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new("ret-1")?,
        summary: CompactionSummary::ProviderBound {
            artifact: artifact("sum-bound")?,
            compatibility: SummaryCompatibility::bind(target),
        },
        retained: RetainedHistory::References {
            artifacts: vec![artifact("hist-1")?],
        },
        baseline: artifact("base-1")?,
        next_request: NextRequestRef::new(StepNumber::new(3)?, artifact("req-1")?),
    })
    .ok()
}

/// Author the fact under one catalog, then destroy that catalog entirely and
/// return only what a restart actually keeps: the persisted bytes.
fn authored_under_a_destroyed_catalog(
) -> Result<(serde_json::Value, ModelRef), Box<dyn std::error::Error>> {
    let original = catalog("release-1", "claude-opus-5", "claude").ok_or("catalog")?;
    let authoring_target = issued(&original, "anthropic", "claude-opus-5").ok_or("target")?;
    let payload = encode(&bound_descriptor(&authoring_target).ok_or("descriptor")?);

    // The catalog that vouched for the coordinates is gone. `authoring_target`
    // outlives it ONLY as a measuring instrument for
    // `the_original_issuance_does_not_survive_the_restart`; no redemption in
    // this file is ever attempted against it, because a restart would not have
    // it.
    drop(original);
    Ok((payload, authoring_target))
}

#[test]
fn compatibility_data_survives_a_catalog_restart() -> TestResult {
    let (payload, _authoring_target) = authored_under_a_destroyed_catalog()?;

    // Restart: a NEW catalog. Its version string is copied from the original,
    // and its sealed claims are not.
    let reloaded =
        catalog("release-1", "claude-opus-5-preview", "claude-preview").ok_or("catalog")?;
    let live_target = issued(&reloaded, "anthropic", "claude-opus-5").ok_or("target")?;

    let decoded = decode(&payload)?;
    let redemption = decoded.summary().redeem_for(&live_target);
    assert!(
        redemption.artifact().is_some(),
        "a coordinate-matching live target must redeem the provider-bound \
         artifact after a restart; refusing here would make provider-bound \
         summaries unredeemable across exactly the event compaction exists to \
         survive"
    );
    assert!(!redemption.needs_neutral_recovery());
    Ok(())
}

#[test]
fn the_original_issuance_does_not_survive_the_restart() -> TestResult {
    let (payload, authoring_target) = authored_under_a_destroyed_catalog()?;
    let reloaded =
        catalog("release-1", "claude-opus-5-preview", "claude-preview").ok_or("catalog")?;
    let live_target = issued(&reloaded, "anthropic", "claude-opus-5").ok_or("target")?;

    // This is the half that must FAIL to survive. Both refs name the same
    // provider and model, and the reloaded catalog even copied the version
    // string; they are still different issuances, because issuance identity is
    // the process-local catalog snapshot and a reload mints a fresh one.
    assert!(
        !authoring_target.is_same_issuance(&live_target),
        "a reloaded catalog must mint a DIFFERENT issuance even for a repeated \
         version string; if this ever passes, the restart drill below proves \
         nothing, because there would be no issuance boundary to cross"
    );

    // And the redemption that succeeds across that boundary succeeds on
    // coordinates, not on the issuance that was just shown not to survive.
    let decoded = decode(&payload)?;
    assert!(decoded
        .summary()
        .redeem_for(&live_target)
        .artifact()
        .is_some());
    Ok(())
}

#[test]
fn an_audit_version_change_alone_still_redeems() -> TestResult {
    let (payload, _authoring_target) = authored_under_a_destroyed_catalog()?;

    // Same coordinates, a genuinely different catalog release. An ordinary
    // catalog upgrade must not strand a redeemable summary.
    let upgraded = catalog("release-2", "claude-opus-5", "claude").ok_or("catalog")?;
    let live_target = issued(&upgraded, "anthropic", "claude-opus-5").ok_or("target")?;

    let decoded = decode(&payload)?;
    assert!(
        decoded
            .summary()
            .redeem_for(&live_target)
            .artifact()
            .is_some(),
        "the recorded catalog version is audit provenance; a redemption rule \
         that consulted it would refuse this legitimate summary"
    );

    // The version is nonetheless CARRIED, or "recorded as audit provenance"
    // would be an empty claim: it survives durability and still reads as the
    // release that vouched for the coordinates, not as the live one.
    let compatibility = match decoded.summary() {
        CompactionSummary::ProviderBound { compatibility, .. } => compatibility,
        CompactionSummary::ClientReadable { .. } => return Err("expected a bound summary".into()),
    };
    assert_eq!(compatibility.catalog_version().as_str(), "release-1");
    assert_eq!(live_target.catalog_version().as_str(), "release-2");
    Ok(())
}

#[test]
fn a_foreign_target_is_refused_with_its_neutral_recovery_after_a_restart() -> TestResult {
    let (payload, _authoring_target) = authored_under_a_destroyed_catalog()?;
    let reloaded = catalog("release-1", "claude-opus-5", "claude").ok_or("catalog")?;
    let foreign = issued(&reloaded, "openai", "gpt-x").ok_or("target")?;

    let decoded = decode(&payload)?;
    let refused = decoded.summary().redeem_for(&foreign);
    assert_eq!(refused.artifact(), None, "a refusal must not yield content");
    assert!(refused.needs_neutral_recovery());
    assert_eq!(
        refused.failure(),
        Some(ContextFailure::CompactionIncompatibleSummary),
        "the refusal must carry the typed outcome a restarted owner settles"
    );

    // The refusal reads as compatibility DATA on both sides after a restart,
    // exactly as it did before one.
    let incompatibility = refused.incompatibility().ok_or("incompatibility")?;
    assert_eq!(incompatibility.bound().model().as_str(), "claude-opus-5");
    assert_eq!(incompatibility.requested().model().as_str(), "gpt-x");
    Ok(())
}

#[test]
fn the_whole_descriptor_survives_the_restart_unchanged() -> TestResult {
    // Redemption is the interesting half, but a restart that silently dropped
    // any other coordinate would be the same defect one field over. Total
    // equality across the boundary, not field-spotting.
    let original_catalog = catalog("release-1", "claude-opus-5", "claude").ok_or("catalog")?;
    let target = issued(&original_catalog, "anthropic", "claude-opus-5").ok_or("target")?;
    let authored = bound_descriptor(&target).ok_or("descriptor")?;
    let payload = encode(&authored);
    drop(original_catalog);

    assert_eq!(decode(&payload)?, authored);
    Ok(())
}
