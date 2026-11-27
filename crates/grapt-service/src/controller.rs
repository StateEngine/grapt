//! The elected service controller: the only writer of a discovery registration.
//!
//! # The laws this module holds
//!
//! Master §"Service replacement lifecycle" states seven, and each one is a
//! named refusal or a distinct outcome below:
//!
//! | law | where it lives here |
//! | --- | --- |
//! | candidate has a private registration, independent port, authenticated health check, matching instance PID and environment receipt **before** switch | [`ServiceController::prepare`], five separate refusals |
//! | registration commit is CAS against registration-file **contents** | [`ServiceController::commit`] over the [`RegistrationStore`](crate::RegistrationStore) |
//! | pre-commit candidate failure rolls back only candidate-owned state; incumbent remains owner | [`PreparedCandidate::rollback`], which touches the slot **not at all** |
//! | post-commit retire failure is success **with warning**, not rollback failure | [`Retirement`] → `ReplacedRetirePending` |
//! | owner identity is `{ServiceSlotId, ServiceInstanceId, EnvironmentReceipt}`, never `{pid, url}` | inherited whole from [`OwnerReceipt`]; this crate mints no identity |
//! | ensure, stop, replace, rollback and upgrade return **distinct** typed outcomes | [`EnsureOutcome`], [`StopOutcome`], [`SwitchReceipt`], [`RollbackReceipt`], [`UpgradeReceipt`] |
//! | only the elected controller may mutate discovery registration | every mutation is behind [`ServiceController`] and the store's lock |
//!
//! # Why rollback is total, and unnameable after a commit
//!
//! The master is emphatic that these two are not the same fact: *"Pre-commit
//! candidate failure rolls back only candidate-owned files/process; incumbent
//! remains owner"*, but *"Switched — candidate later fails — FailedAfterCommit
//! { current registration identifies candidate }"*, and *"No caller … prints a
//! rollback message after the candidate became current."*
//!
//! So the distinction is enforced by the type system rather than by discipline.
//! [`PreparedCandidate`] is the only thing that can be rolled back, and
//! [`ServiceController::commit`] **consumes** it. After a commit there is no
//! value left to call `rollback` on, so the forbidden message is not something
//! a caller must remember not to print — it is something that does not compile.
//! `tests/probes/01_rollback_after_commit.rs` is the compile-fail witness, and
//! it fails with `E0382: use of moved value` — a borrow-check failure rather
//! than a name-resolution one, which is what proves the seal instead of a typo.
//!
//! And because nothing was committed, rollback is *total*: it returns a receipt
//! rather than a `Result`, and it performs no store operation whatsoever. That
//! is not an optimisation — a rollback that wrote anything to the slot could
//! fail, and a failing rollback would leave the incumbent's ownership in doubt
//! exactly when the law says it is untouched.
//!
//! # Why the controller mints the generation
//!
//! A candidate declares who it is; it does not get to declare *which
//! ownership* it holds. `ServiceGeneration` orders ownership transfers, so a
//! candidate that could choose its own could name a generation that already
//! passed and make a stale claim look current. The controller therefore
//! computes the successor from the discovered incumbent and mints the
//! [`OwnerReceipt`] that is actually written.
//!
//! A candidate that declared a different generation is **refused**, not
//! silently corrected. Silent correction would take a candidate that believes
//! it is generation 5 of a slot standing at 2 — which means it has read
//! something stale, or is meant for another slot entirely — and quietly write
//! it in as generation 3.
//!
//! # What this module still does not do
//!
//! It starts no process, opens no socket, sends no request and kills nothing.
//! Health, liveness and retirement are *observations supplied by the caller*,
//! carried in [`ProbeOutcome`] — R4D-1's vocabulary, reused rather than
//! re-spelled — and in [`Retirement`]. That boundary is what keeps
//! `grapt-service`'s landed dependency-direction gate truthful: the crate that
//! owns the registration must not be the crate that owns a listener.
#[expect(
    unused_imports,
    reason = "the accepted module docs bind the landed probe vocabulary link"
)]
use crate::receipt::ProbeOutcome;
#[expect(
    unused_imports,
    reason = "the accepted module docs bind these dependency-owned vocabulary links"
)]
use grapt_types::service::{OwnerReceipt, ServiceGeneration};

mod discovery;
mod refusal;
mod registration;
mod transition;

pub use discovery::{Discovered, Observation, SlotState};
pub use refusal::{ControlRefusal, UpgradeTransitionRefusal};
pub use registration::{
    PreparedCandidate, PreparedUpgrade, RollbackReceipt, ServiceController, SwitchReceipt,
    UpgradeReceipt,
};
pub use transition::{EnsureOutcome, Provenance, ReplacementReason, Retirement, StopOutcome};
