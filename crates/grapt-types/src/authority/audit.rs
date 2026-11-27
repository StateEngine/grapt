//! Durable audit truth: the authority vocabulary and the settlement
//! receipt.
//!
//! Everything in this module decodes from stored bytes, is `Clone` and
//! `PartialEq`, and authorizes nothing. Its invariant is completeness of
//! attribution, and the refusal law that a refusal carries denial provenance
//! exactly when it is a refusal.
//!
//! It depends on crate leaves only. In particular it does NOT depend on the
//! deterministic clock seam: a settlement carries no `Timestamp`, because the
//! answering instant exists only on the live types. That is the positive form
//! of "replay is decoding, not authentication" — the durable receipt has no
//! clock coordinate at all.

use crate::capability::{CapabilityId, GrantScope, RealizedEnforcement};
use crate::credential::AccountId;
use crate::hook::HookId;
use crate::model::AgentId;
use crate::permission::{PermissionId, PermissionReply};
use crate::subagent::EffectiveGraphId;
use crate::transport::{AuthenticatedClientId, DecisionNonce, RequestId};

crate::id::opaque_id! {
    /// Version identity of the policy that produced a decision.
    PolicyVersion
}

crate::id::opaque_id! {
    /// The name of a configured policy layer.
    ///
    /// A layer name is a *coordinate inside* [`AuthoritySource::PolicyLayer`],
    /// never the whole authority vocabulary.
    LayerName
}

/// Who exercised authority.
///
/// Closed (law C3). A remotely supplied prompt cannot silently become an
/// authority policy, and an automated reviewer is not a human.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuthorityPrincipal {
    /// A human, identified by the account that authenticated them.
    Human { account: AccountId },
    /// The harness itself, acting under versioned system policy.
    System,
    /// A configured policy layer.
    PolicyLayer { layer: LayerName },
    /// A hook, bound to its stable identity rather than its list position.
    Hook { hook: HookId },
    /// A model acting as an automated reviewer. Distinct from `Human`: a model
    /// claim is never human ratification.
    AutomatedReviewer { agent: AgentId },
}

impl AuthorityPrincipal {
    /// Whether this principal can supply human ratification.
    ///
    /// Law: model claims do not become human ratification, so this is the one
    /// place the distinction is decided.
    pub fn is_human(&self) -> bool {
        matches!(self, AuthorityPrincipal::Human { .. })
    }
}

/// Where a decision came from.
///
/// Supersedes `{Layer, Fallback}`: a default-policy outcome names the exact
/// policy version that produced it, so a fallback can never be read back as a
/// layer name or as an unexplained default.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuthoritySource {
    /// Answered live by a principal.
    DirectReply,
    /// Decided by a configured policy layer.
    PolicyLayer { layer: LayerName },
    /// Decided by a previously persisted rule.
    SavedRule { policy: PolicyVersion },
    /// Decided by a hook.
    Hook { hook: HookId },
    /// Decided by the versioned default policy because nothing more specific
    /// applied. This is the corrected replacement for a bare `Fallback`.
    DefaultPolicy { policy: PolicyVersion },
}

/// Whether a refusal was reported by the answering principal or inferred.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DenialProvenance {
    /// The principal explicitly refused.
    ReportedByPrincipal,
    /// The harness refused under a named policy version without asking.
    InferredFromPolicy { policy: PolicyVersion },
}

/// The `permission.replied` v2 settlement receipt.
///
/// Joined to the request by [`PermissionId`]. It records the exact capability,
/// decision, authority principal and source, granted scope, policy and
/// effective-graph versions, realized enforcement, denial provenance and the
/// transport-authenticated answering client, reply request and decision nonce.
///
/// Construction is validating: a refusal must carry denial provenance and a
/// grant must not, so "why was this denied" can never be absent from a real
/// denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSettlement {
    permission: PermissionId,
    decision: PermissionReply,
    principal: AuthorityPrincipal,
    source: AuthoritySource,
    capability: CapabilityId,
    scope: GrantScope,
    policy: PolicyVersion,
    effective_graph: EffectiveGraphId,
    enforcement: RealizedEnforcement,
    denial: Option<DenialProvenance>,
    client: AuthenticatedClientId,
    request: RequestId,
    nonce: DecisionNonce,
}

/// Everything a settlement needs, so the validating constructor does not take
/// a dozen positional arguments that are easy to transpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSettlementParts {
    pub permission: PermissionId,
    pub decision: PermissionReply,
    pub principal: AuthorityPrincipal,
    pub source: AuthoritySource,
    pub capability: CapabilityId,
    pub scope: GrantScope,
    pub policy: PolicyVersion,
    pub effective_graph: EffectiveGraphId,
    pub enforcement: RealizedEnforcement,
    pub denial: Option<DenialProvenance>,
    pub client: AuthenticatedClientId,
    pub request: RequestId,
    pub nonce: DecisionNonce,
}

impl PermissionSettlement {
    /// Build a settlement, refusing the two states the law says cannot exist:
    /// a refusal without denial provenance, and a grant that carries one.
    pub fn new(parts: PermissionSettlementParts) -> Option<Self> {
        let refused = !parts.decision.is_grant();
        if refused != parts.denial.is_some() {
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
            denial: parts.denial,
            client: parts.client,
            request: parts.request,
            nonce: parts.nonce,
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

    pub fn denial(&self) -> Option<&DenialProvenance> {
        self.denial.as_ref()
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

    // NOTE: there is deliberately no `session_grant` here any more.
    //
    // The earlier `PermissionSettlement::session_grant(&self, session)` was a
    // live-authority bypass with two independent defects. First, every input to
    // `PermissionSettlement::new` is publicly authorable — `PermissionId`
    // parses from text, and `AuthenticatedClientId`, `RequestId` and
    // `DecisionNonce` are opaque ids with public constructors — so any caller
    // could assemble a settlement whose decision was `Session` and mint real
    // process-local authority from data it made up. Second, the `session`
    // argument was chosen by the caller and never compared with the settlement's
    // own `GrantScope::Session(..)`, so even a genuine settlement for one
    // Session could mint a grant for another.
    //
    // Both follow from conflating two different things, which amendment §8.2
    // keeps apart: this type is the DURABLE AUDIT RECEIPT (`permission.replied`
    // v2), and the store must be able to rebuild one by decoding stored bytes
    // during replay. "A recorded Session-scope approval is audit truth, not
    // automatically reconstructible authority." Live authority now comes only
    // from [`LivePermissionDecision`], which a decoded row cannot produce.

    /// Whether `other` is the exact same answer and may therefore reconcile as
    /// a duplicate rather than refusing.
    ///
    /// Law: the first valid answer wins; an exact duplicate reconciles, a
    /// conflicting duplicate refuses.
    pub fn reconciles_with(&self, other: &PermissionSettlement) -> bool {
        self == other
    }
}

/// A stored permission settlement, including imported v1 history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionSettlementRecord {
    /// A v2 settlement with a complete attribution tuple.
    ///
    /// Boxed: the attributed tuple is an order of magnitude larger than a
    /// legacy row, and every stored record would otherwise pay for it.
    Attributed(Box<PermissionSettlement>),
    /// An imported v1 settlement. It has no answering client, so it is audit
    /// truth only and can never rebuild a live grant.
    LegacyUnattributed {
        permission: PermissionId,
        decision: PermissionReply,
    },
}

impl PermissionSettlementRecord {
    /// The fully attributed v2 settlement, if this is one.
    ///
    /// Renamed from `live_authority`, which asserted something no longer true —
    /// and was false in the same way at both ends. A v1 row cannot reconstruct
    /// live authority because it has no answering client; a v2 row cannot either,
    /// because NO decoded settlement can. Replay is decoding, not authentication
    /// (amendment 8.2), and the only origin of live authority is
    /// [`LivePermissionDecision`](crate::authority::LivePermissionDecision), which has no public mint.
    ///
    /// What the variants actually distinguish is ATTRIBUTION: whether the record
    /// names the principal, client, request and nonce that answered. `None` here
    /// means the imported v1 row does not.
    pub fn attributed_settlement(&self) -> Option<&PermissionSettlement> {
        match self {
            PermissionSettlementRecord::Attributed(settlement) => Some(settlement.as_ref()),
            PermissionSettlementRecord::LegacyUnattributed { .. } => None,
        }
    }
}
