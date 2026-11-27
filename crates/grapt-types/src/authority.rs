//! Authority principals, decision sources, the permission settlement receipt
//! and the injected clock/ID seams.
//!
//! Law (`CORRECT`, survey rows C2/C3 and amendment §8.2): the two-variant
//! `AuthoritySource::{Layer, Fallback}` proposal is superseded. Human, system,
//! policy layer, hook and future reviewer principals stay typed and can never
//! be inferred from prose or parsed out of a layer name.
//!
//! `permission.replied` v2 is the settlement receipt; no redundant third fact
//! is added. Imported v1 settlements carry no answering client, so they decode
//! as [`PermissionSettlementRecord::LegacyUnattributed`] audit truth and can
//! never reconstruct live authority after a restart.
//!
//! # Topology
//!
//! This module is a FACADE. Three private owners carry the three lifecycles the
//! vocabulary actually has, and the dependency direction between them is the
//! architecture:
//!
//! - `audit` — durable truth. Decodable, `Clone`, `PartialEq`; holding one
//!   authorizes nothing. Depends on crate leaves only, and deliberately not on
//!   the clock seam: a settlement has no time coordinate at all.
//! - `live` — unforgeable authority. Not decodable, not `Clone`, not
//!   `Serialize`, redacted `Debug`, `pub(crate)` mint, clock-stamped. Depends
//!   inward on `audit` and on `determinism`.
//! - `determinism` — the injected clock and identity seams. Zero
//!   crate-internal dependencies; it is not about permission at all.
//!
//! All three are PRIVATE. The public surface is exactly the `pub use` list
//! below, so `grapt_types::authority::live::LivePermissionDecision` is not
//! nameable from outside this crate and every existing
//! `grapt_types::authority::X` path keeps resolving unchanged.

mod audit;
mod determinism;
mod live;

pub use audit::{
    AuthorityPrincipal, AuthoritySource, DenialProvenance, LayerName, PermissionSettlement,
    PermissionSettlementParts, PermissionSettlementRecord, PolicyVersion,
};
pub use determinism::{ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, Timestamp};
pub use live::LivePermissionDecision;

/// In-crate test support for the sealed live-authority mints.
///
/// `#[cfg(test)]` is the point: this compiles only into this crate's own unit
/// test binary. It is not a feature and not `doc(hidden)` — both of those would
/// be reachable by a dependent crate (features are additive, so any dependent
/// can turn one on), and a reachable test hook is just a second public route to
/// authority wearing a different name. This one fails closed.
#[cfg(test)]
pub(crate) mod test_support {
    use super::live::LivePermissionDecisionParts;
    use super::{
        AuthorityPrincipal, AuthoritySource, DeterministicAuthorities, LivePermissionDecision,
        PolicyVersion,
    };
    use crate::capability::{CapabilityId, GrantScope, RealizedEnforcement};
    use crate::credential::AccountId;
    use crate::permission::PermissionReply;
    use crate::subagent::EffectiveGraphId;
    use crate::transport::{AuthenticatedClientId, DecisionNonce, RequestId};

    // The deterministic fakes live in `determinism` because `FixedIds` touches
    // `IdToken`'s private tuple field, and only a descendant of the module that
    // owns `IdToken` may do so. Re-exported here so every existing
    // `crate::authority::test_support::…` consumer keeps resolving unchanged.
    pub(crate) use super::determinism::{FixedClock, FixedIds};

    /// Mint a live permission decision for a test.
    ///
    /// Deliberately routed through the real [`LivePermissionDecision::mint`]
    /// so tests exercise the same coherence refusals production would.
    pub(crate) fn live_decision(
        decision: PermissionReply,
        capability: CapabilityId,
        scope: GrantScope,
    ) -> Option<LivePermissionDecision> {
        let clock = FixedClock(1_700_000_000_000);
        let ids = FixedIds;
        let authorities = DeterministicAuthorities::new(&clock, &ids);
        LivePermissionDecision::mint(
            LivePermissionDecisionParts {
                permission: "per_test".parse().ok()?,
                decision,
                principal: AuthorityPrincipal::Human {
                    account: AccountId::new("acct-1")?,
                },
                source: AuthoritySource::DirectReply,
                capability,
                scope,
                policy: PolicyVersion::new("policy-1")?,
                effective_graph: EffectiveGraphId::new("graph-1")?,
                enforcement: RealizedEnforcement::HarnessOnly,
                client: AuthenticatedClientId::new("client-1")?,
                request: RequestId::new("req-1")?,
                nonce: DecisionNonce::new("nonce-1")?,
            },
            &authorities,
        )
    }
}

#[cfg(test)]
mod tests;
