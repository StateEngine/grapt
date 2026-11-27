use grapt_types::service::{ServiceRegistrationVersion, UpgradeRefusal as VersionUpgradeRefusal};
use thiserror::Error;

use crate::receipt::AttachmentRefusal;
use crate::registration::{RegistrationBuildError, RegistrationDecodeError};
use crate::store::StoreError;
/// Why a control operation was refused.
///
/// Attachment-level laws are carried through R4D-1's [`AttachmentRefusal`]
/// verbatim rather than re-spelled, so this crate holds exactly one vocabulary
/// per fact — the same rule `DifferentialRefusal` follows.
#[derive(Debug, Error)]
pub enum ControlRefusal {
    /// An R4D-1 attachment law. `Unattributed` here means the candidate's probe
    /// proved liveness and nothing else.
    #[error(transparent)]
    Attachment(#[from] AttachmentRefusal),
    /// The probe attributed a different owner than the candidate declared.
    #[error(
        "the candidate declares owner instance {declared} but its probe attributed {attributed}: \
         the health check reached something other than the candidate"
    )]
    ProbeAttributedAnotherOwner {
        declared: String,
        attributed: String,
    },
    /// The attributed instance is running under a different PID than declared.
    ///
    /// Separate from the identity comparison above and not implied by it:
    /// `OwnerReceipt::is_same_owner` deliberately excludes the PID, so identity
    /// can match while the process does not. Master law 1 requires a *matching
    /// instance PID* alongside identity, and this is that check.
    #[error(
        "the candidate declares PID {declared} but its probe attributed PID {attributed}: owner \
         identity matched while the process did not, so the declaration describes a different \
         process than the one that answered"
    )]
    PidDoesNotMatchTheProbe { declared: u32, attributed: u32 },
    /// Health attribution must describe the endpoint that was actually
    /// declared and probed, not merely the same logical owner.
    #[error(
        "the candidate declares endpoint {declared} but its authenticated probe attributed endpoint {attributed}"
    )]
    EndpointDoesNotMatchTheProbe {
        declared: String,
        attributed: String,
    },
    /// The candidate is serving on the incumbent's endpoint.
    #[error(
        "the candidate is at {endpoint}, which is the incumbent's endpoint: a candidate must hold \
         an independent port before the switch, or a failed candidate leaves the incumbent's \
         address serving nothing"
    )]
    EndpointNotIndependent { endpoint: String },
    /// The candidate already holds the slot.
    #[error(
        "the candidate is already the registered owner of this slot: a replacement needs a \
         candidate with a private registration, not the incumbent replacing itself"
    )]
    CandidateIsTheIncumbent,
    /// The candidate belongs to another slot.
    #[error(
        "the candidate names slot {declared} but this controller owns slot {expected}: a \
         candidate is never registered into a slot it did not name"
    )]
    NotThisSlot { expected: String, declared: String },
    /// The candidate carries another environment's receipt.
    #[error(
        "the candidate carries environment receipt {declared} but this controller requires \
         {expected}: an environment receipt is checked before the switch, not after it"
    )]
    NotTheExpectedEnvironment { expected: String, declared: String },
    /// A readable durable record belongs to a different slot than this
    /// controller and therefore cannot be acted upon through this authority.
    #[error(
        "the durable registration names slot {registered} but this controller is bound to slot {controller}"
    )]
    DurableSlotMismatch {
        controller: String,
        registered: String,
    },
    /// A readable durable record belongs to a different environment than this
    /// controller and therefore cannot be acted upon through this authority.
    #[error(
        "the durable registration names environment {registered} but this controller is bound to environment {controller}"
    )]
    DurableEnvironmentMismatch {
        controller: String,
        registered: String,
    },
    /// The candidate declared a generation other than the successor.
    #[error(
        "the candidate declares generation {declared} where this slot's next ownership is \
         {required}: a candidate does not choose its own generation, and a wrong one is refused \
         rather than quietly rewritten"
    )]
    GenerationNotSuccessor { declared: u64, required: u64 },
    /// The generation counter cannot advance.
    #[error("this slot's ownership generation is exhausted and cannot advance")]
    GenerationExhausted,
    /// `prepare` was given a slot whose bytes decode to no record.
    #[error(
        "this slot holds bytes that decode to no record ({source}); an ordinary replacement is \
         refused because the prior generation is unknowable, and recovery is the explicitly named \
         reclaim transition"
    )]
    UnreadableRegistration {
        #[source]
        source: RegistrationDecodeError,
    },
    /// `reclaim` was given a slot that is not unreadable.
    #[error(
        "reclaim applies only to a slot whose bytes decode to no record; this slot is readable or \
         empty and has an ordinary replacement path"
    )]
    NothingToReclaim,
    /// A prepared candidate was committed through a controller for another slot.
    #[error(
        "this candidate was prepared for slot {prepared} and cannot be committed by the \
         controller for slot {controller}"
    )]
    PreparedForAnotherSlot {
        prepared: String,
        controller: String,
    },
    /// A post-commit fact was claimed about a receipt that never committed.
    #[error(
        "this switch never committed, so there is no current registration for the candidate to \
         have failed after: a rollback message and a post-commit failure are different facts"
    )]
    NotCommitted,
    #[error(transparent)]
    RegistrationBuild(#[from] RegistrationBuildError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl ControlRefusal {
    /// The stable label used in the corpus projection.
    ///
    /// The `Attachment` arm **delegates** to R4D-1's own label rather than
    /// spelling one of its own, so this lane mints no second word for a landed
    /// law. `no_control_label_collides_with_a_landed_attachment_label` in the
    /// corpus owner holds that mechanically.
    ///
    /// The match is exhaustive over this enum, which is the binding that
    /// matters: a new refusal is a **compile error** here before it can be an
    /// unrecorded label in a projection. The R4D-2A review established why a
    /// hand-written list is not good enough — a new variant with a label and no
    /// corpus row left that lane's whole suite green.
    pub fn label(&self) -> &'static str {
        match self {
            ControlRefusal::Attachment(refusal) => refusal.label(),
            ControlRefusal::ProbeAttributedAnotherOwner { .. } => "probe-attributed-another-owner",
            ControlRefusal::PidDoesNotMatchTheProbe { .. } => "pid-does-not-match-the-probe",
            ControlRefusal::EndpointDoesNotMatchTheProbe { .. } => {
                "endpoint-does-not-match-the-probe"
            }
            ControlRefusal::EndpointNotIndependent { .. } => "endpoint-not-independent",
            ControlRefusal::CandidateIsTheIncumbent => "candidate-is-the-incumbent",
            ControlRefusal::NotThisSlot { .. } => "not-this-slot",
            ControlRefusal::NotTheExpectedEnvironment { .. } => "not-the-expected-environment",
            ControlRefusal::DurableSlotMismatch { .. } => "durable-slot-mismatch",
            ControlRefusal::DurableEnvironmentMismatch { .. } => "durable-environment-mismatch",
            ControlRefusal::GenerationNotSuccessor { .. } => "generation-not-successor",
            ControlRefusal::GenerationExhausted => "generation-exhausted",
            ControlRefusal::UnreadableRegistration { .. } => "unreadable-registration",
            ControlRefusal::NothingToReclaim => "nothing-to-reclaim",
            ControlRefusal::PreparedForAnotherSlot { .. } => "prepared-for-another-slot",
            ControlRefusal::NotCommitted => "not-committed",
            ControlRefusal::RegistrationBuild(_) => "registration-build-refused",
            ControlRefusal::Store(_) => "store-failure",
        }
    }

    /// Every label this lane declares in its own right.
    ///
    /// Deliberately excludes the attachment words: those belong to R4D-1 and
    /// reach a projection through the `Attachment` arm in R4D-1's own spelling.
    /// Listing them here would be this lane claiming a vocabulary it does not
    /// own, and the collision check exists to keep that true.
    pub const OWN_LABELS: &'static [&'static str] = &[
        "probe-attributed-another-owner",
        "pid-does-not-match-the-probe",
        "endpoint-does-not-match-the-probe",
        "endpoint-not-independent",
        "candidate-is-the-incumbent",
        "not-this-slot",
        "not-the-expected-environment",
        "durable-slot-mismatch",
        "durable-environment-mismatch",
        "generation-not-successor",
        "generation-exhausted",
        "unreadable-registration",
        "nothing-to-reclaim",
        "prepared-for-another-slot",
        "not-committed",
        "registration-build-refused",
        "store-failure",
    ];
}

/// Why the upgrade transition was not admitted.
///
/// Version projection failure carries the landed, side-specific
/// [`VersionUpgradeRefusal`]. Equal precedence and downgrade stay separate:
/// build-metadata-only changes are equal under SemVer and can never be
/// mislabeled as an upgrade, while a lower candidate is a named downgrade.
#[derive(Debug, Error)]
pub enum UpgradeTransitionRefusal {
    #[error("upgrade requires a readable incumbent registration; this slot is empty")]
    NoIncumbent,
    #[error(transparent)]
    Version(#[from] VersionUpgradeRefusal),
    #[error(
        "candidate version {candidate} has the same semantic precedence as incumbent {incumbent}: equal precedence is not an upgrade"
    )]
    SamePrecedence {
        incumbent: ServiceRegistrationVersion,
        candidate: ServiceRegistrationVersion,
    },
    #[error(
        "candidate version {candidate} has lower semantic precedence than incumbent {incumbent}: downgrade is refused"
    )]
    Downgrade {
        incumbent: ServiceRegistrationVersion,
        candidate: ServiceRegistrationVersion,
    },
    #[error(transparent)]
    Control(#[from] ControlRefusal),
}

impl UpgradeTransitionRefusal {
    /// Stable corpus/mutation label. Side-specific version failures remain
    /// distinguishable instead of collapsing into a generic parse error.
    pub fn label(&self) -> &'static str {
        match self {
            UpgradeTransitionRefusal::NoIncumbent => "upgrade-no-incumbent",
            UpgradeTransitionRefusal::Version(VersionUpgradeRefusal::Incumbent(_)) => {
                "upgrade-incumbent-version-unorderable"
            }
            UpgradeTransitionRefusal::Version(VersionUpgradeRefusal::Candidate(_)) => {
                "upgrade-candidate-version-unorderable"
            }
            UpgradeTransitionRefusal::SamePrecedence { .. } => "upgrade-same-precedence",
            UpgradeTransitionRefusal::Downgrade { .. } => "upgrade-downgrade",
            UpgradeTransitionRefusal::Control(refusal) => refusal.label(),
        }
    }
}
