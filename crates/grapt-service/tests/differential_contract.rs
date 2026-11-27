//! Every law the R4D-2A differential driver contract adds, witnessed against
//! its own refusal variant.
//!
//! # Why each law gets its own assertion
//!
//! A witness that only checks "it refused" passes when the wrong guard fires,
//! and a table of laws sharing one assertion is one law witnessed many times.
//! So each test below names the exact [`DifferentialRefusal`] variant it
//! expects, and the positive control at the end proves the same inputs are
//! admitted once the single defect is repaired — otherwise a refusal could be
//! coming from something incidental to the case.
//!
//! # What is NOT claimed here
//!
//! Nothing in this file starts, contacts or measures an engine. Every
//! `OwnerReceipt` is a value built from literals, every timestamp comes from
//! the injected test clock, and no assertion says anything about a running
//! service.

use grapt_service::{
    AdmittedDifferential, AttachmentRefusal, BoundProvenance, DifferentialMatrix, DifferentialPlan,
    DifferentialRefusal, EngineSelection, HostBoundName, LongSessionBound, MatrixEvidence,
    Operation, OperationSubset, ProbeOutcome, RecordedBound, RecordedHostBounds, RestartBoundary,
    ResumeReadmission, SelectionReceipt, SelectorState,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceSlotId,
};

/// A clock that returns exactly what it was told to, so every recorded instant
/// in this file is an input rather than an observation.
#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

/// Build an owner receipt, or fail loudly.
///
/// `unwrap`/`expect` are denied workspace-wide, and that is not a formality
/// here: a silent `unwrap_or_default` on an identity would let a test construct
/// an owner that is not the one it meant to name.
fn owner(slot: &str, instance: &str, environment: &str, pid: u32, endpoint: &str) -> OwnerReceipt {
    let parts = (
        ServiceSlotId::new(slot),
        ServiceInstanceId::new(instance),
        EnvironmentReceipt::new(environment),
        ProcessId::new(pid),
        Endpoint::new(endpoint),
    );
    match parts {
        (Some(slot), Some(instance), Some(environment), Some(pid), Some(endpoint)) => {
            OwnerReceipt::new(
                slot,
                instance,
                environment,
                ServiceGeneration::FIRST,
                pid,
                endpoint,
            )
        }
        _ => panic!("an identity in this fixture is unrepresentable: {slot}/{instance}"),
    }
}

fn rust_owner() -> OwnerReceipt {
    owner(
        "slot_desktop",
        "inst_rust_a",
        "env_rust",
        4101,
        "http://127.0.0.1:4101",
    )
}

fn retained_owner() -> OwnerReceipt {
    owner(
        "slot_desktop_retained",
        "inst_retained_a",
        "env_retained",
        4102,
        "http://127.0.0.1:4102",
    )
}

fn receipt(selection: EngineSelection, owner: OwnerReceipt, at: i64) -> SelectionReceipt {
    match SelectionReceipt::admit(selection, ProbeOutcome::Attributed(owner), &FixedClock(at)) {
        Ok(receipt) => receipt,
        Err(refusal) => panic!("an attributed probe must admit, but it refused: {refusal}"),
    }
}

fn admitted_pair() -> AdmittedDifferential {
    let left = receipt(EngineSelection::Rust, rust_owner(), 1_000);
    let right = receipt(EngineSelection::Retained, retained_owner(), 2_000);
    match AdmittedDifferential::admit(left, right) {
        Ok(pair) => pair,
        Err(refusal) => panic!("two different engines must admit, but it refused: {refusal}"),
    }
}

/// A complete, host-sourced bound recording.
fn complete_bounds() -> Vec<RecordedBound> {
    HostBoundName::ALL
        .into_iter()
        .map(|name| {
            RecordedBound::new(
                name,
                "recorded-by-host",
                BoundProvenance::ReportedByHostReceipt,
            )
        })
        .collect()
}

fn recorded_bounds() -> RecordedHostBounds {
    match RecordedHostBounds::record(complete_bounds()) {
        Ok(bounds) => bounds,
        Err(refusal) => panic!("a complete host-sourced recording must admit: {refusal}"),
    }
}

fn subset(matrix: DifferentialMatrix) -> OperationSubset {
    match OperationSubset::declare(matrix, matrix.minimum_subset().to_vec()) {
        Ok(subset) => subset,
        Err(refusal) => panic!("a matrix's own minimum subset must declare: {refusal}"),
    }
}

/// Assert an exact refusal variant, reporting what actually happened.
fn expect_refusal<T: std::fmt::Debug>(
    outcome: Result<T, DifferentialRefusal>,
    expected: &DifferentialRefusal,
    law: &str,
) {
    match outcome {
        Ok(value) => panic!("{law}: expected a refusal but it was admitted: {value:?}"),
        Err(actual) => assert_eq!(
            &actual, expected,
            "{law}: refused for the wrong reason — the guard that fired is not the law under test"
        ),
    }
}

#[path = "differential_contract/comparison.rs"]
mod comparison;
#[path = "differential_contract/construction.rs"]
mod construction;
#[path = "differential_contract/lifecycle.rs"]
mod lifecycle;
