//! Share one link for integration tests without process-wide fixture state.
//! Keep source files in place so their relative fixtures and diagnostics survive.

#[path = "../cache_usage_benchmark.rs"]
mod cache_usage_benchmark;
#[path = "../carry_admission.rs"]
mod carry_admission;
#[path = "../config_refusals.rs"]
mod config_refusals;
#[path = "../dependency_direction.rs"]
mod dependency_direction;
#[path = "../launcher_environment.rs"]
mod launcher_environment;
#[path = "../live_usage_accounting.rs"]
mod live_usage_accounting;
#[path = "../live_usage_accounting_dispatch.rs"]
mod live_usage_accounting_dispatch;
#[path = "../plugin_compatibility.rs"]
mod plugin_compatibility;
#[path = "../profile_approval.rs"]
mod profile_approval;
#[path = "../rename_compatibility.rs"]
mod rename_compatibility;
#[path = "../sandbox_policy.rs"]
mod sandbox_policy;
#[path = "../startup_inspection.rs"]
mod startup_inspection;
#[path = "../stream_end_reasons.rs"]
mod stream_end_reasons;
#[path = "../stream_positions.rs"]
mod stream_positions;
#[path = "../tool_effect_census.rs"]
mod tool_effect_census;
#[path = "../toolchain_activate.rs"]
mod toolchain_activate;
#[path = "../windows_terminal_dependencies.rs"]
mod windows_terminal_dependencies;
