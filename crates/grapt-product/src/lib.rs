//! Product composition root: the server's command surface, the optional linked
//! engine seam, and measurement surfaces shared with acceptance instruments.

pub mod cache_accounting;
pub mod checkpoint;
#[cfg(feature = "checkpoint-fixture")]
pub mod checkpoint_fixture;
pub mod session_cleanup;
pub mod usage_accounting;

#[cfg(test)]
#[path = "../tests/support/native_mail_peer.rs"]
pub(crate) mod native_mail_test_peer;

#[cfg(test)]
use grapt_store::EventStore;
#[cfg(test)]
#[path = "../../../test-support/commit_fault.rs"]
mod commit_fault;

mod cleanup_command;
mod command;
mod commit_executor;
mod compose;
mod compose_host;
mod diagnostics;
mod dogfood;
mod dogfood_application_port;
mod dogfood_composition;
mod fixture_seam;
mod handover;
mod input;
mod installation;
mod provider_error_text;
mod ready;
mod receipt;
mod retire_service;
mod server;
mod server_log;
mod startup;
mod stateengine;

pub use server::run;
pub use stateengine::{EmbeddedFactoryBuilder, StateEngine};
