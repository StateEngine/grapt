//! Shared typed client, projection and presentation helpers for terminal consumers.
//!
//! ## The one architectural claim
//!
//! This crate observes and drives the product ONLY through the versioned U3a
//! DTO/protocol surface. That is ratified law, not a preference:
//!
//! > TUI and desktop consume the same DTO/protocol surface even when embedded;
//! > in-process transport may optimize delivery but not bypass the boundary.
//!
//! and the supersession matrix retires the older reading by name — "Embedded
//! TUI may call Core/runtime directly for speed" is superseded by
//! "Desktop/TUI/CLI use generated versioned DTOs in embedded, daemon and
//! remote transports."
//!
//! So the dependency list is the architecture. There is no `grapt-runtime`,
//! `grapt-store`, `grapt-session` or `grapt-event` dependency and there
//! must never be one. A client that could reach the engine through a Rust call
//! would render truth the wire cannot carry, and every reconnect and restart
//! law below would be certifying a path the real remote transport never takes.
//! `tests/dependency_direction.rs` is the structural gate;
//! `tests/compile_fail.rs` is the compile-time witness that the bypass has no
//! public route at all.
//!
//! ## Layering
//!
//! - [`contract`] — the frozen wire vocabulary, read from the compatibility
//!   record, including the durable/live asymmetry that makes reconciliation
//!   provable rather than merely asserted.
//!
//! Each layer below is a pure function of the layer above wherever it can be.
//! The reason is not elegance: a projection that is a pure fold over decoded
//! frames can be witnessed against a synthetic transcript carrying the exact
//! defect it exists to catch, including transcripts no server could be made to
//! produce on purpose — a duplicated terminal, a sequence that goes backwards,
//! a gap that heals into a duplicate.
//!
//! ## What this crate deliberately does not port
//!
//! The frozen oracle TUI is a UX oracle, not a blueprint. The god-file map
//! names `tui/src/routes/session/index.tsx` (3,480 LOC) as DO-NOT-PORT and
//! `tui/src/mini/stream-v2.transport.ts` (1,786 LOC) as delete-its-race-and-
//! timer-mechanics. Neither shape appears here: the transcript is a typed
//! projection plus a reducer, the transport is cursor-aware with durable
//! reconciliation, and there is no timer/idle race because idleness is a
//! contract member rather than an inferred timeout.

pub mod cells;
// The transport, contract and wire layer lives below this crate; its public
// paths stay here unchanged.
pub use grapt_client_transport::{bounds, cache, contract, format, http, preset, transport, wire};
pub mod client;
pub mod composer_commands;
pub mod composer_history;
pub mod cursor;
pub mod double;
pub mod file_finder;
pub mod input_ids;
pub mod keymap;
pub mod permission_input;
pub mod projection;
pub mod style;
pub mod supervise;
pub mod view;
pub mod vt;

pub use bounds::{Bounds, BoundsError};
pub use client::{
    Client, ClientError, Connection, Diagnostics, PeriodicRead, PermissionAsk, PreparedRead,
    SessionSummary,
};
// `EVENT_ID_PREFIX` and `SESSION_ID_PREFIX` are DELETED rather than deprecated.
//
// They were the carriers of review blocker 1: two public string constants whose
// only use was `starts_with`. Leaving them exported would leave the flattened
// acceptance one call away and reachable from outside the crate, which is
// exactly the shape a future author restores by accident. Identity law now has
// one carrier — `grapt_types` — reached through `crate::wire`.
pub use contract::{
    ContractError, DurableRef, EventEnvelope, EventMember, Method, Operation, DURABLE_VERSION,
};
pub use cursor::{Admission, DurableCursor, GapReason, StreamState};
pub use double::{ContractDouble, Fault, PendingPermission};
pub use http::{BasicCredential, HttpConnection, HttpError, StreamCancellation};
pub use keymap::Key;
pub use projection::{
    Applied, ExecutionState, Outcome, ProjectionError, SessionView, Slice, SliceKind, Usage,
};
pub use style::{Emphasis, Paint, Style, Token};
pub use transport::{FramePoll, SseReader, Transport, TransportError, WireRequest, WireResponse};

#[cfg(test)]
mod transport_layer_tests;
