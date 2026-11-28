//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../aws_credential_owner.rs"]
mod aws_credential_owner;
#[path = "../connection_startup.rs"]
mod connection_startup;
#[path = "../google_adc_bootstrap.rs"]
mod google_adc_bootstrap;
#[path = "../profile_approval.rs"]
mod profile_approval;
#[path = "../profile_connection_migration.rs"]
mod profile_connection_migration;

/// The libtest path of test `name` in the module whose `module_path!()` is
/// `module`, for a test that re-runs itself in a child process.
pub(crate) fn exact(module: &str, name: &str) -> String {
    let path = module.split_once("::").map_or(module, |(_, path)| path);
    format!("{path}::{name}")
}
