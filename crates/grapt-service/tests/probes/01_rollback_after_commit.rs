//! Law: a rollback message may never be printed after the candidate became
//! current (master §"Service replacement lifecycle": *"No caller infers success
//! from a boolean or prints a rollback message after the candidate became
//! current"*).
//!
//! `ServiceController::commit` takes the `PreparedCandidate` **by value**, so
//! after a commit there is no candidate left to roll back. This probe must not
//! compile: the failure is a use-after-move in borrow checking, not a missing
//! name.

use grapt_service::{
    ElectedControllerAuthority, Observation, ProbeOutcome, Retirement, ServiceController,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(0)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = ElectedControllerAuthority::isolated();
    let clock = FixedClock;
    let controller = ServiceController::new(
        ServiceSlotId::new("slot-secode").ok_or("slot")?,
        EnvironmentReceipt::new("env-alpha").ok_or("environment")?,
        &store,
        &clock,
    );
    let declared = OwnerReceipt::new(
        ServiceSlotId::new("slot-secode").ok_or("slot")?,
        ServiceInstanceId::new("inst-a").ok_or("instance")?,
        EnvironmentReceipt::new("env-alpha").ok_or("environment")?,
        ServiceGeneration::FIRST,
        ProcessId::new(4242).ok_or("pid")?,
        Endpoint::new("http://127.0.0.1:8080").ok_or("endpoint")?,
    );
    let candidate = Observation::new(
        declared.clone(),
        ProbeOutcome::Attributed(declared),
        ServiceRegistrationVersion::new("0.1.0").ok_or("version")?,
    );
    let discovered = controller.discover()?;
    let prepared = controller.prepare(&discovered, &candidate)?;

    // The commit consumes the candidate.
    let _receipt = controller.commit(prepared, Retirement::NoIncumbent)?;

    // And so this cannot be reached: the candidate is current, and calling it a
    // rollback would be a lie the type system refuses to let anyone tell.
    let _rolled_back = prepared.rollback();
    Ok(())
}
