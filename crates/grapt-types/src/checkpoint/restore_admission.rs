use super::restore_plan::{
    RestorePrecondition, RestoreRescue, RestoreRescueEvidence, SettledRestorePlan,
};
use super::CheckpointRestorePlanId;
use crate::authority::{DeterministicAuthorities, Timestamp};
use crate::capability::{CheckpointRestoreAuthorityLease, CheckpointRestoreCapabilityId};
use crate::operation::AmbiguityEvidence;
use crate::terminality::ExternalEffectState;

impl SettledRestorePlan {
    /// The one governed entry that applies a workspace restore.
    ///
    /// Law §8.3: the entry carries a non-clone [`CheckpointRestoreAuthorityLease`] through effect
    /// completion, and every terminal class commits a typed receipt. The lease
    /// is consumed **first and unconditionally**, so a refused admission burns
    /// the authorization exactly like a successful one: a caller cannot probe
    /// the boundary for free and retry with the same authority.
    ///
    /// Law §8.5: rescue happens before the first destructive write, so the
    /// rescue is an *input* here rather than something the applier promises to
    /// do later, and it is copied into the receipt.
    ///
    /// # Which failures are compile errors and which fail closed
    ///
    /// Omitting the lease, omitting the rescue, or passing anything that is not
    /// this exact settled plan does not compile — they are arity and type
    /// errors (probes 37, 38). Two of the checks below cannot be compile errors
    /// and are therefore refusals that fail closed:
    ///
    /// - a lease for a **different capability**. [`CheckpointRestoreCapabilityId`] is a single
    ///   nominal type carrying a derived value, which is what makes capabilities
    ///   comparable at all; the compiler cannot tell two values of one type
    ///   apart, so this is a value comparison against
    ///   [`SettledRestorePlan::required_capability`].
    /// - an **expired** plan. Expiry is a fact about the clock at apply time,
    ///   not about the type.
    ///
    /// Both refuse before any external effect is entered, and both commit a
    /// receipt naming the reason.
    pub fn apply(
        self,
        rescue: RestoreRescue,
        preconditions: PlanPreconditionObservation,
        lease: CheckpointRestoreAuthorityLease,
        authorities: &DeterministicAuthorities<'_>,
    ) -> RestoreAdmission {
        // Consumed first: the lease is spent whatever this boundary decides.
        let capability = lease.consume();
        let now = authorities.now();

        let expired_at = self.expires_at;
        if now >= expired_at {
            return RestoreAdmission::refused(
                self,
                capability,
                rescue.evidence,
                RestoreRefusalReason::PlanExpired {
                    expired_at,
                    observed: now,
                },
                now,
            );
        }
        if capability != self.required_capability {
            return RestoreAdmission::refused(
                self,
                capability,
                rescue.evidence,
                RestoreRefusalReason::LeaseNamesAnotherCapability,
                now,
            );
        }
        if rescue.plan != self.plan {
            return RestoreAdmission::refused(
                self,
                capability,
                rescue.evidence,
                RestoreRefusalReason::RescueProvenForAnotherPlan,
                now,
            );
        }

        // ITEM 3. The plan bound its preconditions and nothing ever read them.
        // Every check below is a PRE-EFFECT refusal: it happens before the
        // admission exists, so a stale provider version, an unnegotiated lane or
        // a non-quiescent worktree cannot reach the destructive write.
        if preconditions.plan() != &self.plan {
            return RestoreAdmission::refused(
                self,
                capability,
                rescue.evidence,
                RestoreRefusalReason::PreconditionsObservedForAnotherPlan,
                now,
            );
        }
        // The observation must describe the world inside this plan's validity
        // window. One taken before the plan was settled describes a world that
        // predates the plan; one taken at or after expiry is outside it.
        let observed_at = preconditions.observed_at();
        if observed_at < self.settled_at || observed_at >= expired_at {
            return RestoreAdmission::refused(
                self,
                capability,
                rescue.evidence,
                RestoreRefusalReason::PreconditionObservationOutsidePlanWindow { observed_at },
                now,
            );
        }
        if let Some(missing) = self
            .preconditions
            .iter()
            .find(|required| !preconditions.covers(required))
            .cloned()
        {
            return RestoreAdmission::refused(
                self,
                capability,
                rescue.evidence,
                RestoreRefusalReason::PreconditionNotSatisfied {
                    precondition: missing,
                },
                now,
            );
        }

        RestoreAdmission::Admitted(Box::new(RestoreApplicationAdmission {
            plan: self,
            capability,
            rescue: rescue.evidence,
            preconditions,
            admitted_at: now,
        }))
    }
}

/// A provider observation that a settled plan's preconditions currently hold.
///
/// ITEM 3. The plan bound its preconditions and then nothing ever looked at
/// them: `apply` checked expiry, capability and rescue only, so an application
/// could enter with a stale provider version, an unnegotiated lane or a
/// non-quiescent worktree — exactly the conditions the plan recorded in order to
/// prevent that.
///
/// Bound to one plan and to one instant. Not `Clone`: an observation is a
/// statement about the world at a moment, and reusing it is how a stale
/// observation authorizes a changed world.
#[derive(Debug)]
pub struct PlanPreconditionObservation {
    plan: CheckpointRestorePlanId,
    satisfied: Vec<RestorePrecondition>,
    observed_at: Timestamp,
}

impl PlanPreconditionObservation {
    /// Record that the named preconditions were observed to hold for `plan`.
    ///
    /// Doc-hidden owner seam. Only `grapt-checkpoint` calls it in production,
    /// after querying the provider and the independent VCS observer. The
    /// workspace architecture gate refuses any second production caller.
    #[doc(hidden)]
    pub fn observe_at_provider_boundary(
        plan: &SettledRestorePlan,
        satisfied: Vec<RestorePrecondition>,
        authorities: &DeterministicAuthorities<'_>,
    ) -> Self {
        Self {
            plan: plan.plan().clone(),
            satisfied,
            observed_at: authorities.now(),
        }
    }

    pub fn plan(&self) -> &CheckpointRestorePlanId {
        &self.plan
    }

    pub fn satisfied(&self) -> &[RestorePrecondition] {
        &self.satisfied
    }

    pub fn observed_at(&self) -> Timestamp {
        self.observed_at
    }

    /// Whether this observation covers `precondition`.
    pub fn covers(&self, precondition: &RestorePrecondition) -> bool {
        self.satisfied.contains(precondition)
    }
}

/// The outcome of the governed apply entry.
///
/// ITEM 4. The previous shape claimed something a types crate cannot know. It
/// returned a `RestoreInFlight` whose `applied()` took a caller assertion and
/// minted an `Applied` terminal — so "the restore landed" was whatever the
/// caller said — and the in-flight token could simply be DROPPED, leaving no
/// receipt at all despite the doc promising exactly one per admission. A
/// `#[must_use]` would not have fixed either: it is a lint, not a guarantee.
///
/// So the effect API is deleted rather than decorated. This crate can establish
/// two things and does: it can REFUSE before any destructive write, with a
/// receipt naming why, and it can ADMIT an application whose every checkable
/// precondition held. It cannot observe a provider, so it never claims an
/// external effect entered or completed.
#[derive(Debug)]
pub enum RestoreAdmission {
    /// Admitted. The lease is spent and every condition this crate can check
    /// held. What happens next is the provider owner's to observe.
    ///
    /// Boxed: both arms own a whole settled plan.
    Admitted(Box<RestoreApplicationAdmission>),
    /// Refused before the first destructive write, with the receipt that says
    /// why. Boxed for the same reason.
    Refused(Box<RestoreRefusalReceipt>),
}

impl RestoreAdmission {
    #[allow(clippy::too_many_arguments)]
    fn refused(
        plan: SettledRestorePlan,
        capability: CheckpointRestoreCapabilityId,
        rescue: RestoreRescueEvidence,
        reason: RestoreRefusalReason,
        at: Timestamp,
    ) -> Self {
        RestoreAdmission::Refused(Box::new(RestoreRefusalReceipt {
            plan,
            capability,
            rescue,
            reason,
            settled_at: at,
        }))
    }

    /// The receipt of a refused admission, if this is one.
    pub fn refusal(&self) -> Option<&RestoreRefusalReceipt> {
        match self {
            RestoreAdmission::Refused(receipt) => Some(receipt.as_ref()),
            RestoreAdmission::Admitted(_) => None,
        }
    }

    /// The admission, if the entry admitted one.
    pub fn admission(&self) -> Option<&RestoreApplicationAdmission> {
        match self {
            RestoreAdmission::Admitted(admission) => Some(admission.as_ref()),
            RestoreAdmission::Refused(_) => None,
        }
    }

    /// Consume the admission boundary result so the checkpoint owner can
    /// carry the one-shot application token into the provider or persist the
    /// refusal receipt.
    pub fn into_result(self) -> Result<RestoreApplicationAdmission, Box<RestoreRefusalReceipt>> {
        match self {
            RestoreAdmission::Admitted(admission) => Ok(*admission),
            RestoreAdmission::Refused(receipt) => Err(receipt),
        }
    }
}

/// An admitted restore application.
///
/// Deliberately OPAQUE about the effect. It records what this crate actually
/// established — the exact plan, the capability the consumed lease named, the
/// pre-write rescue evidence, the precondition observation it was admitted
/// against, and when — and it offers no method that asserts the restore ran,
/// landed, or failed. There is no `applied()`, because nothing here can know.
///
/// Not `Clone`: one settlement, one admission.
///
/// `grapt-checkpoint` executes the application and owns its terminal classes.
/// Its durable pre-effect journal makes a dropped admission recoverable, and
/// its provider receipt comparison keeps `Applied` on observed provider
/// evidence rather than caller assertion.
#[derive(Debug)]
pub struct RestoreApplicationAdmission {
    plan: SettledRestorePlan,
    capability: CheckpointRestoreCapabilityId,
    rescue: RestoreRescueEvidence,
    preconditions: PlanPreconditionObservation,
    admitted_at: Timestamp,
}

impl RestoreApplicationAdmission {
    /// The exact plan this application was admitted for.
    pub fn plan(&self) -> &SettledRestorePlan {
        &self.plan
    }

    /// The capability the application is authorized under, taken from the
    /// consumed lease.
    pub fn capability(&self) -> &CheckpointRestoreCapabilityId {
        &self.capability
    }

    /// The rescue evidence recorded before any destructive write (§8.5).
    pub fn rescue(&self) -> &RestoreRescueEvidence {
        &self.rescue
    }

    /// The precondition observation this admission was granted against.
    pub fn preconditions(&self) -> &PlanPreconditionObservation {
        &self.preconditions
    }

    pub fn admitted_at(&self) -> Timestamp {
        self.admitted_at
    }

    /// What this crate can say about the external effect: nothing definite.
    ///
    /// Admission is authorization, not observation. Until the provider owner
    /// reports, the effect is ambiguous — which is precisely what stops an
    /// automatic replay.
    pub fn external_effect(&self) -> Option<ExternalEffectState> {
        AmbiguityEvidence::new("restore admitted; provider outcome unobserved")
            .map(|evidence| ExternalEffectState::Ambiguous { evidence })
    }
}

/// Why the governed entry refused before entering the external effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreRefusalReason {
    /// The plan's expiry had passed when the entry was reached.
    PlanExpired {
        expired_at: Timestamp,
        observed: Timestamp,
    },
    /// The consumed lease names a capability other than the one this plan's
    /// complete canonical form derives.
    LeaseNamesAnotherCapability,
    /// The rescue was proven for a different restore plan.
    RescueProvenForAnotherPlan,
    /// The precondition observation was taken for a different restore plan.
    PreconditionsObservedForAnotherPlan,
    /// The observation predates the plan it is offered for, or was taken after
    /// the plan's validity window closed: a stale observation describes a world
    /// that is no longer the one being restored.
    PreconditionObservationOutsidePlanWindow { observed_at: Timestamp },
    /// A precondition the plan requires was not observed to hold.
    PreconditionNotSatisfied { precondition: RestorePrecondition },
}

/// The typed receipt a refusal commits.
///
/// Owns the [`SettledRestorePlan`] that was refused, so the receipt names the
/// exact plan rather than a copy of some of its coordinates. Because the plan is
/// not `Clone`, a receipt can never be taken apart to apply that plan again.
///
/// There is deliberately no counterpart for a successful application: see
/// [`RestoreApplicationAdmission`].
#[derive(Debug, PartialEq, Eq)]
pub struct RestoreRefusalReceipt {
    plan: SettledRestorePlan,
    capability: CheckpointRestoreCapabilityId,
    rescue: RestoreRescueEvidence,
    reason: RestoreRefusalReason,
    settled_at: Timestamp,
}

impl RestoreRefusalReceipt {
    /// The exact plan this application was refused for.
    pub fn plan(&self) -> &SettledRestorePlan {
        &self.plan
    }

    /// The capability the consumed lease named.
    pub fn capability(&self) -> &CheckpointRestoreCapabilityId {
        &self.capability
    }

    /// The rescue evidence, recorded as §8.5 requires.
    pub fn rescue(&self) -> &RestoreRescueEvidence {
        &self.rescue
    }

    pub fn reason(&self) -> &RestoreRefusalReason {
        &self.reason
    }

    pub fn settled_at(&self) -> Timestamp {
        self.settled_at
    }

    /// A refusal is the one restore terminal whose external-effect fact this
    /// crate can state: nothing was initiated, so it is safe to replay.
    pub fn external_effect(&self) -> ExternalEffectState {
        ExternalEffectState::NoneInitiated
    }
}
