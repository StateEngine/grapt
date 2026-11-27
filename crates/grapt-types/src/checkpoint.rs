//! External workspace-checkpoint identity.
//!
//! Law (amendment §8.5, survey delta §2.1): the ambiguous generic
//! `CheckpointId` is deleted. A workspace checkpoint is **not** a Git
//! commit/tree, a Session fork, a context compaction, a durable event-log
//! snapshot or an ordinary output artifact; each keeps its own identity and
//! lifecycle. These types are refs into an external provider — Grapt stores
//! exact identity, version and bounded receipts, never the provider's
//! content-addressed objects.
//!
//! Capture always settles as [`CheckpointCaptureOutcome::Captured`],
//! `Skipped(reason)` or `Refused(reason)`: a skipped or degraded capture is
//! visible policy evidence and can never be inferred from a missing reference
//! or presented as exact recovery coverage.
//!
//! # The governed restore application boundary
//!
//! The external workspace-checkpoint lifecycle is:
//!
//! ```text
//! RestorePlanRequested { operation, checkpoint_ref, root, lanes }
//!   --> Planned { plan_id, exact consequences, expiry }
//!   -- exact capability/permission settlement --> Authorized
//!   -- provider rescue capture ---------------> RescueProven
//!   --> Applying { external_effect = AmbiguousUntilSettled }
//!   --> Applied { receipt, recovery_anchor? }
//!   --> Interrupted/Ambiguous -- inspect/reconcile --> Applied | Refused | Quarantined
//! ```
//!
//! "The restore plan binds provider/version, checkpoint, root identity,
//! selected lanes, writes/deletes/bytes, unusual conditions and provider
//! preconditions. **Applying a different plan is unnameable.** A destructive
//! provider operation needs explicit authority even if capture/inspect is
//! policy-automatic."
//!
//! Amendment §8.3: "Every mutation or external effect crosses one governed
//! entry carrying a non-clone authority lease through effect completion. Every
//! terminal class commits a typed governed receipt before the owning attempt is
//! released. Alternate mutation entry points that bypass policy, sandbox,
//! verification, or receipt are forbidden."
//!
//! Amendment §8.5: "Provider-supported rescue occurs before the first
//! destructive write and is included in the receipt." and "An agent cannot
//! auto-confirm a heavy restore merely because it may capture or inspect
//! checkpoints."
//!
//! [`SettledRestorePlan::apply`] is that one entry. It is the only public item
//! in this crate that authorizes a destructive restore; it consumes the settled
//! plan by value, a [`CheckpointRestoreAuthorityLease`] by value and a plan-bound
//! [`PlanPreconditionObservation`], and it either commits a
//! [`RestoreRefusalReceipt`] or yields an opaque
//! [`RestoreApplicationAdmission`].
//!
//! It does NOT claim the effect happened. A types crate cannot observe a
//! provider, so there is no `Applied` mint here at all — see
//! [`RestoreApplicationAdmission`] for what is deferred to the provider owner.

use crate::recovery::RecoveryAnchor;

#[cfg(test)]
use crate::authority::{DeterministicAuthorities, Timestamp};
#[cfg(doc)]
#[allow(unused_imports)]
use crate::capability::CheckpointRestoreAuthorityLease;
#[cfg(test)]
use crate::capability::{
    CheckpointRestoreAuthorityLease, CheckpointRestoreCapabilityId, RealizedEnforcement,
};
#[cfg(test)]
use crate::recovery::{RecoveryAnchorId, WorktreeIdentity};
#[cfg(test)]
use crate::terminality::ExternalEffectState;

mod restore_admission;
mod restore_plan;

pub use restore_admission::{
    PlanPreconditionObservation, RestoreAdmission, RestoreApplicationAdmission,
    RestoreRefusalReason, RestoreRefusalReceipt,
};
#[doc(hidden)]
pub use restore_plan::SettledRestorePlanParts;
pub use restore_plan::{
    RestoreConsequences, RestorePrecondition, RestoreRescue, RestoreRescueEvidence,
    SettledRestorePlan, UnusualRestoreCondition,
};

crate::id::opaque_id! {
    /// The external checkpoint provider's identity.
    CheckpointProviderId
}

crate::id::opaque_id! {
    /// The provider's exact contract/schema version.
    CheckpointProviderVersion
}

crate::id::opaque_id! {
    /// An immutable checkpoint record inside a provider.
    WorkspaceCheckpointId
}

crate::id::opaque_id! {
    /// A movable named head inside a provider.
    ///
    /// Distinct from [`WorkspaceCheckpointId`]: inspecting a movable head is
    /// not lineage, and a head is not an immutable record.
    CheckpointHeadId
}

crate::id::opaque_id! {
    /// A negotiated typed lane within a provider.
    CheckpointLaneId
}

crate::id::opaque_id! {
    /// One governed capture/inspect/restore operation.
    CheckpointOperationId
}

crate::id::opaque_id! {
    /// A pure restore plan, distinct from its authorized application.
    CheckpointRestorePlanId
}

/// A fully qualified reference to an immutable checkpoint record.
///
/// Provider identity and version travel with the checkpoint because the same
/// opaque id under a different provider or contract version is not the same
/// checkpoint. Ownership probe #12 depends on this being its own nominal type:
/// a Git commit, artifact, compaction or Session-fork identity cannot be
/// passed where this is required.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkspaceCheckpointRef {
    provider: CheckpointProviderId,
    version: CheckpointProviderVersion,
    checkpoint: WorkspaceCheckpointId,
}

impl WorkspaceCheckpointRef {
    pub fn new(
        provider: CheckpointProviderId,
        version: CheckpointProviderVersion,
        checkpoint: WorkspaceCheckpointId,
    ) -> Self {
        Self {
            provider,
            version,
            checkpoint,
        }
    }

    pub fn provider(&self) -> &CheckpointProviderId {
        &self.provider
    }

    pub fn version(&self) -> &CheckpointProviderVersion {
        &self.version
    }

    pub fn checkpoint(&self) -> &WorkspaceCheckpointId {
        &self.checkpoint
    }
}

/// Why a capture could not represent every requested dimension exactly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FidelityLoss {
    /// A requested lane is not supported by this provider version.
    LaneUnsupported { lane: CheckpointLaneId },
    /// Content was excluded by policy (for example secret redaction).
    PolicyExcluded,
    /// The provider truncated content at its own bound.
    ProviderTruncated,
}

/// How faithfully a capture represents the workspace.
///
/// `Degraded` always names its reason, so degraded coverage cannot be reported
/// as exact recovery coverage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CheckpointFidelity {
    Exact,
    Degraded { reason: FidelityLoss },
}

impl CheckpointFidelity {
    pub fn is_exact(&self) -> bool {
        matches!(self, CheckpointFidelity::Exact)
    }
}

/// Exact provider-side ancestry.
///
/// Law: provider fork/head creation records its exact parent reference.
/// Ancestry is never inferred from a movable head later.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CheckpointLineage {
    /// The first record on a line.
    Root,
    /// Created from an exact parent checkpoint on a named head.
    ForkedFrom {
        checkpoint: WorkspaceCheckpointId,
        head: CheckpointHeadId,
    },
}

/// Why a capture was not taken.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaptureSkipReason {
    /// Policy declined capture for this operation.
    PolicyDeclined,
    /// The provider does not support capture for this root.
    Unsupported,
    /// Nothing changed since the previous capture on this line.
    NoChanges,
}

/// Why a capture was refused.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaptureRefusalReason {
    /// Git HEAD moved between the before and after observations, so no single
    /// truthful baseline can be assigned.
    HeadChangedDuringCapture,
    /// The requested root is not the authorized root.
    WrongRoot,
    /// A requested lane was never negotiated.
    UnknownLane { lane: CheckpointLaneId },
    /// The provider was unavailable.
    ProviderUnavailable,
    /// The root is a Git worktree whose branch has no commit to pin.
    UnbornGitRoot,
    /// The VCS observation was unavailable; this is distinct from a non-Git
    /// root and from a provider failure.
    VcsUnavailable {
        reason: crate::vcs::VcsUnavailableReason,
    },
    /// The root resolved to a different worktree identity after capture.
    WorktreeChangedDuringCapture,
}

/// How one capture settled.
///
/// Three closed outcomes and no fourth "absent" state: absence of a reference
/// can never masquerade as a successful checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointCaptureOutcome {
    /// Captured. The anchor is the SINGLE source of the checkpoint reference
    /// and the fidelity, so an outcome cannot pair checkpoint A with anchor B
    /// or claim `Exact` over a degraded anchor.
    ///
    /// Boxed: the anchor dominates the enum's size and a skipped or refused
    /// capture should not pay for it.
    Captured {
        anchor: Box<RecoveryAnchor>,
        lineage: CheckpointLineage,
    },
    /// Deliberately not captured, with the reason recorded as evidence.
    Skipped { reason: CaptureSkipReason },
    /// Capture was attempted and refused.
    Refused { reason: CaptureRefusalReason },
}

impl CheckpointCaptureOutcome {
    /// The checkpoint this outcome produced, if any.
    pub fn checkpoint(&self) -> Option<&WorkspaceCheckpointRef> {
        match self {
            CheckpointCaptureOutcome::Captured { anchor, .. } => Some(anchor.checkpoint()),
            CheckpointCaptureOutcome::Skipped { .. } | CheckpointCaptureOutcome::Refused { .. } => {
                None
            }
        }
    }

    /// Whether this outcome proves exact recovery coverage.
    ///
    /// A skipped, refused or degraded capture does not, which is what lets a
    /// required-checkpoint policy block a dangerous mutation path.
    pub fn proves_exact_coverage(&self) -> bool {
        match self {
            CheckpointCaptureOutcome::Captured { anchor, .. } => anchor.fidelity().is_exact(),
            CheckpointCaptureOutcome::Skipped { .. } | CheckpointCaptureOutcome::Refused { .. } => {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod admission_and_preconditions;
    mod capture_and_reference;
    mod restore_plan_rescue_and_lease;
}
