//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../support/mod.rs"]
mod support;

#[path = "../effective_graph.rs"]
mod effective_graph;
#[path = "../graph_contributors.rs"]
mod graph_contributors;
#[path = "../graph_receipt.rs"]
mod graph_receipt;
#[path = "../prefix_identity_bounds.rs"]
mod prefix_identity_bounds;
