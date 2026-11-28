// SPDX-License-Identifier: MIT OR Apache-2.0
//! A client for a StateEngine host's local IPC protocol, version 1: endpoint
//! discovery, the pinned host binary check, bounded JSON framing, the
//! three-step owner handshake and the request/response exchange.
//!
//! The crate links no StateEngine code; it speaks the published wire format.
//! No owner proof leaves the process before the connected server is checked
//! against the endpoint: on Linux by its kernel-reported user, process, start
//! time and executable. Other platforms refuse until they can check as much.
//! It draws no randomness and reads no clock beyond Tokio's timeout: callers
//! supply the client nonce from their own named entropy adapter.
#![forbid(unsafe_code)]

mod activity;
mod binary;
mod canonical;
mod connection;
mod digest;
mod endpoint;
mod error;
mod events;
mod frame;
mod handshake;
mod ids;
#[cfg(target_os = "linux")]
mod linux_proc;
mod peer;
mod proof;
mod server_peer;
mod state_dir;
mod strict;
mod turn;
pub mod wire;

pub use activity::{Activity, ActivityReceipt, ExternalRequest, ACTIVITY_CAPABILITY};
pub use binary::verify_binary;
pub use connection::Connection;
pub use digest::Sha256Hex;
pub use endpoint::{discover, Endpoint};
pub use error::IpcError;
pub use events::{
    Event, EventCursor, EventKind, EventStream, StreamFrame, SubscribeRequest, EVENTS_CAPABILITY,
    MIN_BATCH_BYTES,
};
pub use frame::{read_frame, write_frame};
pub use grapt_stateengine_api::{AgentId, EventId, IdempotencyKey, ProjectId, SessionId};
pub use handshake::{Capabilities, HandshakeOptions, Session};
#[cfg(target_os = "linux")]
pub use linux_proc::observe_self;
pub use peer::{PeerBinding, Principal};
#[cfg(target_os = "linux")]
pub use server_peer::connect;
pub use server_peer::VerifiedPeer;
pub use turn::{
    CommitState, PublishTurn, PublishTurnReceipt, TurnCommit, TurnDecision, TurnDiff, TurnModel,
    TurnNesting, TurnRole, TurnUsage, PUBLISH_TURN_CAPABILITY,
};

use std::time::Duration;

/// The protocol version this crate speaks.
pub const API_VERSION: wire::Version = wire::Version { major: 1, minor: 0 };
/// The absolute bound on any frame.
pub const MAX_FRAME_BYTES: u32 = 1_048_576;
/// The reply bound a client asks for unless it chooses another.
pub const DEFAULT_REPLY_BYTES: u32 = 262_144;
/// The bound on `endpoint.json` and on every handshake frame.
pub const MAX_ENDPOINT_BYTES: u32 = 16_384;
/// The deepest nesting of objects and arrays any frame may carry.
pub const MAX_JSON_DEPTH: usize = 64;
/// How long the whole handshake may take.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// The capability that says a host negotiates capabilities.
pub const CAPABILITY_NEGOTIATION: &str = "ipc.capabilities";
