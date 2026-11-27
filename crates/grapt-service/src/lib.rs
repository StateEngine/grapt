//! The retained-desktop attachment receipt (R4D-1) and the differential driver
//! contract built on it (R4D-2A).
//!
//! # What this crate owns
//!
//! [`EngineSelection`] — which engine is selected — and [`SelectionReceipt`],
//! the fail-closed fact that an engine was actually attached, admitted from an
//! owner identity rather than from liveness. [`SelectorState`] carries the
//! reversibility of the switch, and [`AdmittedDifferential`] is the only way to
//! claim that two receipts compare two genuinely different engines.
//!
//! On top of those, [`DifferentialPlan`] is the contract a differential driver
//! must satisfy before it may claim it compared two engines: the four
//! [`DifferentialMatrix`] rows and their pinned minimum [`OperationSubset`]s,
//! the [`RestartBoundary`] and [`ResumeReadmission`] identity laws, the bounded
//! [`LongSessionBound`], and [`RecordedHostBounds`] — the host's eight resource
//! bounds as recorded facts rather than asserted ones.
//!
//! [`DifferentialRefusal`] carries R4D-1's [`AttachmentRefusal`] verbatim for
//! every attachment-level law rather than re-spelling one, so this crate holds
//! exactly one vocabulary for each fact it can refuse.
//!
//! # The elected service controller (R4D-2C)
//!
//! [`ServiceController`] is the single owner of one discovery-registration
//! slot. [`RegistrationRecord`] is its canonical byte contract, and
//! [`FileRegistrationStore`] keeps read/compare/write under one OS file lock.
//! Ensure, stop, replace, rollback, and upgrade have distinct typed results:
//! [`EnsureOutcome`], [`StopOutcome`], [`SwitchReceipt`], [`RollbackReceipt`],
//! and [`UpgradeReceipt`]. A prepared candidate is consumed by commit, so the
//! forbidden rollback-after-commit path does not compile.
//!
//! # What this crate deliberately does NOT own
//!
//! It starts no process, owns no listener, serves no request, drives no desktop,
//! touches no oracle code, and reads no clock of its own — every timestamp
//! arrives through an injected [`grapt_types::authority::ClockAuthority`].
//! IA0-1R adds one bounded authenticated outbound health probe; successful
//! attribution still requires exact pid/build/environment/instance agreement.
//!
//! The install-prefix adapter owns only a kernel exclusion guard, independent
//! of election; installation manifests, version pointers and history belong
//! to the CLI. Controller registration is the persistence surface owned here. An
//! attachment receipt remains a value: proving that a selection survives a
//! restart on disk still belongs to whichever owner writes that receipt down.
//! The controller's file store proves only canonical registration durability
//! and compare-and-swap ownership — nothing about a live service.
//!
//! The differential half adds no exception to any of that. A [`DifferentialPlan`]
//! is the claim that a driver *may* run a matrix, never that it did: no engine
//! is started, contacted or measured anywhere in this crate, and the host's
//! recorded bounds are carried as opaque renderings this crate never parses.
//! `grapt-api` is not a dependency and its `ApiHostLimits` is neither imported
//! nor re-declared here.
//!
//! # The law it exists to hold
//!
//! The frozen desktop's `checkHealth` accepts any 2xx, so liveness and identity
//! are different facts. Attachment is therefore admitted from
//! `{ServiceSlotId, ServiceInstanceId, EnvironmentReceipt}`, with PID and
//! endpoint as attributes — a reused PID never inherits ownership, and the same
//! owner keeps its slot across a new PID or ephemeral port.

mod bounds;
mod controller;
mod differential;
pub mod installation;
pub mod launcher_environment;
mod receipt;
mod registration;
mod selection;
mod store;

pub use bounds::{BoundProvenance, HostBoundName, RecordedBound, RecordedHostBounds};
pub use controller::{
    ControlRefusal, Discovered, EnsureOutcome, Observation, PreparedCandidate, PreparedUpgrade,
    Provenance, ReplacementReason, Retirement, RollbackReceipt, ServiceController, SlotState,
    StopOutcome, SwitchReceipt, UpgradeReceipt, UpgradeTransitionRefusal,
};
pub use differential::{
    DifferentialMatrix, DifferentialPlan, DifferentialRefusal, LongSessionBound, MatrixEvidence,
    Operation, OperationSubset, RestartBoundary, ResumeReadmission,
};
pub use receipt::{
    AdmittedDifferential, AttachmentRefusal, ProbeOutcome, SelectionReceipt, SelectorState,
};
pub use registration::{
    RegistrationBuildError, RegistrationDecodeError, RegistrationRecord, MAX_REGISTRATION_BYTES,
    MAX_REGISTRATION_FIELD_BYTES,
};
pub use selection::EngineSelection;
pub use store::{
    CasOutcome, ElectedControllerAuthority, FileRegistrationStore, LockAttemptPolicy,
    PostCommitUnlockWarning, RegistrationMutationPermit, RegistrationStore, StoreError,
};

// ---------------------------------------------------------------------------
// R4D-2E — the pre-entrypoint operator contract.
//
// Appended rather than folded into the declarations above so that every byte of
// the reviewed R4D-1 and R4D-2A surface is unchanged: this lane's candidate adds
// modules and adds nothing else, and a reviewer can confirm that from the diff
// shape alone. The crate doc above is deliberately left byte-identical for the
// same reason.
//
// What this half owns: the evidence tier a live run would execute under and the
// authorization it does not have (`tier`); artifact custody, the run's own
// timeouts and PID-scoped cleanup (`custody`); the retained engine's owner
// identity as a declared mapping and the scope a comparison may have
// (`attribution`); and the operator plan, its phase gate, its mandatory negative
// controls and the loud refusal to construct a `DifferentialPlan` before a host
// has reported its bounds (`operator`).
//
// It opens no socket, starts no process, drives no desktop, reads no clock and
// touches no filesystem — the same boundary the two lanes above hold, extended
// forward rather than relaxed.
// ---------------------------------------------------------------------------

mod attribution;
mod authenticated_health_probe;
mod custody;
mod database_transition;
mod operator;
mod tier;

pub use attribution::{
    CommonStoreProof, ComparisonScope, MappingBasis, MappingReceipt, OwnerCoordinate,
    RetainedOwnerMapping, RetainedSource, ShapeObservation,
};
pub use authenticated_health_probe::{
    AuthenticatedHealthProbe, HealthExpectation, HealthProbeRefusal,
};
pub use custody::{
    ArtifactClass, ArtifactCustody, CapThatFired, CleanupInventory, DeclaredArtifact,
    EngineProcessCount, KillDiscipline, MatrixOutcome, ObservedPath, ReceiptRoot, TimeoutBudget,
};
pub use database_transition::{
    prepare_database_transition, DatabaseTransitionRefusal, QuiescedSource,
};
pub use operator::{
    construct_differential_plan, BoundaryKind, ComponentReceipt, DifferentialConstruction,
    EntrypointCapability, GateConclusion, HostBoundsSource, NegativeControl, OperatorPlan,
    OperatorPlanParts, OperatorRefusal, PhaseGate, RunPhase,
};
pub use tier::{AttendedWindowReceipt, EvidenceTier, ParkReason, RunAuthorization, TierCaps};
