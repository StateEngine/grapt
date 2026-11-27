// Probe 30: a durable audit settlement can never recreate live authority.
//
// This is the attacker's shortest path as it existed before the correction, and
// every step of it up to the last line still compiles — which is the point.
// `PermissionId` parses from text; `AccountId`, `PolicyVersion`,
// `EffectiveGraphId`, `AuthenticatedClientId`, `RequestId` and `DecisionNonce`
// are opaque ids with public constructors; `CapabilityId::derive` is public;
// and `PermissionSettlementParts` has public fields. So a caller could assemble
// a settlement out of values it invented, declare the decision `Session`, and
// call `session_grant` to mint real process-local authority.
//
// Amendment §8.2 says a recorded Session approval "is audit truth, not
// automatically reconstructible authority". The method is gone: this type is
// the durable receipt the store decodes on replay, and decoding is not
// authentication.
use grapt_types::{
    AccountId, AuthenticatedClientId, AuthorityPrincipal, AuthoritySource, CapabilityId,
    DecisionNonce, EffectClass, EffectiveGraphId, EgressAuthority, FilesystemAuthority, GrantScope,
    PermissionReply, PermissionSettlement, PermissionSettlementParts, PolicyVersion,
    RealizedEnforcement, RequestId, ResolvedCapability, SessionId,
};

// Every coordinate below is authored by the caller. None of it is refused.
fn forged_settlement(session: SessionId) -> Option<PermissionSettlement> {
    let capability = CapabilityId::derive(&ResolvedCapability::new(
        EffectClass::FilesystemWrite,
        None,
        FilesystemAuthority::default(),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    ));
    PermissionSettlement::new(PermissionSettlementParts {
        permission: "per_invented".parse().ok()?,
        decision: PermissionReply::Session,
        principal: AuthorityPrincipal::Human {
            account: AccountId::new("acct-invented")?,
        },
        source: AuthoritySource::DirectReply,
        capability,
        scope: GrantScope::Session(session),
        policy: PolicyVersion::new("policy-invented")?,
        effective_graph: EffectiveGraphId::new("graph-invented")?,
        enforcement: RealizedEnforcement::HarnessOnly,
        denial: None,
        client: AuthenticatedClientId::new("client-invented")?,
        request: RequestId::new("req-invented")?,
        nonce: DecisionNonce::new("nonce-invented")?,
    })
}

fn main() {
    let session: SessionId = "ses_victim".parse().expect("session");
    let settlement = forged_settlement(session.clone()).expect("settlement");

    // The one step that must not exist.
    let _authority = settlement.session_grant(session);
}
