//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../support/mod.rs"]
mod support;

#[path = "../bounded_capture.rs"]
mod bounded_capture;
#[path = "../discovery_order.rs"]
mod discovery_order;
#[path = "../jsonc_strictness.rs"]
mod jsonc_strictness;
#[path = "../layer_bridge.rs"]
mod layer_bridge;
#[path = "../ledger_admission.rs"]
mod ledger_admission;
#[path = "../reload_authority.rs"]
mod reload_authority;
#[path = "../resolution_precedence.rs"]
mod resolution_precedence;
#[path = "../secret_containment.rs"]
mod secret_containment;
