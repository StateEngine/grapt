//! The client's transport, contract and wire layer, below `grapt-client`:
//! the HTTP connection and its SSE reader, the frozen operation contract, the
//! wire DTO decoders, the cache-usage model those decoders produce, and the
//! client's bounds. It owns no client state; the client depends on it, never
//! the reverse.

pub mod bounds;
pub mod cache;
pub mod connection;
pub mod contract;
pub mod format;
pub mod http;
pub mod preset;
pub mod transport;
pub mod wire;
