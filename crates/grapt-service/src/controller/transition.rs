use grapt_types::service::{OwnerReceipt, ReplacementOutcome, ServiceRegistrationVersion};

use crate::registration::RegistrationDecodeError;
use crate::store::PostCommitUnlockWarning;

#[expect(
    unused_imports,
    reason = "the accepted responsibility docs bind controller and generation links"
)]
use super::registration::ServiceController;
#[expect(
    unused_imports,
    reason = "the accepted responsibility docs bind the first-generation link"
)]
use grapt_types::service::ServiceGeneration;
/// Why the incumbent cannot simply be reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplacementReason {
    /// Nothing holds the slot.
    SlotEmpty,
    /// The slot holds bytes that decode to no record. Recovery is
    /// [`ServiceController::reclaim`], which is deliberately a separate,
    /// explicitly named transition.
    UnreadableRegistration(RegistrationDecodeError),
    /// The registration names an owner the probe did not attribute — the
    /// registration outlived its writer.
    StaleRegistration { registered: OwnerReceipt },
    /// The incumbent is live and is who it says it is, but is not the version
    /// asked for.
    IncompatibleVersion {
        registered: ServiceRegistrationVersion,
        required: ServiceRegistrationVersion,
    },
    /// The authenticated probe reported a version that does not agree with
    /// both the durable registration and the caller's requested version.
    ObservedVersionMismatch {
        registered: ServiceRegistrationVersion,
        observed: ServiceRegistrationVersion,
        required: ServiceRegistrationVersion,
    },
}

/// The outcome of `ensure`.
///
/// Distinct from every other transition's outcome (master law 6). `ensure`
/// answers one question — may the incumbent stand? — and deliberately cannot
/// answer "I started something", because this crate starts nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// A healthy, attributed, compatible incumbent holds the slot.
    ReusedIncumbent { owner: OwnerReceipt },
    /// The incumbent cannot stand, for exactly this reason.
    ReplacementRequired { reason: ReplacementReason },
}

impl EnsureOutcome {
    /// The landed classification for this outcome.
    ///
    /// `Reused` is the only [`ReplacementOutcome`] `ensure` can produce; the
    /// replacement path has not been taken, so no other variant would be true.
    pub fn reused(&self) -> Option<ReplacementOutcome> {
        match self {
            EnsureOutcome::ReusedIncumbent { .. } => Some(ReplacementOutcome::Reused),
            EnsureOutcome::ReplacementRequired { .. } => None,
        }
    }
}

/// The outcome of `stop`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// The slot held this owner and now holds nothing.
    Released {
        owner: OwnerReceipt,
        unlock_warning: Option<PostCommitUnlockWarning>,
    },
    /// There was nothing to release.
    SlotAlreadyEmpty,
    /// The slot is held by somebody else. Releasing it would be one process
    /// evicting another's registration, which master law 7 reserves to the
    /// elected controller of *that* ownership.
    NotTheOwner { registered: OwnerReceipt },
    /// The contents changed between the discovery read and the compare.
    LostRace { observed: Option<Vec<u8>> },
}

/// Whether the incumbent actually went away after the switch committed.
///
/// Supplied by the caller because this crate does not stop processes. The
/// distinction it carries is master law 4: a retirement that failed **after**
/// the commit is a success with a warning, never a rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retirement {
    /// There was no incumbent to retire.
    NoIncumbent,
    /// The incumbent retired.
    Retired,
    /// The incumbent did not retire within the caller's bound.
    TimedOut,
    /// The incumbent could not be retired.
    Failed { reason: String },
}

/// How a prepared candidate came to be entitled to the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// The slot held nothing.
    EmptySlot,
    /// The candidate succeeds this owner.
    Succeeding { incumbent: OwnerReceipt },
    /// The slot held bytes that decoded to no record, so the prior generation
    /// is not knowable and ownership restarts at
    /// [`ServiceGeneration::FIRST`]. Recorded rather than hidden: a generation
    /// that restarts must say why it restarted.
    Reclaimed { because: RegistrationDecodeError },
}
