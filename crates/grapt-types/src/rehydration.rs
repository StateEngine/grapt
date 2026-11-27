//! The bounded rehydration manifest a compaction produces for the next request.
//!
//! # The law
//!
//! Amendment §5.5: "Compaction produces a bounded rehydration manifest for the
//! next request." It may reference, with provenance and integrity identity, the
//! active specification and plan files, the current objective and acceptance
//! criteria, working-state/decision/receipt identities, exact branches, commits
//! and dirty-worktree warnings, open blockers and who can resolve them, and the
//! context artifacts intentionally retained across the new epoch.
//!
//! Two sentences of that section carry the whole design: "References are
//! preferred over stale copied content" and "Missing required references fail
//! visibly; optional references carry an explicit unavailable state."
//!
//! # Why this is a type and not a bag
//!
//! The F4B preflight recorded that no rehydration-manifest type existed
//! anywhere in the workspace, so the obligation
//! had no carrier at all. A `Vec<String>` would satisfy the words and none of
//! the law: it could copy content instead of referencing it, lose provenance,
//! grow unbounded, and — worst — represent a missing REQUIRED input as an
//! absent element, which is the exact collapse §9 and §5.5 both forbid.
//!
//! So: every entry names a closed [`RehydrationSlot`], carries a
//! [`crate::context::TrustLabel`] provenance and a
//! [`crate::context::ContributionState`] whose `Available` arm is an
//! [`ArtifactRef`] — a reference with integrity identity, never inline bytes.
//! Requirement is a property of the ENTRY, so a required slot that could not be
//! resolved is a typed refusal at construction rather than a hole a later
//! reader has to notice.
//!
//! # Bounded before allocation
//!
//! [`RehydrationManifest::build`] refuses on entry count and on total
//! referenced bytes, and it refuses BEFORE it retains the entries, so an
//! oversized manifest cannot transiently exist. This mirrors the artifact law:
//! a count-bounded container holding unbounded values is not bounded.

use crate::artifact::ArtifactRef;
use crate::context::{ContributionState, ContributionUnavailable, TrustLabel};

/// What one rehydration entry supplies to the next request.
///
/// Closed over exactly the categories §5.5 enumerates. It is a closed enum
/// rather than a string so a new category cannot be introduced by a caller
/// without every exhaustive owner acknowledging it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RehydrationSlot {
    /// Active specification and plan files.
    SpecificationOrPlan,
    /// The current objective and its acceptance criteria.
    ObjectiveAndAcceptance,
    /// Working-state, decision and receipt identities.
    WorkingStateReceipt,
    /// Exact branch, commit and dirty-worktree warning.
    VcsBaseline,
    /// Open blockers and the principal who can resolve each.
    OpenBlocker,
    /// Context artifacts intentionally retained across the new epoch.
    RetainedContext,
}

/// Why a rehydration manifest could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RehydrationError {
    /// A slot the caller declared REQUIRED could not be resolved.
    ///
    /// This is the visible failure §5.5 demands. It names the slot and the
    /// exact unavailability, so the refusal cannot be mistaken for "there was
    /// nothing to include".
    RequiredMissing {
        slot: RehydrationSlot,
        state: RequiredMiss,
    },
    /// More entries than the declared bound.
    TooManyEntries { limit: usize, offered: usize },
    /// The referenced bytes exceed the declared bound.
    ExceedsByteBound { limit: u64, offered: u64 },
}

impl std::fmt::Display for RehydrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RehydrationError::RequiredMissing { slot, state } => write!(
                f,
                "required rehydration slot {slot:?} is missing: {state:?}"
            ),
            RehydrationError::TooManyEntries { limit, offered } => write!(
                f,
                "rehydration manifest offered {offered} entries, bound is {limit}"
            ),
            RehydrationError::ExceedsByteBound { limit, offered } => write!(
                f,
                "rehydration manifest references {offered} bytes, bound is {limit}"
            ),
        }
    }
}

impl std::error::Error for RehydrationError {}

/// How a required slot failed to resolve.
///
/// `Absent` and `Unavailable` are kept apart here for the same reason §9 keeps
/// them apart on the contribution itself: one says the contract proves there is
/// no value, the other says a value should exist and could not be loaded. A
/// required slot fails on BOTH, but the reason survives into the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequiredMiss {
    /// The contract proved there is no value for a slot declared required.
    Absent,
    /// A value should have existed but could not be resolved.
    Unavailable { reason: ContributionUnavailable },
}

/// One referenced input for the next request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RehydrationEntry {
    slot: RehydrationSlot,
    provenance: TrustLabel,
    required: bool,
    state: ContributionState,
}

impl RehydrationEntry {
    /// A required entry. Construction of the manifest refuses if it is not
    /// `Available`.
    pub fn required(
        slot: RehydrationSlot,
        provenance: TrustLabel,
        state: ContributionState,
    ) -> Self {
        Self {
            slot,
            provenance,
            required: true,
            state,
        }
    }

    /// An optional entry. It may carry an explicit unavailable state and the
    /// manifest still builds — that is §5.5's "optional references carry an
    /// explicit unavailable state", not silent omission.
    pub fn optional(
        slot: RehydrationSlot,
        provenance: TrustLabel,
        state: ContributionState,
    ) -> Self {
        Self {
            slot,
            provenance,
            required: false,
            state,
        }
    }

    pub fn slot(&self) -> RehydrationSlot {
        self.slot
    }

    pub fn provenance(&self) -> TrustLabel {
        self.provenance
    }

    pub fn is_required(&self) -> bool {
        self.required
    }

    pub fn state(&self) -> &ContributionState {
        &self.state
    }

    /// The reference, when one resolved.
    pub fn artifact(&self) -> Option<&ArtifactRef> {
        match &self.state {
            ContributionState::Available { artifact } => Some(artifact),
            ContributionState::Absent | ContributionState::Unavailable { .. } => None,
        }
    }
}

/// Bounds a rehydration manifest is built under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RehydrationBounds {
    max_entries: usize,
    max_bytes: u64,
}

impl RehydrationBounds {
    pub fn new(max_entries: usize, max_bytes: u64) -> Self {
        Self {
            max_entries,
            max_bytes,
        }
    }

    pub fn max_entries(self) -> usize {
        self.max_entries
    }

    pub fn max_bytes(self) -> u64 {
        self.max_bytes
    }
}

/// The bounded set of references handed to the next request after compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RehydrationManifest {
    entries: Vec<RehydrationEntry>,
    bounds: RehydrationBounds,
    referenced_bytes: u64,
}

impl RehydrationManifest {
    /// Build the manifest, refusing on a missing required slot or either bound.
    ///
    /// Order matters and is deliberate. The count bound is checked against the
    /// OFFERED entries before anything is retained, and the byte bound is
    /// accumulated from the references' exact declared lengths, so neither an
    /// oversized manifest nor its payload total exists even transiently.
    pub fn build(
        entries: Vec<RehydrationEntry>,
        bounds: RehydrationBounds,
    ) -> Result<Self, RehydrationError> {
        if entries.len() > bounds.max_entries() {
            return Err(RehydrationError::TooManyEntries {
                limit: bounds.max_entries(),
                offered: entries.len(),
            });
        }

        let mut referenced_bytes: u64 = 0;
        for entry in &entries {
            if entry.is_required() {
                match entry.state() {
                    ContributionState::Available { .. } => {}
                    ContributionState::Absent => {
                        return Err(RehydrationError::RequiredMissing {
                            slot: entry.slot(),
                            state: RequiredMiss::Absent,
                        })
                    }
                    ContributionState::Unavailable { reason } => {
                        return Err(RehydrationError::RequiredMissing {
                            slot: entry.slot(),
                            state: RequiredMiss::Unavailable { reason: *reason },
                        })
                    }
                }
            }
            if let Some(artifact) = entry.artifact() {
                referenced_bytes = referenced_bytes.saturating_add(artifact.length());
                if referenced_bytes > bounds.max_bytes() {
                    return Err(RehydrationError::ExceedsByteBound {
                        limit: bounds.max_bytes(),
                        offered: referenced_bytes,
                    });
                }
            }
        }

        Ok(Self {
            entries,
            bounds,
            referenced_bytes,
        })
    }

    pub fn entries(&self) -> &[RehydrationEntry] {
        &self.entries
    }

    pub fn bounds(&self) -> RehydrationBounds {
        self.bounds
    }

    /// Total exact length of everything referenced. Never the size of inlined
    /// content, because nothing is inlined.
    pub fn referenced_bytes(&self) -> u64 {
        self.referenced_bytes
    }

    /// Entries for one slot, in declared order.
    pub fn slot(&self, slot: RehydrationSlot) -> impl Iterator<Item = &RehydrationEntry> {
        self.entries.iter().filter(move |e| e.slot() == slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{ArtifactDigest, ArtifactId, ArtifactRef};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn artifact(id: &str, length: u64) -> Option<ArtifactRef> {
        Some(ArtifactRef::new(
            ArtifactId::new(id)?,
            length,
            ArtifactDigest::new("digest")?,
        ))
    }

    fn bounds() -> RehydrationBounds {
        RehydrationBounds::new(8, 1_000)
    }

    #[test]
    fn a_required_slot_that_is_absent_fails_visibly() -> TestResult {
        // §5.5: "Missing required references fail visibly". The failure mode
        // this closes is a manifest that BUILDS while a required input is
        // missing, because then the next request silently assembles without it.
        let entries = vec![RehydrationEntry::required(
            RehydrationSlot::ObjectiveAndAcceptance,
            TrustLabel::Trusted,
            ContributionState::Absent,
        )];
        assert_eq!(
            RehydrationManifest::build(entries, bounds()),
            Err(RehydrationError::RequiredMissing {
                slot: RehydrationSlot::ObjectiveAndAcceptance,
                state: RequiredMiss::Absent,
            })
        );
        Ok(())
    }

    #[test]
    fn a_required_slot_that_is_unavailable_keeps_its_exact_reason() -> TestResult {
        // Absent and Unavailable both refuse, but they are NOT the same fact and
        // the refusal must not merge them: one says the contract proves there is
        // no value, the other says a value exists and could not be loaded.
        let entries = vec![RehydrationEntry::required(
            RehydrationSlot::SpecificationOrPlan,
            TrustLabel::ProjectContent,
            ContributionState::Unavailable {
                reason: ContributionUnavailable::SourceUnreadable,
            },
        )];
        assert_eq!(
            RehydrationManifest::build(entries, bounds()),
            Err(RehydrationError::RequiredMissing {
                slot: RehydrationSlot::SpecificationOrPlan,
                state: RequiredMiss::Unavailable {
                    reason: ContributionUnavailable::SourceUnreadable,
                },
            })
        );
        Ok(())
    }

    #[test]
    fn an_optional_slot_carries_its_unavailable_state_instead_of_vanishing() -> TestResult {
        // §5.5: "optional references carry an explicit unavailable state". The
        // manifest must BUILD and the entry must SURVIVE, because dropping it
        // would make an unavailable optional input indistinguishable from one
        // that was never requested.
        let entries = vec![RehydrationEntry::optional(
            RehydrationSlot::OpenBlocker,
            TrustLabel::Trusted,
            ContributionState::Unavailable {
                reason: ContributionUnavailable::PolicyRefused,
            },
        )];
        let manifest = RehydrationManifest::build(entries, bounds())?;
        assert_eq!(manifest.entries().len(), 1, "the entry must not be dropped");
        assert_eq!(
            manifest.entries()[0].state(),
            &ContributionState::Unavailable {
                reason: ContributionUnavailable::PolicyRefused,
            }
        );
        assert_eq!(manifest.referenced_bytes(), 0);
        Ok(())
    }

    #[test]
    fn the_byte_bound_refuses_before_the_manifest_exists() -> TestResult {
        // A count-bounded container holding unbounded values is not bounded.
        // Two entries are within the count bound and still exceed the bytes.
        let entries = vec![
            RehydrationEntry::optional(
                RehydrationSlot::RetainedContext,
                TrustLabel::Trusted,
                ContributionState::Available {
                    artifact: artifact("a", 600).ok_or("artifact")?,
                },
            ),
            RehydrationEntry::optional(
                RehydrationSlot::RetainedContext,
                TrustLabel::Trusted,
                ContributionState::Available {
                    artifact: artifact("b", 600).ok_or("artifact")?,
                },
            ),
        ];
        assert_eq!(
            RehydrationManifest::build(entries, bounds()),
            Err(RehydrationError::ExceedsByteBound {
                limit: 1_000,
                offered: 1_200,
            })
        );
        Ok(())
    }

    #[test]
    fn the_entry_bound_refuses_a_manifest_with_too_many_entries() -> TestResult {
        let entries: Vec<RehydrationEntry> = (0..3)
            .map(|_| {
                RehydrationEntry::optional(
                    RehydrationSlot::WorkingStateReceipt,
                    TrustLabel::Trusted,
                    ContributionState::Absent,
                )
            })
            .collect();
        assert_eq!(
            RehydrationManifest::build(entries, RehydrationBounds::new(2, 1_000)),
            Err(RehydrationError::TooManyEntries {
                limit: 2,
                offered: 3,
            })
        );
        Ok(())
    }

    #[test]
    fn a_built_manifest_references_and_never_inlines() -> TestResult {
        let entries = vec![RehydrationEntry::required(
            RehydrationSlot::VcsBaseline,
            TrustLabel::ProjectContent,
            ContributionState::Available {
                artifact: artifact("vcs", 42).ok_or("artifact")?,
            },
        )];
        let manifest = RehydrationManifest::build(entries, bounds())?;
        assert_eq!(manifest.referenced_bytes(), 42);
        let entry = manifest
            .slot(RehydrationSlot::VcsBaseline)
            .next()
            .ok_or("slot lookup")?;
        assert!(entry.is_required());
        assert_eq!(entry.artifact().map(ArtifactRef::length), Some(42));
        Ok(())
    }
}
