//! One actor per session serializes that session's runtime state machine.
//!
//! Every transition commits to the run store before the actor publishes the
//! new `(SessionRuntimeState, StateSeq)` snapshot and its change, so a
//! subscriber never sees a state the store could lose. The actor knows only
//! a single session's turn lifecycle; it has no mail or multi-agent concepts.

mod actor;
mod dedup;
mod error;
mod key;
mod state;
mod store;

pub use actor::{ActorConfig, ActorHandle, SessionActor};
pub use error::{SpawnError, SubmitError};
pub use key::{InvalidSubmitKey, SubmitKey};
pub use state::{
    Applied, Event, InvalidTransition, RuntimeSnapshot, SessionRuntimeState, StateChange, StateSeq,
};
pub use store::{load, LoadError, SCHEMA};
