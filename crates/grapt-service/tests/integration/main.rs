//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../attachment_corpus.rs"]
mod attachment_corpus;
#[path = "../authenticated_health_probe.rs"]
mod authenticated_health_probe;
#[path = "../controller_contract.rs"]
mod controller_contract;
#[path = "../controller_corpus.rs"]
mod controller_corpus;
#[path = "../database_transition.rs"]
mod database_transition;
#[path = "../differential_contract.rs"]
mod differential_contract;
#[path = "../differential_corpus.rs"]
mod differential_corpus;
#[path = "../host_bound_drift.rs"]
mod host_bound_drift;
#[path = "../operator_contract.rs"]
mod operator_contract;
#[path = "../operator_corpus.rs"]
mod operator_corpus;
#[path = "../registration_durability.rs"]
mod registration_durability;
#[path = "../schema_conformance.rs"]
mod schema_conformance;
#[path = "../selection_contract.rs"]
mod selection_contract;
