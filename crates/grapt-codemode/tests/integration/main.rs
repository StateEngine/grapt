//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../catalog.rs"]
mod catalog;
#[path = "../protocol.rs"]
mod protocol;
#[path = "../worker.rs"]
mod worker;
