//! Client-side transport for the pure `grapt-mcp` protocol domain.
//!
//! SSE parsing and recovery policy are independent of HTTP I/O. Callers retain
//! session identity, request correlation, permissions, and durable state.

mod active_time;
mod bridge;
mod http;
pub mod oauth;
mod reconnect;
mod sse;

pub use active_time::{HumanWait, HumanWaitGuard};
pub use bridge::{connect, RemoteConnection, RemoteRead, RemoteTask, TransportBudgets};
pub use http::auth::{BearerFuture, BearerLease, BearerProvider};
pub use http::{HttpClient, HttpError, HttpReply, Incoming};
pub use reconnect::{ReconnectExhausted, ReconnectPolicy};
pub use sse::{SseDecodeError, SseDecoder, SseEvent};
