//! A turn a service stop cut short, and whether the service may continue it.
//!
//! The record is the durable answer to "was this stop the user's decision or
//! the service's?" so that a later service can act on it without the process
//! that wrote it.
mod turn;

pub(crate) use turn::interrupted_turns_at;
pub(crate) use turn::INTERRUPTED_TURN;
pub use turn::{
    interrupted_turns, pending_resume, record_interrupted_turn, InterruptedTurn, Resume, StoppedBy,
};
#[cfg(test)]
mod tests;
