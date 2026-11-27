//! Builds what a model sees each turn: request context, history paging,
//! compaction and summarization.
//!
//! History is read only through the session and inbox query interfaces. Model
//! limits and tool formatting arrive through a narrow interface supplied by the
//! server application, never by reaching into it.

pub mod history;
pub mod response_order;
