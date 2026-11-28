//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../aws_selection.rs"]
mod aws_selection;
#[path = "../azure_cli_boundaries.rs"]
mod azure_cli_boundaries;
#[path = "../google_adc_sources.rs"]
mod google_adc_sources;
#[path = "../vault_diagnostics.rs"]
mod vault_diagnostics;
#[path = "../vertex_adc_sources.rs"]
mod vertex_adc_sources;

/// The libtest path of test `name` in the module whose `module_path!()` is
/// `module`, for a test that re-runs itself in a child process.
pub(crate) fn exact(module: &str, name: &str) -> String {
    let path = module.split_once("::").map_or(module, |(_, path)| path);
    format!("{path}::{name}")
}
