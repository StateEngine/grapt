//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../artifact_store.rs"]
mod artifact_store;
#[path = "../carry_structural.rs"]
mod carry_structural;
#[path = "../memory_store.rs"]
mod memory_store;
