//! Witnesses for the MCP transport, registry and publication.
//!
//! Split by concern under the workspace god-file ceiling. They live beside
//! their subject rather than in a `tests/` tree because every one of them
//! drives `pub(super)` surface an integration test could not reach.

mod bounds;
mod child;
mod fixtures;
mod liveness;
mod pagination;
mod phases;
mod publication;
mod registry;
mod retirement;
mod startup;

mod recovery;

mod rebind;

mod cancellation;
mod continuity_startup;

mod producer_version;
mod result_isolation;

mod progress;
