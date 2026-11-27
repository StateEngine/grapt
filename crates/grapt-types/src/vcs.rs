//! Version-control baseline identity.
//!
//! Law (amendment §8.5): Git and workspace checkpoints are independent
//! engines, but an Grapt checkpoint of a Git-backed root is incomplete
//! without its exact VCS baseline. Non-Git, unborn, detached and unavailable
//! states are explicit variants — absence is never an omitted field, so a
//! missing baseline cannot masquerade as "not a repository".

/// A Git object name.
///
/// Validated as lowercase hex of a supported width (SHA-1 or SHA-256), so an
/// abbreviated ref, a branch name or a tag cannot be stored as a commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GitCommitId(String);

impl GitCommitId {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        let width_ok = raw.len() == 40 || raw.len() == 64;
        let hex_ok = raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if width_ok && hex_ok {
            Some(Self(raw))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for GitCommitId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

crate::id::opaque_id! {
    /// A normalized branch name. Never a worktree identity.
    BranchName
}

/// Why VCS state could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VcsUnavailableReason {
    /// No VCS tool was available on this host.
    ToolMissing,
    /// The command failed or timed out.
    QueryFailed,
    /// The root exists but its VCS metadata is unreadable.
    MetadataUnreadable,
}

/// Branch state at an exact observation.
///
/// Closed per law so a branch label is neither a worktree identity nor
/// silently absent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BranchState {
    /// HEAD points at a named branch.
    Symbolic(BranchName),
    /// HEAD points directly at a commit.
    Detached,
    /// HEAD names a branch that has no commits yet.
    Unborn(BranchName),
    /// Branch state could not be resolved.
    Unavailable(VcsUnavailableReason),
}

/// The exact version-control baseline observed at capture time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VcsBaseline {
    /// A Git root with a resolved HEAD commit.
    Git {
        head: GitCommitId,
        branch: BranchState,
    },
    /// A Git root on an unborn branch: there is genuinely no HEAD commit to
    /// pin, which is a fact rather than a missing field.
    GitUnborn { branch: BranchName },
    /// The root is not under version control.
    NotVersioned,
    /// VCS state could not be determined.
    Unavailable { reason: VcsUnavailableReason },
}

impl VcsBaseline {
    /// The exact commit this baseline pins, if any.
    pub fn head(&self) -> Option<&GitCommitId> {
        match self {
            VcsBaseline::Git { head, .. } => Some(head),
            VcsBaseline::GitUnborn { .. }
            | VcsBaseline::NotVersioned
            | VcsBaseline::Unavailable { .. } => None,
        }
    }

    /// Whether a Git-backed capture may settle successfully against this
    /// baseline.
    ///
    /// Law: a Git-backed capture without an unchanged before/after HEAD commit
    /// cannot settle as `Captured`.
    pub fn can_pin_git_capture(&self) -> bool {
        matches!(self, VcsBaseline::Git { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> Option<GitCommitId> {
        GitCommitId::new("0123456789abcdef0123456789abcdef01234567")
    }

    #[test]
    fn a_commit_id_refuses_abbreviations_and_refs() {
        assert!(GitCommitId::new("0123456").is_none(), "abbreviated");
        assert!(GitCommitId::new("refs/heads/main").is_none(), "a ref");
        assert!(GitCommitId::new("HEAD").is_none(), "a symbolic name");
        assert!(
            GitCommitId::new("0123456789ABCDEF0123456789abcdef01234567").is_none(),
            "uppercase is not the canonical form"
        );
        assert!(head().is_some(), "a full sha-1 object name is accepted");
    }

    #[test]
    fn non_git_and_unborn_are_explicit_not_absent() -> Result<(), Box<dyn std::error::Error>> {
        assert!(VcsBaseline::NotVersioned.head().is_none());
        assert!(!VcsBaseline::NotVersioned.can_pin_git_capture());

        let unborn = VcsBaseline::GitUnborn {
            branch: BranchName::new("main").ok_or("branch")?,
        };
        assert!(unborn.head().is_none());
        assert!(
            !unborn.can_pin_git_capture(),
            "an unborn branch cannot pin a Git-backed capture"
        );
        Ok(())
    }

    #[test]
    fn detached_head_still_pins_an_exact_commit() -> Result<(), Box<dyn std::error::Error>> {
        let baseline = VcsBaseline::Git {
            head: head().ok_or("head")?,
            branch: BranchState::Detached,
        };
        assert!(baseline.can_pin_git_capture());
        assert!(baseline.head().is_some());
        Ok(())
    }

    #[test]
    fn unavailable_is_not_not_versioned() {
        let unavailable = VcsBaseline::Unavailable {
            reason: VcsUnavailableReason::ToolMissing,
        };
        assert_ne!(unavailable, VcsBaseline::NotVersioned);
        assert!(!unavailable.can_pin_git_capture());
    }
}
