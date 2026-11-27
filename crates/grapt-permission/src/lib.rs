//! Permission blocker domain. The blocker machine lives in `grapt-types`;
//! this crate owns the permission event vocabulary, strict decode, and the
//! lifecycle fold.

pub mod command_safety;
pub mod event;
pub mod fold;
mod policy;
pub mod prefix;
pub mod preset;
pub mod service;
mod tool_authority;

/// Narrow native filesystem amendments; remembered grants are literal paths
/// and never authorize the shell command that uses them.
pub const SANDBOX_WRITE_ACTION: &str = "sandbox.write";
/// Explicit per-executable permission to run an incompatible MSYS command
/// without OS containment. Ordinary command grants cannot authorize this.
pub const SANDBOX_MSYS_ACTION: &str = "sandbox.msys_unconfined";
/// Explicit consent for Windows shell command trees in this Session only.
pub const SANDBOX_MSYS_SESSION: &str = "windows-session-command-trees";

pub(crate) fn sandbox_amendment(action: &str) -> bool {
    matches!(action, SANDBOX_WRITE_ACTION | SANDBOX_MSYS_ACTION)
}

pub use event::{
    reply_as_str, CancelReason, PermissionAskedCoordinates, PermissionEvent, PermissionEventError,
};
// `PermissionReply` is owned by grapt-types; re-exported here only so existing
// callers keep one import path. There is no second definition.
pub use fold::{FoldOutcome, PermissionFold, RequestRecord};
pub use grapt_types::PermissionReply;
pub use service::{
    CascadeSettled, DecisionIds, OrdinaryReply, PermissionConsequence, PermissionCreateEffect,
    PermissionError, PermissionHttpError, PermissionHttpLifecycle, PermissionHttpPage,
    PermissionHttpReply, PermissionHttpRequest, PermissionHttpSnapshot, PermissionHttpSource,
    PermissionHttpState, PermissionOwner, PermissionReplyCommand, PermissionReplyEffect,
    PermissionRequest, PermissionRevokeRuleCommand, PermissionRuleWithdrawal, PermissionService,
    ReplyOutcome, SavedPermissionRule, SessionPermissionRule,
};
pub use tool_authority::{
    CommittedPermissionCancellation, CommittedPermissionReply, PendingToolEffect,
    ToolAuthorizationError, ToolPermissionAuthority, MAX_PENDING_TOOL_PERMISSIONS,
};
