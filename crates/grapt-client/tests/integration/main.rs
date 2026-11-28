//! Share one link for this crate's integration tests.
//! Source files stay in place so their relative fixtures and paths survive.

#[path = "../after_cut_recovery.rs"]
mod after_cut_recovery;
#[path = "../held_identity_routing.rs"]
mod held_identity_routing;
#[path = "../held_reasoning.rs"]
mod held_reasoning;
#[path = "../positions_negotiation.rs"]
mod positions_negotiation;
#[path = "../positions_snapshot.rs"]
mod positions_snapshot;
#[path = "../resume_equivalence.rs"]
mod resume_equivalence;
#[path = "../slice_identity.rs"]
mod slice_identity;
#[path = "../snapshot_join.rs"]
mod snapshot_join;
