//! Session lifecycle projection.
//!
//! Measured semantics (frozen clone, 2026-08-08): `session.created.1` seeds
//! the row from its `info` object (including `time` — the table's
//! `time_created` is `info.time`, NOT the event row's `created` column, which
//! is 1ms later on the sampled row); `renamed`/`agent.selected`/
//! `model.selected`/`preset.selected` are last-write-wins updates. Model is
//! stored as a JSON object `{id, providerID, variant}`.

pub mod config_binding;
pub mod create;
mod deletion;
pub mod event;
pub mod execution;
pub mod interrupted_turn;
pub mod lifecycle;
pub mod list;
pub mod notice;
pub mod patch_receipt;
pub mod projection;
pub mod record;
pub mod rewind;
pub mod selection;
pub mod store;
pub mod terminal;
pub mod title;

pub use create::{
    create_session, SessionCreateError, SessionCreated, SessionSeed, MAX_SLUG_BYTES,
    MAX_TITLE_BYTES,
};
pub use event::{SessionEvent, SessionEventError};
// The history read model now lives in `grapt-session-history`; these stay
// re-exported so a Session consumer reads the same paths it always did.
pub use grapt_session_history::{
    history, AssistantMessage, CapturedContentItem, CommittedPosition, DurableToolCall,
    HistoryError, HistoryOmission, HistoryOmissionClass, HistoryReadError, HistoryRow,
    MessageContent, MessageHistory, SessionHistoryStore, SessionToolCallPage, StepTerminal,
    MAX_HISTORY_DATA_BYTES, MAX_HISTORY_ROWS, MAX_TOOL_CALL_PAGE_ROWS,
};
pub use lifecycle::{
    delete_session, fork_session, fork_session_at_seq, fork_session_before_call,
    fork_session_before_call_at_location, fork_session_before_prompt, SessionDeleted,
    SessionForked, SessionLifecycleError,
};
pub use list::{
    BoundaryKey, BoundedSearch, KeyDigest, NonNegativeMs, ParentFilter, SessionListBoundary,
    SessionListCriteria, SessionListDegradedRow, SessionListDirection, SessionListError,
    SessionListItem, SessionListKey, SessionListLimits, SessionListOrder, SessionListPage,
    SessionListProjector, SessionListProvenance, SessionListScope, SessionListStore,
};
pub use projection::{SessionProjection, SessionRow};
pub use record::SessionRecord;
pub use store::{SessionStore, SessionStoreError};
