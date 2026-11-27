//! Form identity and ownership. Law: `CORRECT` — replaces the oracle's
//! `Form.Info.sessionID: String` and its `"global"` sentinel
//! (`schema/src/form.ts`): a Form cannot claim a fake SessionId, and global
//! ownership is a real variant, not a magic string.

use crate::session::SessionId;

crate::id::prefixed_id! {
    /// Validated `frm_*` form identity.
    FormId, "frm"
}

crate::id::opaque_id! {
    /// Opaque MCP elicitation identity (owner token for global forms).
    McpElicitationId
}

/// Who owns a form. The `"global"` string sentinel is unrepresentable here —
/// endorsed by the red-team review as a genuine correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormOwner {
    Session(SessionId),
    Global(McpElicitationId),
}

/// A validated Form answer.
///
/// Law: `Issued` and `Settled` carry different payloads, and a settled Form
/// carries its answer. An empty answer is not an answer, so validation happens
/// in the constructor rather than at every read site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormAnswer(String);

impl FormAnswer {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            return None;
        }
        Some(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a Form is suspended.
///
/// Deliberately a distinct type from [`crate::permission::PermissionSuspensionReason`]:
/// the two blockers share the lifecycle law but not their terminal payloads,
/// and Form suspension is non-terminal while a suspended Permission cannot be
/// settled by a direct reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormSuspensionReason {
    /// The owning runtime shut down cleanly.
    Shutdown,
    /// Startup reconciliation found a dead owning generation.
    ProcessLoss,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_answer_is_not_an_answer() {
        assert!(FormAnswer::new("").is_none());
        assert!(FormAnswer::new("yes").is_some());
    }

    #[test]
    fn owner_is_session_or_global_never_a_sentinel_string() -> Result<(), Box<dyn std::error::Error>>
    {
        let ses: SessionId = "ses_0a1b".parse()?;
        let owner = FormOwner::Session(ses);
        assert!(matches!(owner, FormOwner::Session(_)));
        let global = FormOwner::Global(McpElicitationId::new("elic-1").ok_or("elicitation id")?);
        assert!(matches!(global, FormOwner::Global(_)));
        // A bare "global" cannot become an owner: McpElicitationId is opaque
        // but non-empty, and FormOwner has no string constructor at all.
        Ok(())
    }
}
