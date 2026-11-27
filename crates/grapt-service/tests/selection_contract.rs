//! The selection/attribution laws, each shown to fail for its own reason.
//!
//! Every refusal below is a separate assertion against a separate variant, so
//! no one law can pass by tripping another's guard. That separation is the
//! point: the failure classes this lane must keep independent are exactly the
//! ones a single shared assertion would collapse.

use std::error::Error;

use grapt_service::{
    AdmittedDifferential, AttachmentRefusal, EngineSelection, ProbeOutcome, SelectionReceipt,
    SelectorState,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceSlotId,
};

/// An injected clock. `selected_at` can only arrive through one of these, so a
/// test that wants a known stamp supplies it and a production caller supplies a
/// real adapter. There is no constructor that takes a `Timestamp` directly.
#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

/// An owner receipt with each identity coordinate and attribute controllable,
/// so a test can vary exactly one of them.
fn owner(
    slot: &str,
    instance: &str,
    environment: &str,
    pid: u32,
    port: u16,
) -> Option<OwnerReceipt> {
    Some(OwnerReceipt::new(
        ServiceSlotId::new(slot)?,
        ServiceInstanceId::new(instance)?,
        EnvironmentReceipt::new(environment)?,
        ServiceGeneration::new(1),
        ProcessId::new(pid)?,
        Endpoint::new(format!("http://127.0.0.1:{port}"))?,
    ))
}

fn rust_owner() -> Option<OwnerReceipt> {
    owner("slot-desktop", "inst-rust", "env-a", 4242, 41_001)
}

fn retained_owner() -> Option<OwnerReceipt> {
    owner("slot-desktop", "inst-retained", "env-a", 5353, 41_002)
}

#[test]
fn an_attributed_probe_is_admitted_and_carries_the_injected_instant() -> Result<(), Box<dyn Error>>
{
    let observed = rust_owner().ok_or("owner")?;
    let receipt = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(observed.clone()),
        &FixedClock(1_700_000_000_123),
    )?;

    assert_eq!(receipt.selection(), EngineSelection::Rust);
    assert_eq!(receipt.owner(), &observed);
    assert_eq!(
        receipt.selected_at(),
        Timestamp::from_millis(1_700_000_000_123),
        "selected_at must be exactly what the injected clock returned"
    );
    assert_eq!(
        receipt.endpoint(),
        observed.endpoint(),
        "the endpoint is read through the owner, so there is one carrier for it"
    );
    Ok(())
}

#[test]
fn selected_at_comes_from_the_clock_and_nowhere_else() -> Result<(), Box<dyn Error>> {
    let observed = rust_owner().ok_or("owner")?;
    let early = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(observed.clone()),
        &FixedClock(1_000),
    )?;
    let late = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(observed),
        &FixedClock(2_000),
    )?;
    assert_eq!(early.selected_at(), Timestamp::from_millis(1_000));
    assert_eq!(late.selected_at(), Timestamp::from_millis(2_000));
    assert_ne!(
        early.selected_at(),
        late.selected_at(),
        "two different injected clocks must produce two different instants"
    );
    Ok(())
}

#[test]
fn a_health_only_answer_cannot_produce_a_receipt() {
    // The frozen probe accepts any 2xx. Liveness without an owner is not an
    // attachment, and there is no path from this outcome to a receipt.
    match SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::HealthyUnattributed,
        &FixedClock(1),
    ) {
        Err(AttachmentRefusal::Unattributed) => {}
        other => panic!("a health-only answer must be refused as unattributed, got {other:?}"),
    }
}

#[test]
fn a_stale_selection_is_refused_against_a_different_owner() -> Result<(), Box<dyn Error>> {
    let receipt = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(rust_owner().ok_or("owner")?),
        &FixedClock(1),
    )?;

    let someone_else = owner("slot-desktop", "inst-other", "env-a", 4242, 41_001).ok_or("other")?;
    match receipt.still_current(&someone_else) {
        Err(AttachmentRefusal::NotTheRecordedOwner) => {}
        other => panic!("a different owner must refuse as stale, got {other:?}"),
    }
    Ok(())
}

#[test]
fn a_reused_pid_does_not_inherit_the_attachment() -> Result<(), Box<dyn Error>> {
    let receipt = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(rust_owner().ok_or("owner")?),
        &FixedClock(1),
    )?;

    // Same PID and endpoint, different instance: NOT the recorded owner.
    let impostor =
        owner("slot-desktop", "inst-impostor", "env-a", 4242, 41_001).ok_or("impostor")?;
    match receipt.still_current(&impostor) {
        Err(AttachmentRefusal::NotTheRecordedOwner) => {}
        other => panic!("a reused PID must not inherit the attachment, got {other:?}"),
    }

    // The positive direction, which is what makes the check identity-based
    // rather than merely strict: the same owner behind a new PID and a new
    // ephemeral port still holds the attachment.
    let restarted = owner("slot-desktop", "inst-rust", "env-a", 9_999, 41_099).ok_or("restart")?;
    receipt.still_current(&restarted)?;
    Ok(())
}

#[test]
fn a_differential_over_one_owner_is_refused() -> Result<(), Box<dyn Error>> {
    // The degenerate case that would otherwise report perfect parity: the
    // selector claims two engines, but both sides reached the same service.
    let shared = rust_owner().ok_or("owner")?;
    let left = SelectionReceipt::admit(
        EngineSelection::Retained,
        ProbeOutcome::Attributed(shared.clone()),
        &FixedClock(1),
    )?;
    let right = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(shared),
        &FixedClock(2),
    )?;

    match AdmittedDifferential::admit(left, right) {
        Err(AttachmentRefusal::SameOwnerOnBothSides) => Ok(()),
        other => panic!("one engine measured twice must be refused, got {other:?}"),
    }
}

#[test]
fn a_differential_over_one_engine_selection_is_refused() -> Result<(), Box<dyn Error>> {
    // Distinct owners, but both sides name the same engine: not a comparison.
    // Separate from the same-owner refusal so neither can mask the other.
    let left = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(rust_owner().ok_or("rust")?),
        &FixedClock(1),
    )?;
    let right = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(retained_owner().ok_or("retained")?),
        &FixedClock(2),
    )?;

    match AdmittedDifferential::admit(left, right) {
        Err(AttachmentRefusal::SameSelectionOnBothSides) => Ok(()),
        other => panic!("a same-engine pair must be refused, got {other:?}"),
    }
}

#[test]
fn two_different_engines_on_two_different_owners_are_admitted() -> Result<(), Box<dyn Error>> {
    // The positive control for the two refusals above. Without it, both could
    // be passing because `admit` refuses everything.
    let left = SelectionReceipt::admit(
        EngineSelection::Retained,
        ProbeOutcome::Attributed(retained_owner().ok_or("retained")?),
        &FixedClock(1),
    )?;
    let right = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(rust_owner().ok_or("rust")?),
        &FixedClock(2),
    )?;

    let pair = AdmittedDifferential::admit(left, right)?;
    assert_eq!(pair.left().selection(), EngineSelection::Retained);
    assert_eq!(pair.right().selection(), EngineSelection::Rust);
    assert!(
        !pair.left().owner().is_same_owner(pair.right().owner()),
        "an admitted differential must name two different owners"
    );
    Ok(())
}

#[test]
fn the_selector_reverts_from_every_state() -> Result<(), Box<dyn Error>> {
    let receipt = SelectionReceipt::admit(
        EngineSelection::Rust,
        ProbeOutcome::Attributed(rust_owner().ok_or("owner")?),
        &FixedClock(1),
    )?;

    let attached = SelectorState::attach(receipt.clone());
    assert!(
        attached.receipt().is_some(),
        "an attached selector carries its receipt"
    );

    assert_eq!(
        attached.revert(),
        SelectorState::Retained,
        "reverting must return to the retained engine: the switch is not one-way"
    );
    assert_eq!(
        SelectorState::Retained.revert(),
        SelectorState::Retained,
        "reverting is total, so it is available from every state"
    );

    // And the switch can be thrown again afterwards — reversibility that only
    // works once is not reversibility.
    let reattached = SelectorState::attach(receipt);
    assert!(reattached.receipt().is_some());
    Ok(())
}

#[test]
fn an_unknown_label_is_refused_rather_than_defaulted() {
    // Lives here rather than beside the enum so the mutation battery can reach
    // it: a row naming an integration target cannot select a `#[cfg(test)]`
    // unit test, and a filter that matches nothing exits zero.
    for unknown in ["", "RETAINED", "typescript", "node", "rust "] {
        assert_eq!(
            EngineSelection::from_label(unknown),
            None,
            "{unknown:?} must not resolve to an engine"
        );
    }
}
