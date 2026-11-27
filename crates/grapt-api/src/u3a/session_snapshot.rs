//! One coherent session snapshot: history rows, permissions, forms and state
//! read from a single store snapshot at revision P, plus the open live text
//! every subscriber has been offered. An opt-in route beside the unchanged
//! messages route; older clients never call it.

use super::prelude::*;
use super::{
    FormInfoDto, PermissionRequestStateDto, SessionMessageInfoDto, SessionMessagesApplicationError,
    SessionMessagesResponseDtoCursor, SessionStateDto, SessionStateErrorDto, MAX_PERMISSION_ROWS,
};
use grapt_types::{MessageId, SessionId};

/// Live blocks one session can carry: the producer holds up to 16 finished
/// reasoning blocks uncommitted, beside one open text and one open reasoning
/// block. One value, shared with the stream's live-block registry.
pub use grapt_stream::provisional::MAX_PROVISIONAL_BLOCKS;

string_enum! { pub enum SessionSnapshotProvisionalKindDto {
    Text => "text",
    Reasoning => "reasoning",
} }

closed_object! { pub struct SessionSnapshotProvisionalDto => "inline:session-snapshot-provisional" {
    required assistant_message_id: WireId<MessageId> => "assistantMessageID",
    required kind: SessionSnapshotProvisionalKindDto => "kind",
    required ordinal: NonNegativeU64 => "ordinal",
    required text: BoundedString<262144> => "text",
    required bytes: NonNegativeU64 => "bytes",
} }

// The committed session position every section was read at.
closed_object! { pub struct SessionSnapshotRevisionDto => "inline:session-snapshot-revision" {
    required aggregate: WireId<SessionId> => "aggregate",
    required seq: NonNegativeU64 => "seq",
} }

// Execution state as the session read reports it, at the same position.
closed_object! { pub struct SessionSnapshotExecutionDto => "inline:session-snapshot-execution" {
    optional state: SessionStateDto => "state",
    optional state_error: SessionStateErrorDto => "stateError",
} }

closed_object! { pub struct SessionSnapshotResponseDto => "inline:session-snapshot" {
    required revision: SessionSnapshotRevisionDto => "revision",
    required rows: BoundedVec<SessionMessageInfoDto, MAX_MESSAGE_ROWS> => "rows",
    required cursor: SessionMessagesResponseDtoCursor => "cursor",
    required permissions: BoundedVec<PermissionRequestStateDto, MAX_PERMISSION_ROWS> => "permissions",
    required forms: BoundedVec<FormInfoDto, MAX_CATALOG_ROWS> => "forms",
    required execution: SessionSnapshotExecutionDto => "execution",
    required provisional: BoundedVec<SessionSnapshotProvisionalDto, MAX_PROVISIONAL_BLOCKS> => "provisional",
} }

/// The snapshot read, implemented apart from the frozen session operations.
pub trait SessionSnapshotApplicationPort: Send + Sync + 'static {
    fn session_snapshot(
        &self,
        _principal: crate::SealedPrincipal,
        _session_id: WireId<SessionId>,
    ) -> super::ApplicationFuture<SessionSnapshotResponseDto, SessionMessagesApplicationError> {
        Box::pin(async { Err(SessionMessagesApplicationError::Unavailable) })
    }
}
