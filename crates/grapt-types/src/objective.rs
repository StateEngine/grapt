//! Objective and work-frontier identity. Law (foundational report §"Objective,
//! blocker and work-frontier lifecycle"; supersession matrix "Work identity" and
//! "Objective authority" rows): one Session owns at most ONE active durable
//! objective. User/system authority owns the create/replace/pause/resume/cancel/
//! budget transitions; the model may submit ONLY an evidenced `Complete` or
//! `Blocked` claim — it cannot implicitly redefine or park work.
//!
//! F1A owns these identities and closed vocabularies; the runtime that enforces
//! the transition authority and the derived frontier projection are later crates.

crate::id::opaque_id! {
    /// The one active durable objective an objective-owning Session may hold.
    ObjectiveId
}

crate::id::opaque_id! {
    /// A path-scoped unit of work under an objective's derived frontier.
    WorkItemId
}

crate::id::opaque_id! {
    /// A path-scoped blocker attached to work under an objective. Records the
    /// affected work, evidence, resolving principal and resumable action at the
    /// event layer; this is its stable identity.
    WorkBlockerId
}

/// The durable status of an objective. A closed vocabulary: a completion or
/// blocked CLAIM is `…PendingReview` until reviewed, so a model claim is never
/// itself the terminal fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectiveStatus {
    /// The objective is active and its frontier is actionable.
    Active,
    /// User/system paused the objective; no work is selected until resumed.
    Paused,
    /// Every acceptance path is exhausted by blockers; a `Blocked` claim awaits
    /// review. Not a terminal — review may reopen it.
    BlockedClaimPendingReview,
    /// Acceptance is proved and a `Complete` claim awaits review.
    CompleteClaimPendingReview,
    /// User/system cancelled the objective.
    Cancelled,
    /// User/system replaced the objective with a new one.
    Replaced,
}

/// The ONLY claims a model may submit about an objective. Each must be evidenced
/// (evidence identity is bound at the event layer). The model cannot create,
/// replace, pause, resume, cancel, or re-budget an objective — those are
/// user/system authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectiveClaim {
    /// The model claims the objective's acceptance is proved.
    Complete,
    /// The model claims every acceptance path is blocked.
    Blocked,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objective_identities_are_distinct_non_empty() {
        assert!(ObjectiveId::new("obj_1").is_some());
        assert!(WorkItemId::new("wit_1").is_some());
        assert!(WorkBlockerId::new("wbk_1").is_some());
        assert!(ObjectiveId::new("").is_none());
    }

    #[test]
    fn a_model_claim_is_only_complete_or_blocked() {
        // Exhaustive by construction: adding a variant here forces every match
        // that decides review to be updated.
        for claim in [ObjectiveClaim::Complete, ObjectiveClaim::Blocked] {
            match claim {
                ObjectiveClaim::Complete | ObjectiveClaim::Blocked => {}
            }
        }
        assert_ne!(ObjectiveStatus::Active, ObjectiveStatus::Paused);
    }
}
