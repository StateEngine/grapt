//! The R4H-H host witnesses that need a server-derived principal.
//!
//! These live in `src/` and not in `tests/`, and the reason is structural
//! rather than stylistic. A's fixture verification route is `#[cfg(test)]`, so
//! it compiles into this crate's library test binary and into nothing else — an
//! integration test is a separate crate and cannot reach it. Production A
//! authenticates and REFUSES every credential (ruling R4HA-04), so a witness
//! that must show a sealed principal arriving at a route has exactly one place
//! it can be written. The host witnesses that need no principal live in
//! `tests/`, where a hostile client belongs.
//!
//! Nothing here sleeps on the wall clock, resolves a name, opens a database or
//! reaches the network. Every listener is loopback with an ephemeral port
//! obtained through the accepted location crate, and every deadline that must
//! fire is fired by the witness.

mod admission;
mod doubles;
mod fixtures;
mod http1_client;
mod shutdown;
