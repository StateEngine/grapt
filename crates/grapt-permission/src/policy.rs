//! Transitional, NON-AUTHORITY approval record.
//!
//! This module deliberately does NOT own a `SessionGrant`. The route it used to
//! name — mint from a `PermissionSettlement` whose decision granted Session
//! scope — NO LONGER EXISTS: that settlement is the durable audit receipt, every
//! input to it is publicly authorable, and replay is decoding rather than
//! authentication, so `PermissionSettlement::session_grant` was removed. The
//! only origin of a `SessionGrant` is now `grapt_types`' crate-private
//! `LivePermissionDecision`.
//!
//! Either way this crate could not produce one, and the underlying reason is
//! unchanged: a permission row names an action STRING and knows nothing about
//! the program, argv, cwd, environment, filesystem roots, egress or realized
//! enforcement a resolved capability is derived from.
//!
//! So the object below is named for what it actually is: an
//! [`UnresolvedApproval`]. It records that a Session reply covered an action.
//! It is NOT authority and cannot become authority:
//!
//! - it does not implement or produce `CapabilityGrant`, `SessionGrant` or
//!   `AuthorityLease`;
//! - it cannot authorize a governed effect;
//! - it preserves the existing control flow for compilation only.
//!
//! LATER-LANE DEPENDENCY: the tool/policy resolution owner must supply an
//! exact `ResolvedCapability` for an action before this can become a canonical
//! sealed grant. Until then the authority conversion is deliberately
//! impossible rather than approximated.
//!
//! Deliberately NOT a `grapt_types::CapabilityGrant`. A permission row names
//! an *action string*; it does not know the program, argv, cwd, environment,
//! filesystem roots, egress or realized enforcement that a resolved capability
//! is derived from. Manufacturing those coordinates in order to satisfy the
//! capability type would invent tool and policy semantics this crate has no
//! authority to invent, and would make an unresolved action look like resolved
//! authority. So the grant holds the action verbatim and is explicitly
//! unresolved until the owning lane lands.
//!
//! Compile-time guarantees: the type is not exported, derives no
//! `Clone`/`Serialize`, and has no public constructor — external code cannot
//! mint, copy, wire, or persist a `SessionGrant`.

/// The action a permission row names, before any capability resolution.
///
/// This is NOT authority. It cannot be compared against a
/// `grapt_types::CapabilityId` and cannot authorize an effect; it only lets
/// the interim service remember which action a Session reply covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnresolvedAction(String);

impl UnresolvedAction {
    pub(crate) fn new(action: &str) -> Option<Self> {
        if action.is_empty() {
            return None;
        }
        Some(Self(action.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub(crate) struct UnresolvedApproval {
    action: UnresolvedAction,
}

impl UnresolvedApproval {
    /// Private to this crate. Records an approval; grants nothing.
    pub(crate) fn record(action: UnresolvedAction) -> Self {
        Self { action }
    }

    /// The approved action. Deliberately returns the unresolved action and
    /// NOT a capability: there is no route from here to a governed effect.
    pub(crate) fn action(&self) -> &UnresolvedAction {
        &self.action
    }
}
