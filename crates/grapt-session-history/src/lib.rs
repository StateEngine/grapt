//! The Session's durable history read model.
//!
//! One responsibility: fold recorded Session facts into the transcript a reader
//! shows, and read them under explicit bounds. It owns no lifecycle, no notice
//! vocabulary and no write path, so a consumer that only reads history does not
//! take the Session crate's whole surface to do it.
pub mod fork_boundary;
pub mod history;

// The fold witnesses reach the page bound through the crate root, as they did
// inside `grapt-session`.
pub use history::MAX_TOOL_CALL_PAGE_ROWS;
// `grapt-session` re-exports these so its own consumers keep their paths.
pub use history::{
    AssistantMessage, CapturedContentItem, CommittedPosition, DurableToolCall, HistoryError,
    HistoryOmission, HistoryOmissionClass, HistoryReadError, HistoryRow, MessageContent,
    MessageHistory, SessionHistoryStore, SessionToolCallPage, StepTerminal, MAX_HISTORY_DATA_BYTES,
    MAX_HISTORY_ROWS,
};
