//! Revisioned service observations shared by session reads and stream notices.
use super::prelude::*;

string_enum! { pub enum SessionStateKind {
    Idle => "idle",
    Running => "running",
    Waiting => "waiting",
    Paused => "paused",
    Failed => "failed",
    Interrupted => "interrupted",
    Disconnected => "disconnected",
    Completed => "completed",
} }

string_enum! { pub enum SessionStateReason {
    Preparing => "preparing",
    Compacting => "compacting",
    ProviderSlot => "provider-slot",
    ProviderRetry => "provider-retry",
    Permission => "permission",
    Question => "question",
    Compaction => "compaction",
    Capability => "capability",
    Continuity => "continuity",
    Execution => "execution",
    User => "user",
    Shutdown => "shutdown",
    Superseded => "superseded",
    WindowClosed => "window-closed",
    ProcessLoss => "process-loss",
} }

string_enum! { pub enum SessionStateAction {
    FollowUp => "follow-up",
    Compact => "compact",
    SignIn => "sign-in",
    RestoreQuota => "restore-quota",
    Rebind => "rebind",
    InspectConnection => "inspect-connection",
    InspectSession => "inspect-session",
    ReviewPermission => "review-permission",
    AnswerQuestion => "answer-question",
    Wait => "wait",
    ChangeModel => "change-model",
} }

closed_object! { pub struct SessionStateDto => "inline:session-state" {
    required kind: SessionStateKind => "type",
    required revision: NonNegativeU64 => "revision",
    optional execution_id: WireId<grapt_types::ExecutionId> => "executionID",
    optional reason: SessionStateReason => "reason",
    optional message: BoundedString<1024> => "message",
    optional action: SessionStateAction => "action",
} }

string_enum! { pub enum SessionStateErrorCode {
    Unreadable => "unreadable",
    OwnerChanged => "owner-changed",
    OwnerGone => "owner-gone",
    CommitUnavailable => "commit-unavailable",
    Encoding => "encoding",
} }

closed_object! { pub struct SessionStateErrorDto => "inline:session-state-error" {
    required code: SessionStateErrorCode => "code",
    required message: BoundedString<1024> => "message",
} }
