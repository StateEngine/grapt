//! Live authenticated decisions.
//!
//! Everything in the sibling audit module is DURABLE AUDIT TRUTH: it decodes
//! from stored bytes, it is `Clone`, and holding one authorizes nothing.
//! Everything here is LIVE AUTHORITY: it cannot be decoded, cannot be cloned,
//! and is the only thing in this crate from which a `SessionGrant`, a
//! `CapabilityGrant` or an `AuthorityLease` can be minted.
//!
//! Amendment §8.2 draws exactly this line: "A recorded Session-scope approval
//! is audit truth, not automatically reconstructible authority. Private
//! `SessionGrant` remains process-local and is re-evaluated after process
//! loss." Imported v1 settlements "cannot reconstruct live authority" — and
//! neither can a v2 one, because replay is decoding, not authentication.
//!
//! HONEST BOUNDARY, stated rather than papered over. `grapt-types` has no
//! dependencies and therefore no authentication primitive: inside this crate
//! there is nothing that can tell "the real answering transport" apart from
//! "any caller in the same process". So the mint below is `pub(crate)` and F1A
//! ships NO public route to it at all. That is deliberate. A public
//! constructor here — under any name, including an `elect`-style trust root —
//! would be exactly the bypass this correction removes, because a caller could
//! reach it by assembling data it authored itself.
//!
//! LATER-LANE DEPENDENCY RECEIPT: the live route belongs to the
//! permission/policy application owner (`grapt-policy` plus the R3 permission
//! application service in commission-order step 4), which holds the
//! authenticated transport channel that issues `AuthenticatedClientId` and
//! validates the reply nonce against the request it actually sent. Until that
//! owner exists, live authority is unreachable rather than approximated.
//! Nothing in F1A depends on it: no crate in this workspace constructs a
//! grant, a lease or a settlement today.

use super::audit::{
    AuthorityPrincipal, AuthoritySource, DenialProvenance, PermissionSettlement,
    PermissionSettlementParts, PolicyVersion,
};
use super::determinism::{DeterministicAuthorities, Timestamp};
use crate::capability::{CapabilityId, GrantScope, RealizedEnforcement};
use crate::permission::{PermissionId, PermissionReply};
use crate::session::SessionId;
use crate::subagent::EffectiveGraphId;
use crate::transport::{AuthenticatedClientId, DecisionNonce, RequestId};
use core::fmt;

/// A live, authenticated permission answer.
///
/// This is the single origin of live authority in the crate. It is
/// deliberately:
///
/// - **not `Clone`** — one authenticated answer is one answer, and law §8.3
///   requires the lease it produces to be non-clone through effect completion;
/// - **not serializable and not `Display`** — it has no wire or durable codec,
///   so it cannot round-trip through a database and be replayed into authority;
/// - **redacted in `Debug`** — diagnostics can name it without exporting it;
/// - **privately constructed** — see the module note above.
///
/// It privately carries every coordinate the settlement receipt carries plus
/// the answering timestamp from the injected
/// [`ClockAuthority`](crate::authority::ClockAuthority), so a grant
/// minted from it cannot be paired with a coordinate the answerer never agreed
/// to. Nothing is passed alongside it at mint time; the mint reads what it
/// needs out of the decision itself.
pub struct LivePermissionDecision {
    permission: PermissionId,
    decision: PermissionReply,
    principal: AuthorityPrincipal,
    source: AuthoritySource,
    capability: CapabilityId,
    scope: GrantScope,
    policy: PolicyVersion,
    effective_graph: EffectiveGraphId,
    enforcement: RealizedEnforcement,
    client: AuthenticatedClientId,
    request: RequestId,
    nonce: DecisionNonce,
    answered_at: Timestamp,
}

/// The coordinates a live permission decision is minted from.
///
/// Crate-private in its entirety: this is the shape the future permission/policy
/// owner will hand across the sealed boundary, not a public builder.
///
/// The `dead_code` allowance is scoped to `not(test)` on purpose. In a test
/// build the seam must be genuinely exercised or the warning fires for real; in
/// a plain library build it has no in-crate caller yet, because F1A ships the
/// sealed boundary and the permission/policy owner that will call it is a named
/// later lane. A blanket `allow` would hide a genuinely unused item in both
/// configurations, which is what the "no broad lint suppression" law forbids.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct LivePermissionDecisionParts {
    pub(crate) permission: PermissionId,
    pub(crate) decision: PermissionReply,
    pub(crate) principal: AuthorityPrincipal,
    pub(crate) source: AuthoritySource,
    pub(crate) capability: CapabilityId,
    pub(crate) scope: GrantScope,
    pub(crate) policy: PolicyVersion,
    pub(crate) effective_graph: EffectiveGraphId,
    pub(crate) enforcement: RealizedEnforcement,
    pub(crate) client: AuthenticatedClientId,
    pub(crate) request: RequestId,
    pub(crate) nonce: DecisionNonce,
}

impl LivePermissionDecision {
    /// Mint a live decision, stamping the answer time from the injected clock.
    ///
    /// Refuses the incoherent combinations rather than recording them:
    ///
    /// - a refusal never carries live authority, so `Reject` is refused here
    ///   (its audit trail is the settlement receipt, which does record it);
    /// - `PermissionReply::Session` must be answered at `GrantScope::Session`,
    ///   and no other reply may claim Session scope. This is the coherence the
    ///   removed `session_grant` never checked.
    ///
    /// The timestamp comes from [`DeterministicAuthorities`] rather than from a
    /// caller-supplied [`Timestamp`], so an answer cannot be backdated and
    /// replay stays deterministic (law H2, ownership probe #11).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mint(
        parts: LivePermissionDecisionParts,
        authorities: &DeterministicAuthorities<'_>,
    ) -> Option<Self> {
        if !parts.decision.is_grant() {
            return None;
        }
        let session_scoped = matches!(parts.scope, GrantScope::Session(_));
        if (parts.decision == PermissionReply::Session) != session_scoped {
            return None;
        }
        Some(Self {
            permission: parts.permission,
            decision: parts.decision,
            principal: parts.principal,
            source: parts.source,
            capability: parts.capability,
            scope: parts.scope,
            policy: parts.policy,
            effective_graph: parts.effective_graph,
            enforcement: parts.enforcement,
            client: parts.client,
            request: parts.request,
            nonce: parts.nonce,
            answered_at: authorities.now(),
        })
    }

    pub fn permission(&self) -> &PermissionId {
        &self.permission
    }

    pub fn decision(&self) -> PermissionReply {
        self.decision
    }

    pub fn principal(&self) -> &AuthorityPrincipal {
        &self.principal
    }

    pub fn source(&self) -> &AuthoritySource {
        &self.source
    }

    pub fn capability(&self) -> &CapabilityId {
        &self.capability
    }

    pub fn scope(&self) -> &GrantScope {
        &self.scope
    }

    pub fn policy(&self) -> &PolicyVersion {
        &self.policy
    }

    pub fn effective_graph(&self) -> &EffectiveGraphId {
        &self.effective_graph
    }

    pub fn enforcement(&self) -> &RealizedEnforcement {
        &self.enforcement
    }

    pub fn client(&self) -> &AuthenticatedClientId {
        &self.client
    }

    pub fn request(&self) -> &RequestId {
        &self.request
    }

    pub fn nonce(&self) -> &DecisionNonce {
        &self.nonce
    }

    pub fn answered_at(&self) -> Timestamp {
        self.answered_at
    }

    /// The Session this decision was answered for, if it is Session-scoped.
    ///
    /// Read from the decision's own scope. A caller never names the Session,
    /// which is what stops a genuine grant for one Session becoming authority
    /// over another.
    pub(crate) fn session(&self) -> Option<&SessionId> {
        match &self.scope {
            GrantScope::Session(session) => Some(session),
            _ => None,
        }
    }

    /// The audit receipt for this live decision.
    ///
    /// One-way on purpose: a live decision can always be written down, but a
    /// written-down decision can never be read back as live authority.
    pub fn audit_receipt(&self, denial: Option<DenialProvenance>) -> Option<PermissionSettlement> {
        PermissionSettlement::new(PermissionSettlementParts {
            permission: self.permission.clone(),
            decision: self.decision,
            principal: self.principal.clone(),
            source: self.source.clone(),
            capability: self.capability.clone(),
            scope: self.scope.clone(),
            policy: self.policy.clone(),
            effective_graph: self.effective_graph.clone(),
            enforcement: self.enforcement.clone(),
            denial,
            client: self.client.clone(),
            request: self.request.clone(),
            nonce: self.nonce.clone(),
        })
    }
}

impl fmt::Debug for LivePermissionDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LivePermissionDecision(<live, process-local>)")
    }
}
