//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

// One instance per process: its call counter keeps every scratch root distinct.
#[path = "../support/receipt_scratch.rs"]
mod receipt_scratch;

#[path = "../eb_arguments.rs"]
mod eb_arguments;
#[path = "../eb_custody.rs"]
mod eb_custody;
#[path = "../eb_platform.rs"]
mod eb_platform;
#[path = "../eb_receipt.rs"]
mod eb_receipt;
#[path = "../eb_service.rs"]
mod eb_service;
