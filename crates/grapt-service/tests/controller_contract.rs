//! R4D-2C acceptance: the service controller's transitions and their laws.
//!
//! Every test here is one row of master §"Service replacement lifecycle" or of
//! the design-gate row at L1333 — *"Candidate failure, CAS loss, stale
//! registration/PID reuse, successful switch, retire warning, and service
//! rebind fixture"* — and of the disposition row at L703 — *"PID reuse, stale
//! registration, concurrent starters, failed candidate and rollback
//! linearizability."*
//!
//! They are separate functions rather than table rows on purpose. The mutation
//! battery's duplicate-evidence guard compares the panic each mutant produces,
//! and two rows of one table test share a source line — so two different laws
//! killed by two different mutants would arrive as one indistinguishable piece
//! of evidence. One law, one test, one failure site.
//!
//! The durable half — the real file lock, the atomic write and the crash cuts —
//! is in `registration_durability.rs`, because those need a real filesystem and
//! real threads rather than a deterministic double.

use grapt_service::{
    AttachmentRefusal, ControlRefusal, ElectedControllerAuthority, EnsureOutcome, Observation,
    ProbeOutcome, RegistrationRecord, ReplacementReason, Retirement, ServiceController, SlotState,
    StopOutcome, UpgradeTransitionRefusal,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ReplacementOutcome, ServiceGeneration,
    ServiceInstanceId, ServiceRegistrationVersion, ServiceSlotId,
    UpgradeRefusal as VersionUpgradeRefusal, VersionOrderError,
};

type R = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

const SLOT: &str = "slot-secode";
const ENVIRONMENT: &str = "env-alpha";
const VERSION: &str = "0.1.0";

fn slot() -> Option<ServiceSlotId> {
    ServiceSlotId::new(SLOT)
}

fn environment() -> Option<EnvironmentReceipt> {
    EnvironmentReceipt::new(ENVIRONMENT)
}

fn version() -> Option<ServiceRegistrationVersion> {
    ServiceRegistrationVersion::new(VERSION)
}

/// An owner receipt with every coordinate nameable, so a test can vary exactly
/// one of them and hold the rest fixed.
fn owner(instance: &str, generation: u64, pid: u32, port: u16) -> Option<OwnerReceipt> {
    owner_in(instance, generation, pid, port, SLOT, ENVIRONMENT)
}

fn owner_in(
    instance: &str,
    generation: u64,
    pid: u32,
    port: u16,
    slot: &str,
    environment: &str,
) -> Option<OwnerReceipt> {
    Some(OwnerReceipt::new(
        ServiceSlotId::new(slot)?,
        ServiceInstanceId::new(instance)?,
        EnvironmentReceipt::new(environment)?,
        ServiceGeneration::new(generation),
        ProcessId::new(pid)?,
        Endpoint::new(format!("http://127.0.0.1:{port}"))?,
    ))
}

/// A candidate whose probe attributes exactly itself: the ordinary healthy case.
fn attributed(declared: OwnerReceipt) -> Option<Observation> {
    Some(Observation::new(
        declared.clone(),
        ProbeOutcome::Attributed(declared),
        version()?,
    ))
}

fn attributed_version(declared: OwnerReceipt, raw: &str) -> Option<Observation> {
    Some(Observation::new(
        declared.clone(),
        ProbeOutcome::Attributed(declared),
        ServiceRegistrationVersion::new(raw)?,
    ))
}

fn record(owner: OwnerReceipt, at: i64) -> Option<RegistrationRecord> {
    RegistrationRecord::new(owner, version()?, Timestamp::from_millis(at)).ok()
}

fn record_version(owner: OwnerReceipt, raw: &str, at: i64) -> Option<RegistrationRecord> {
    RegistrationRecord::new(
        owner,
        ServiceRegistrationVersion::new(raw)?,
        Timestamp::from_millis(at),
    )
    .ok()
}

fn store_holding_version(
    owner: OwnerReceipt,
    raw: &str,
) -> Result<ElectedControllerAuthority, Box<dyn std::error::Error>> {
    let store = ElectedControllerAuthority::isolated();
    let record = record_version(owner, raw, 1_000).ok_or("incumbent record")?;
    store.seed_isolated(Some(record.encode()))?;
    Ok(store)
}

/// A store already holding `owner` as the registered incumbent.
fn store_holding(
    owner: OwnerReceipt,
) -> Result<ElectedControllerAuthority, Box<dyn std::error::Error>> {
    let store = ElectedControllerAuthority::isolated();
    let record = record(owner, 1_000).ok_or("incumbent record")?;
    store.seed_isolated(Some(record.encode()))?;
    Ok(store)
}

fn controller<'a>(
    store: &'a ElectedControllerAuthority,
    clock: &'a FixedClock,
) -> Result<ServiceController<'a>, Box<dyn std::error::Error>> {
    Ok(ServiceController::new(
        slot().ok_or("slot")?,
        environment().ok_or("environment")?,
        store,
        clock,
    ))
}

/// The registered owner, read back through a fresh discovery.
fn registered(
    controller: &ServiceController<'_>,
) -> Result<Option<OwnerReceipt>, Box<dyn std::error::Error>> {
    Ok(controller.discover()?.registered_owner().cloned())
}

include!("controller_contract/switching_commit_and_races.rs");
include!("controller_contract/candidate_identity_and_preconditions.rs");
include!("controller_contract/stop_reclaim_and_store_failure.rs");
include!("controller_contract/refusal_vocabulary_and_durable_binding.rs");
include!("controller_contract/semantic_version_upgrades.rs");
