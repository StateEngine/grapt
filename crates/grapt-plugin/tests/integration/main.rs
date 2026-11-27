//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

// One instance per process: its counter keeps every launch nonce distinct.
#[path = "../support/recovery.rs"]
mod support;

#[path = "../bridge.rs"]
mod bridge;
#[path = "../macos_mutations.rs"]
mod macos_mutations;
#[path = "../unsupported_platform.rs"]
mod unsupported_platform;
