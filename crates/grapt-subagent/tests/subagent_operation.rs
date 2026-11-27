//! R3-g witnesses (commission + review HOLD): the durable
//! subagent operation domain. Full lifecycle (Admitted -> Running(generation)
//! -> DomainTerminal | ExternallyAmbiguous, reconciled, never auto-replayed);
//! a tagged state/delivery union enforced by CHECK constraints and strict
//! decode; a typed claim bound to its exact route/parent/input with mode
//! enforcement; a service-derived delivery payload; and decode-before-mutate
//! recovery. Identity is SubagentOperationId, distinct from the child SessionId.

use grapt_artifact::{
    ArtifactKind, ArtifactStore, CaptureBound, FilesystemArtifactStore, Sensitivity,
};
use grapt_inbox::{InboxEvent, InboxFold};
use grapt_store::{AggregateId, EventStore};
use grapt_subagent::{
    ArtifactBackedOutput, ClaimOutcome, ClaimRoute, DeliveryClaim, DeliveryLimits,
    DeliverySettlement, DeliveryStatus, OperationStatus, SubagentAdmission, SubagentCancelReason,
    SubagentError, SubagentMode, SubagentService, SubagentTerminal,
};
use grapt_types::{
    AgentId, AmbiguityEvidence, CasOutcome, ClockAuthority, DeliveryClaimId, EffectiveGraphId,
    EventId, MessageId, OperationId, RuntimeGeneration, SessionId, SubagentOperationId, Timestamp,
};
use std::sync::atomic::{AtomicU64, Ordering};
#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[path = "subagent_operation/child_listing.rs"]
mod child_listing;

fn op(s: &str) -> SubagentOperationId {
    SubagentOperationId::new(s).unwrap_or_else(|| panic!("operation id {s:?}"))
}
fn ses(s: &str) -> SessionId {
    s.parse()
        .unwrap_or_else(|e| panic!("session id {s:?}: {e}"))
}
fn agent(s: &str) -> AgentId {
    AgentId::new(s).unwrap_or_else(|| panic!("agent id"))
}
fn msg(s: &str) -> MessageId {
    s.parse()
        .unwrap_or_else(|e| panic!("message id {s:?}: {e}"))
}
fn ev(s: &str) -> EventId {
    s.parse().unwrap_or_else(|e| panic!("event id {s:?}: {e}"))
}
fn eg(s: &str) -> EffectiveGraphId {
    EffectiveGraphId::new(s).unwrap_or_else(|| panic!("effective graph id"))
}
fn claim_id(s: &str) -> DeliveryClaimId {
    DeliveryClaimId::new(s).unwrap_or_else(|| panic!("claim id"))
}

fn operation_id(s: &str) -> OperationId {
    OperationId::new(format!("op_{s}")).unwrap_or_else(|| panic!("operation family id"))
}

#[derive(Debug)]
struct FrozenClock;

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1)
    }
}

fn completed(text: &str) -> Result<SubagentTerminal, Box<dyn std::error::Error>> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "grapt-r3-delivery-{}-{ordinal}",
        std::process::id()
    ));
    let store = FilesystemArtifactStore::open(root, b"test-key".to_vec(), Box::new(FrozenClock))?;
    let bound = CaptureBound::new(4096, 64).ok_or("capture bound")?;
    let captured = store.capture(
        std::io::Cursor::new(text.as_bytes()),
        bound,
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    )?;
    let output = ArtifactBackedOutput::verify(
        &store,
        captured.reference().clone(),
        captured.disposition(),
        text.to_owned(),
        4096,
    )?;
    Ok(SubagentTerminal::Completed { output })
}

fn admission(operation: &str, child: &str, parent: &str, mode: SubagentMode) -> SubagentAdmission {
    SubagentAdmission {
        operation: operation_id(operation),
        subagent: op(operation),
        child: ses(child),
        parent: ses(parent),
        agent: agent("test"),
        model: None,
        description: "background op".into(),
        mode,
        tool_call: None,
        effective_graph: eg("efg_test"),
    }
}

fn synthetic_claim(id: &str, parent: &str, input: &str) -> DeliveryClaim {
    DeliveryClaim {
        id: claim_id(id),
        route: ClaimRoute::OperationDelivery,
        parent: ses(parent),
        input: msg(input),
        generation: RuntimeGeneration::new(1),
    }
}

/// A service whose store has already been elected writer for every parent
/// session the test may deliver into. Election happens here, on the bare store
/// before the service takes it, because the append path never elects on a
/// caller's behalf — an unelected writer is refused with `NotElected` rather
/// than silently taking the aggregate — and the service exposes its store
/// immutably, so this is the only place a test can elect.
fn service_for(parents: &[&str]) -> Result<SubagentService, Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    for raw in parents {
        store.elect_writer(&AggregateId::Session(raw.parse()?))?;
    }
    let limits = DeliveryLimits::new(64, 256 * 1024).ok_or("delivery limits")?;
    Ok(SubagentService::new(store, limits)?)
}

/// Open + start a background operation running under generation 1.
fn open_running(svc: &mut SubagentService, id: &str, child: &str, parent: &str) -> TestResult {
    svc.open(&admission(id, child, parent, SubagentMode::Background), 10)?;
    assert_eq!(
        svc.start(&op(id), RuntimeGeneration::new(1), 11)?,
        CasOutcome::Applied
    );
    Ok(())
}

fn parent_admissions(
    svc: &SubagentService,
    parent: &str,
) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(svc.store().connection().query_row(
        "SELECT COUNT(*) FROM session_operation_delivery_pending WHERE session_id = ?1",
        [parent],
        |r| r.get(0),
    )?)
}

// ---- lifecycle --------------------------------------------------------------

include!("subagent_operation/lifecycle_claim.rs");

include!("subagent_operation/delivery_persistence.rs");

include!("subagent_operation/recovery_concurrency.rs");

include!("subagent_operation/child_context.rs");

#[path = "subagent_operation/durable_delivery_r3.rs"]
mod durable_delivery_r3;

#[path = "subagent_operation/mail_presentation.rs"]
mod mail_presentation;
