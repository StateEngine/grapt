//! Witnesses for the message-history fold.
//!
//! The fold's whole claim is that it is a FUNCTION of the durable rows, so
//! most of these tests fold a row list and assert what came out. Two of them
//! fold the SAME list twice and compare — that is the restart/relist equality
//! law, and it is asserted here rather than in an integration test because a
//! fold that is not a function cannot be repaired by the caller.

use super::*;
use grapt_event::session_message::{
    AssistantMessageOpenedFact, ContentAppendedFact, UsageObservedFact,
};
use grapt_event::{session_message as sm, session_step_ended, session_step_failed};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, AttemptUsage, CaptureDisposition, ContentKind,
    TokenCount, UsageField, UsageObservationPhase, UsageProvenance,
};

include!("tests/fold_witnesses.rs");
include!("tests/lifetime_bound_witnesses.rs");

#[path = "future_failure_witnesses.rs"]
mod future_failure_witnesses;

#[path = "retained_tests.rs"]
mod retained_tests;

#[path = "sustained_tests.rs"]
mod sustained_tests;

#[path = "binding_tests.rs"]
mod binding_tests;
