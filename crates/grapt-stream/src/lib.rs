//! The event stream's transport-neutral core, below the HTTP API: its bounds
//! and refusals, the hub's stream positions and replay ring, the live-block
//! registry a snapshot reads, and the reconcile counters.
//!
//! Nothing here knows an event type, a route or a principal. The API encodes
//! events and owns the hub; it takes the subscriber lock, then the registry's,
//! and never the reverse.

pub mod limits;
pub mod positions;
pub mod provisional;
pub mod reconciles;
pub mod replay;
