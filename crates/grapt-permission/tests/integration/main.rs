//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../command_indirection.rs"]
mod command_indirection;
#[path = "../permission_http.rs"]
mod permission_http;
#[path = "../permission_service.rs"]
mod permission_service;
