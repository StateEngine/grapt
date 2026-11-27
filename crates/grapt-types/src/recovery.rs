//! The Grapt recovery anchor: the binding between an external workspace
//! checkpoint and the exact VCS baseline present when it was captured.
//!
//! Law (amendment §8.5): Git and checkpoints remain independent engines. The
//! binding lives in the Grapt receipt — never in provider objects and never
//! in Git trailers — so neither engine impersonates the other. Restoring
//! checkpoint lanes never moves HEAD, the index or refs; any VCS movement is a
//! separately planned and authorized operation.

use crate::checkpoint::{
    CheckpointFidelity, CheckpointLaneId, CheckpointProviderVersion, WorkspaceCheckpointRef,
};
use crate::path::PathIdentity;
use crate::vcs::VcsBaseline;

crate::id::opaque_id! {
    /// Identity of one recovery anchor.
    RecoveryAnchorId
}

/// Which worktree a capture belongs to.
///
/// Law: authority-constructed from the canonical Location root and, for Git,
/// the resolved common-dir plus worktree git-dir identity. Relinking a
/// worktree mints a different identity, so a checkpoint cannot silently apply
/// to a re-pointed tree. Non-Git roots use their canonical path identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorktreeIdentity {
    /// A Git worktree, identified by root plus its resolved directories.
    Git {
        root: PathIdentity,
        common_dir: PathIdentity,
        git_dir: PathIdentity,
    },
    /// A root that is not a Git worktree.
    Plain { root: PathIdentity },
}

impl WorktreeIdentity {
    pub fn root(&self) -> &PathIdentity {
        match self {
            WorktreeIdentity::Git { root, .. } | WorktreeIdentity::Plain { root } => root,
        }
    }
}

/// Binds an external checkpoint to the workspace truth it was captured
/// against.
///
/// The lane list is the set actually negotiated: an un-negotiated lane is not
/// silently assumed, and unknown or newer provider lanes are preserved by the
/// provider but excluded from restore unless negotiated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAnchor {
    id: RecoveryAnchorId,
    checkpoint: WorkspaceCheckpointRef,
    baseline: VcsBaseline,
    worktree: WorktreeIdentity,
    lanes: Vec<CheckpointLaneId>,
    fidelity: CheckpointFidelity,
}

impl RecoveryAnchor {
    /// Build an anchor from the observed before/after baselines.
    ///
    /// Refuses two combinations the law forbids:
    ///
    /// 1. a Git-backed root that cannot pin an exact HEAD commit; and
    /// 2. a baseline that moved during capture. The harness observes HEAD
    ///    before the provider captures and again after it commits; if those
    ///    differ it cannot assign one truthful baseline, so no anchor exists
    ///    and the caller must retry or refuse.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: RecoveryAnchorId,
        checkpoint: WorkspaceCheckpointRef,
        baseline_before: VcsBaseline,
        baseline_after: VcsBaseline,
        worktree: WorktreeIdentity,
        lanes: Vec<CheckpointLaneId>,
        fidelity: CheckpointFidelity,
    ) -> Option<Self> {
        if baseline_before != baseline_after {
            return None;
        }
        let baseline = baseline_before;
        let git_worktree = matches!(worktree, WorktreeIdentity::Git { .. });
        if git_worktree && !baseline.can_pin_git_capture() {
            return None;
        }
        let mut lanes = lanes;
        lanes.sort();
        lanes.dedup();
        Some(Self {
            id,
            checkpoint,
            baseline,
            worktree,
            lanes,
            fidelity,
        })
    }

    pub fn id(&self) -> &RecoveryAnchorId {
        &self.id
    }

    pub fn checkpoint(&self) -> &WorkspaceCheckpointRef {
        &self.checkpoint
    }

    pub fn baseline(&self) -> &VcsBaseline {
        &self.baseline
    }

    pub fn worktree(&self) -> &WorktreeIdentity {
        &self.worktree
    }

    pub fn lanes(&self) -> &[CheckpointLaneId] {
        &self.lanes
    }

    pub fn fidelity(&self) -> &CheckpointFidelity {
        &self.fidelity
    }

    /// The provider contract version this anchor was negotiated against.
    pub fn provider_version(&self) -> &CheckpointProviderVersion {
        self.checkpoint.version()
    }

    /// Whether `lane` was negotiated for this capture.
    ///
    /// Restore consults this rather than assuming a lane the provider happens
    /// to support.
    pub fn covers_lane(&self, lane: &CheckpointLaneId) -> bool {
        self.lanes.iter().any(|known| known == lane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::{
        CheckpointProviderId, CheckpointProviderVersion, WorkspaceCheckpointId,
    };
    use crate::vcs::{BranchName, BranchState, GitCommitId, VcsUnavailableReason};

    fn checkpoint() -> Option<WorkspaceCheckpointRef> {
        Some(WorkspaceCheckpointRef::new(
            CheckpointProviderId::new("stateengine")?,
            CheckpointProviderVersion::new("1.2.0")?,
            WorkspaceCheckpointId::new("ckpt-1")?,
        ))
    }

    fn git_worktree() -> WorktreeIdentity {
        WorktreeIdentity::Git {
            root: PathIdentity::canonical("/work"),
            common_dir: PathIdentity::canonical("/work/.git"),
            git_dir: PathIdentity::canonical("/work/.git"),
        }
    }

    fn pinned_baseline() -> Option<VcsBaseline> {
        Some(VcsBaseline::Git {
            head: GitCommitId::new("0123456789abcdef0123456789abcdef01234567")?,
            branch: BranchState::Symbolic(BranchName::new("main")?),
        })
    }

    #[test]
    fn a_git_root_cannot_anchor_without_an_exact_head() -> Result<(), Box<dyn std::error::Error>> {
        let unpinned = RecoveryAnchor::new(
            RecoveryAnchorId::new("anchor-1").ok_or("anchor id")?,
            checkpoint().ok_or("checkpoint")?,
            VcsBaseline::Unavailable {
                reason: VcsUnavailableReason::QueryFailed,
            },
            VcsBaseline::Unavailable {
                reason: VcsUnavailableReason::QueryFailed,
            },
            git_worktree(),
            Vec::new(),
            CheckpointFidelity::Exact,
        );
        assert!(
            unpinned.is_none(),
            "a Git-backed capture must pin an exact HEAD commit"
        );
        Ok(())
    }

    #[test]
    fn a_non_git_root_anchors_without_a_commit() -> Result<(), Box<dyn std::error::Error>> {
        let anchor = RecoveryAnchor::new(
            RecoveryAnchorId::new("anchor-1").ok_or("anchor id")?,
            checkpoint().ok_or("checkpoint")?,
            VcsBaseline::NotVersioned,
            VcsBaseline::NotVersioned,
            WorktreeIdentity::Plain {
                root: PathIdentity::canonical("/work"),
            },
            Vec::new(),
            CheckpointFidelity::Exact,
        );
        assert!(anchor.is_some(), "not-versioned is a fact, not a failure");
        Ok(())
    }

    #[test]
    fn a_head_that_moved_during_capture_yields_no_anchor() -> Result<(), Box<dyn std::error::Error>>
    {
        let moved = VcsBaseline::Git {
            head: GitCommitId::new("fedcba9876543210fedcba9876543210fedcba98").ok_or("head")?,
            branch: BranchState::Symbolic(BranchName::new("main").ok_or("branch")?),
        };
        assert!(
            RecoveryAnchor::new(
                RecoveryAnchorId::new("anchor-1").ok_or("anchor id")?,
                checkpoint().ok_or("checkpoint")?,
                pinned_baseline().ok_or("baseline")?,
                moved,
                git_worktree(),
                Vec::new(),
                CheckpointFidelity::Exact,
            )
            .is_none(),
            "a HEAD change during capture cannot be assigned one truthful baseline"
        );
        Ok(())
    }

    #[test]
    fn relinking_a_worktree_mints_a_different_identity() {
        let original = git_worktree();
        let relinked = WorktreeIdentity::Git {
            root: PathIdentity::canonical("/work"),
            common_dir: PathIdentity::canonical("/other/.git"),
            git_dir: PathIdentity::canonical("/other/.git/worktrees/work"),
        };
        assert_ne!(
            original, relinked,
            "same root, different git directories: not the same worktree"
        );
        assert_eq!(original.root(), relinked.root());
    }

    #[test]
    fn an_un_negotiated_lane_is_not_covered() -> Result<(), Box<dyn std::error::Error>> {
        let anchor = RecoveryAnchor::new(
            RecoveryAnchorId::new("anchor-1").ok_or("anchor id")?,
            checkpoint().ok_or("checkpoint")?,
            pinned_baseline().ok_or("baseline")?,
            pinned_baseline().ok_or("baseline")?,
            git_worktree(),
            vec![CheckpointLaneId::new("files").ok_or("lane")?],
            CheckpointFidelity::Exact,
        )
        .ok_or("anchor")?;

        assert!(anchor.covers_lane(&CheckpointLaneId::new("files").ok_or("lane")?));
        assert!(
            !anchor.covers_lane(&CheckpointLaneId::new("secrets").ok_or("lane")?),
            "a lane that was never negotiated is never restored"
        );
        Ok(())
    }

    #[test]
    fn anchor_binds_the_exact_provider_version() -> Result<(), Box<dyn std::error::Error>> {
        let anchor = RecoveryAnchor::new(
            RecoveryAnchorId::new("anchor-1").ok_or("anchor id")?,
            checkpoint().ok_or("checkpoint")?,
            pinned_baseline().ok_or("baseline")?,
            pinned_baseline().ok_or("baseline")?,
            git_worktree(),
            Vec::new(),
            CheckpointFidelity::Exact,
        )
        .ok_or("anchor")?;
        assert_eq!(anchor.provider_version().as_str(), "1.2.0");
        assert!(anchor.baseline().head().is_some());
        Ok(())
    }
}
