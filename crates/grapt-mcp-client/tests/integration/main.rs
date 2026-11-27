//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../reconnect.rs"]
mod reconnect;
#[path = "../sse.rs"]
mod sse;
