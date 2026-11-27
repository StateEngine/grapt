//! Permission identity, reply vocabulary and suspension reason.
//!
//! Law (`CORRECT`): a suspended Permission has no reply and no live waiter,
//! and a direct reply to suspended work fails `NotLive` rather than
//! manufacturing a settlement. The oracle's nullable `suspended_reason` column
//! and its `?? "process-loss"` fallback in `core/src/permission.ts` are the
//! defect this vocabulary removes: absence of a reason is unrepresentable.

crate::id::prefixed_id! {
    /// Validated `per_*` permission identity.
    PermissionId, "per"
}

/// How a principal answered a permission request.
///
/// Law: these variants do **not** share one generic "save" branch.
/// [`PermissionReply::Session`] produces a process-local
/// [`crate::capability::SessionGrant`] with no durable codec, while
/// [`PermissionReply::Always`] is a separately typed project-rule
/// transaction. Collapsing them is how a process-local grant becomes a
/// resurrected durable authority after restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionReply {
    /// Authorize exactly this effect, once.
    Once,
    /// Authorize for the remainder of this Session, process-locally.
    Session,
    /// Persist a project rule built by the authority from the resolved
    /// capability.
    Always,
    /// Refuse.
    Reject,
}

impl PermissionReply {
    /// Whether the reply authorizes the requested effect at all.
    pub fn is_grant(self) -> bool {
        !matches!(self, PermissionReply::Reject)
    }

    /// Whether the reply creates authority that outlives the single effect.
    pub fn is_durable_rule(self) -> bool {
        matches!(self, PermissionReply::Always)
    }
}

/// Why a Permission is suspended.
///
/// A shutdown or process loss is **not** a user decline and **not** a user
/// cancel; those are separate terminal facts. This type is deliberately
/// distinct from [`crate::form::FormSuspensionReason`] so the two blockers
/// cannot be settled through one another's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionSuspensionReason {
    /// The owning runtime shut down cleanly.
    Shutdown,
    /// Startup reconciliation found a dead owning generation.
    ProcessLoss,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_always_are_distinct_authorities() {
        assert!(PermissionReply::Always.is_durable_rule());
        assert!(
            !PermissionReply::Session.is_durable_rule(),
            "a Session reply is process-local and must never persist a rule"
        );
        assert_ne!(PermissionReply::Session, PermissionReply::Always);
    }

    #[test]
    fn reject_is_not_a_grant() {
        assert!(!PermissionReply::Reject.is_grant());
        assert!(PermissionReply::Once.is_grant());
    }

    #[test]
    fn suspension_reason_has_no_absent_state() {
        // The oracle's `suspended_reason ?? "process-loss"` fallback is
        // unrepresentable: every value of this type names a real reason, so a
        // shutdown can never be silently reported as process loss.
        let reasons = [
            PermissionSuspensionReason::Shutdown,
            PermissionSuspensionReason::ProcessLoss,
        ];
        assert_eq!(reasons.len(), 2);
        assert_ne!(reasons[0], reasons[1]);
    }
}
