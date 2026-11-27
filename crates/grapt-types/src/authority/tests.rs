use super::determinism::{CountingIds, FixedClock};
use super::*;
use crate::capability::{CapabilityId, GrantScope, RealizedEnforcement};
use crate::capability::{EffectClass, EgressAuthority, FilesystemAuthority, ResolvedCapability};
use crate::credential::AccountId;
use crate::model::AgentId;
use crate::permission::PermissionReply;
use crate::subagent::EffectiveGraphId;
use crate::transport::{AuthenticatedClientId, DecisionNonce, RequestId};

fn capability() -> CapabilityId {
    CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::FilesystemRead,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ))
}

fn parts(
    decision: PermissionReply,
) -> Result<PermissionSettlementParts, Box<dyn std::error::Error>> {
    Ok(PermissionSettlementParts {
        permission: "per_1".parse()?,
        decision,
        principal: AuthorityPrincipal::Human {
            account: AccountId::new("acct-1").ok_or("account")?,
        },
        source: AuthoritySource::DirectReply,
        capability: capability(),
        scope: GrantScope::Once,
        policy: PolicyVersion::new("policy-1").ok_or("policy")?,
        effective_graph: EffectiveGraphId::new("graph-1").ok_or("graph")?,
        enforcement: RealizedEnforcement::HarnessOnly,
        denial: None,
        client: AuthenticatedClientId::new("client-1").ok_or("client")?,
        request: RequestId::new("req-1").ok_or("request")?,
        nonce: DecisionNonce::new("nonce-1").ok_or("nonce")?,
    })
}

#[test]
fn refusal_without_provenance_is_unbuildable() -> Result<(), Box<dyn std::error::Error>> {
    let mut refused = parts(PermissionReply::Reject)?;
    refused.denial = None;
    assert!(
        PermissionSettlement::new(refused).is_none(),
        "a denial must say where the refusal came from"
    );
    Ok(())
}

#[test]
fn grant_carrying_denial_provenance_is_unbuildable() -> Result<(), Box<dyn std::error::Error>> {
    let mut granted = parts(PermissionReply::Once)?;
    granted.denial = Some(DenialProvenance::ReportedByPrincipal);
    assert!(PermissionSettlement::new(granted).is_none());
    Ok(())
}

#[test]
fn well_formed_grant_and_refusal_both_build() -> Result<(), Box<dyn std::error::Error>> {
    assert!(PermissionSettlement::new(parts(PermissionReply::Once)?).is_some());

    let mut refused = parts(PermissionReply::Reject)?;
    refused.denial = Some(DenialProvenance::ReportedByPrincipal);
    assert!(PermissionSettlement::new(refused).is_some());
    Ok(())
}

#[test]
fn exact_duplicate_reconciles_and_a_conflict_does_not() -> Result<(), Box<dyn std::error::Error>> {
    let first =
        PermissionSettlement::new(parts(PermissionReply::Once)?).ok_or("first settlement")?;
    let duplicate = PermissionSettlement::new(parts(PermissionReply::Once)?).ok_or("duplicate")?;
    assert!(first.reconciles_with(&duplicate));

    let conflicting = PermissionSettlement::new(parts(PermissionReply::Session)?)
        .ok_or("conflicting settlement")?;
    assert!(
        !first.reconciles_with(&conflicting),
        "a different decision under the same tuple must refuse, not reconcile"
    );
    Ok(())
}

#[test]
fn a_stored_settlement_records_attribution_and_never_live_authority(
) -> Result<(), Box<dyn std::error::Error>> {
    let legacy = PermissionSettlementRecord::LegacyUnattributed {
        permission: "per_legacy".parse()?,
        decision: PermissionReply::Always,
    };
    assert!(
        legacy.attributed_settlement().is_none(),
        "an unattributed v1 receipt is audit truth only"
    );

    let attributed = PermissionSettlementRecord::Attributed(Box::new(
        PermissionSettlement::new(parts(PermissionReply::Once)?).ok_or("settlement")?,
    ));
    assert!(attributed.attributed_settlement().is_some());
    Ok(())
}

#[test]
fn default_policy_is_not_a_layer_name() -> Result<(), Box<dyn std::error::Error>> {
    let fallback = AuthoritySource::DefaultPolicy {
        policy: PolicyVersion::new("policy-1").ok_or("policy")?,
    };
    let layer = AuthoritySource::PolicyLayer {
        layer: LayerName::new("policy-1").ok_or("layer")?,
    };
    assert_ne!(
        fallback, layer,
        "a default-policy decision must not be readable as a configured layer"
    );
    Ok(())
}

#[test]
fn a_model_reviewer_is_not_a_human() -> Result<(), Box<dyn std::error::Error>> {
    let reviewer = AuthorityPrincipal::AutomatedReviewer {
        agent: AgentId::new("agent-1").ok_or("agent")?,
    };
    assert!(!reviewer.is_human());
    assert!(AuthorityPrincipal::Human {
        account: AccountId::new("acct-1").ok_or("account")?,
    }
    .is_human());
    Ok(())
}

#[test]
fn id_token_is_inert() -> Result<(), Box<dyn std::error::Error>> {
    // A caller can build a token. That is deliberate: external adapters
    // implement IdAuthority. What must NOT exist is a route from this
    // value into durable or live authority.
    let forged = IdToken::new("forged").ok_or("token")?;
    assert_eq!(forged.as_str(), "forged");

    // No authority constructor in this crate accepts an IdToken. The
    // sealed routes each demand their own proof instead:
    //   - SessionGrant  <- LivePermissionDecision (no public mint)
    //   - AuthorityLease <- CapabilityGrant::lease
    // A token cannot be substituted for any of them, which the compiler
    // enforces by type. Empty tokens are refused outright.
    assert!(IdToken::new("").is_none());
    Ok(())
}

#[test]
fn injected_authorities_are_deterministic() -> Result<(), Box<dyn std::error::Error>> {
    let clock = FixedClock(1_700_000_000_000);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);

    assert_eq!(authorities.now(), Timestamp::from_millis(1_700_000_000_000));
    assert_eq!(authorities.now(), Timestamp::from_millis(1_700_000_000_000));
    assert_eq!(authorities.mint().as_str(), "id-1");
    assert_eq!(authorities.mint().as_str(), "id-2");
    Ok(())
}

/// W8 — the live-to-audit projection is total and coordinate-preserving.
///
/// `audit_receipt` copies thirteen coordinates by hand. Asserted PER FIELD,
/// never as one whole-value comparison: a whole-receipt comparison against a
/// whole expectation passes while any two same-typed coordinates are
/// transposed.
#[test]
fn a_live_decision_writes_itself_down_without_losing_a_coordinate(
) -> Result<(), Box<dyn std::error::Error>> {
    let decision =
        test_support::live_decision(PermissionReply::Once, capability(), GrantScope::Once)
            .ok_or("live decision")?;
    let receipt = decision.audit_receipt(None).ok_or("audit receipt")?;

    assert_eq!(receipt.permission(), decision.permission());
    assert_eq!(receipt.decision(), decision.decision());
    assert_eq!(receipt.principal(), decision.principal());
    assert_eq!(receipt.source(), decision.source());
    assert_eq!(receipt.capability(), decision.capability());
    assert_eq!(receipt.scope(), decision.scope());
    assert_eq!(receipt.policy(), decision.policy());
    assert_eq!(receipt.effective_graph(), decision.effective_graph());
    assert_eq!(receipt.enforcement(), decision.enforcement());
    assert_eq!(receipt.client(), decision.client());
    assert_eq!(receipt.request(), decision.request());
    assert_eq!(receipt.nonce(), decision.nonce());
    assert!(
        receipt.denial().is_none(),
        "a grant's receipt carries no denial provenance"
    );
    Ok(())
}

/// W9 — the projection carries its denial law, and it is one-way.
///
/// Two halves, both about the seam rather than a field.
#[test]
fn an_audit_receipt_carries_its_denial_and_never_returns_live_authority(
) -> Result<(), Box<dyn std::error::Error>> {
    let decision =
        test_support::live_decision(PermissionReply::Once, capability(), GrantScope::Once)
            .ok_or("live decision")?;

    // Half one: the grant/denial law of `PermissionSettlement::new` is reached
    // THROUGH the projection, not only through the constructor. A grant that is
    // written down with denial provenance is unbuildable, so `audit_receipt`
    // must refuse rather than emit a receipt that contradicts its own decision.
    assert!(
        decision
            .audit_receipt(Some(DenialProvenance::ReportedByPrincipal))
            .is_none(),
        "a grant projected with denial provenance must refuse"
    );

    // Half two: the projection is one-way, asserted as a documented absence
    // because the code that would violate it does not compile — the same
    // standing `id_token_is_inert` takes. A stored record exposes its
    // settlement and nothing else; no accessor, constructor or conversion on
    // `PermissionSettlement` or `PermissionSettlementRecord` yields a
    // `LivePermissionDecision`, and the compiler enforces that by type.
    let receipt = decision.audit_receipt(None).ok_or("audit receipt")?;
    let record = PermissionSettlementRecord::Attributed(Box::new(receipt));
    let settlement = record.attributed_settlement().ok_or("attributed")?;
    assert_eq!(settlement.decision(), decision.decision());
    assert!(settlement.denial().is_none());
    Ok(())
}
