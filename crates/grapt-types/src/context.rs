//! Context window, compaction descriptor, committed-prefix receipt and the
//! model-visible contribution vocabulary.
//!
//! Law (amendment §4 and §9): a successful compaction commits **one** atomic
//! durable descriptor that *references* retained lineage and artifacts. It
//! never inlines duplicate copies of retained history, and a marker plus a
//! copied replacement transcript is not a compaction.
//!
//! Prefix reuse is receipted from exact ordered contributions and exact
//! adapter/protocol/policy versions — never inferred from text length or a
//! provider cache-hit flag.

use crate::artifact::ArtifactRef;
use crate::attempt::StepNumber;
use crate::model::{ModelCatalogVersion, ModelId, ModelRef, ModelVariantId, ProviderId};
use crate::session::SessionId;
use crate::terminality::ContextFailure;

crate::id::opaque_id! {
    /// Identity of one compacted context window.
    ContextWindowId
}

/// The committed context window used to assemble one provider request.
/// This is provenance, never an authority token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContextIdentity {
    pub window: ContextWindowId,
    pub epoch: ContextEpoch,
    pub descriptor: crate::EventId,
}

crate::id::opaque_id! {
    /// Identity of one compaction.
    CompactionId
}

crate::id::opaque_id! {
    /// Version of the retention policy that selected what was retained.
    RetentionPolicyVersion
}

crate::id::opaque_id! {
    /// Version of the adapter that assembled a request.
    AdapterVersion
}

crate::id::opaque_id! {
    /// Stable identity of one model-visible context contribution.
    ContributionId
}

/// Monotonic epoch of a context window.
///
/// A compaction that crosses an epoch invalidates plan-read receipts and may
/// re-mint assistant message identity, so the epoch is a first-class value
/// rather than a counter hidden in a projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContextEpoch(u64);

impl ContextEpoch {
    pub const FIRST: ContextEpoch = ContextEpoch(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// What produced a compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompactionTrigger {
    /// The prepared request exceeded the model's context.
    ContextOverflow,
    /// A versioned policy threshold was reached at a safe boundary.
    PolicyThreshold,
    /// The user asked for it.
    UserRequested,
    /// Emergency headroom was consumed and no safe boundary arrived.
    EmergencyHeadroom,
    /// A bounded durable-history projection reached its structural page limit.
    StructuralBound,
}

/// The summary artifact produced by a compaction.
///
/// Closed per amendment §4.3: a provider-bound opaque summary carries the
/// exact target it can be redeemed against, so it can never be replayed into
/// an incompatible provider or model. The provider-neutral text strategy stays
/// first-class and is the recovery path when a bound artifact cannot be
/// redeemed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionSummary {
    /// Client-readable summary text, redeemable anywhere.
    ClientReadable { artifact: ArtifactRef },
    /// Provider-held opaque content bound to an exact compatibility identity.
    ///
    /// The binding is durable [`SummaryCompatibility`] DATA, not a
    /// [`ModelRef`]. A descriptor is a durable fact, and a `ModelRef`'s
    /// equality authority is a process-local catalog snapshot that cannot
    /// survive one — see `SummaryCompatibility`.
    ProviderBound {
        artifact: ArtifactRef,
        compatibility: SummaryCompatibility,
    },
}

impl CompactionSummary {
    /// Whether this summary can be replayed against `target`.
    ///
    /// Compatibility, not issuance. The predicate compares the bound
    /// provider/model/variant coordinates against the live target's; the
    /// recorded catalog version takes no part. Demanding the original
    /// issuance would make every provider-bound summary unredeemable after a
    /// restart, which is the one event compaction most needs to survive.
    pub fn is_redeemable_by(&self, target: &ModelRef) -> bool {
        match self {
            CompactionSummary::ClientReadable { .. } => true,
            CompactionSummary::ProviderBound { compatibility, .. } => {
                compatibility.matches_target(target)
            }
        }
    }

    pub fn artifact(&self) -> &ArtifactRef {
        match self {
            CompactionSummary::ClientReadable { artifact }
            | CompactionSummary::ProviderBound { artifact, .. } => artifact,
        }
    }

    /// Decide redemption against `target`, carrying the recovery path with the
    /// refusal rather than leaving the caller to invent one.
    ///
    /// Law §4.3: "Provider-bound opaque summaries cannot be replayed against an
    /// incompatible provider/model. The provider-neutral text strategy remains
    /// first-class and is the recovery path when a provider-bound artifact
    /// cannot be redeemed."
    ///
    /// [`Self::is_redeemable_by`] answers only yes/no, which is why it is not
    /// enough on its own: a bare `false` at this seam is indistinguishable from
    /// "no summary", and a caller that treated it as absence would silently
    /// continue with no summary at all instead of rebuilding a neutral one. The
    /// refusal therefore CARRIES [`SummaryRedemption::RefusedIncompatible`],
    /// whose payload names the bound compatibility coordinates, the
    /// coordinates actually asked for, and the typed
    /// [`ContextFailure::CompactionIncompatibleSummary`] the owner must settle
    /// if it cannot take the recovery path.
    ///
    /// Both sides of that report are compatibility DATA. The requested side is
    /// derived from the live target rather than holding the target itself, so
    /// a refusal carries no process-local issuance authority out of this call
    /// and reads the same whether it was produced before or after a restart.
    pub fn redeem_for(&self, target: &ModelRef) -> SummaryRedemption {
        match self {
            // Provider-neutral text is first-class: it redeems everywhere, and
            // it is what an incompatible bound summary recovers TO.
            CompactionSummary::ClientReadable { artifact } => SummaryRedemption::Redeemable {
                artifact: artifact.clone(),
            },
            CompactionSummary::ProviderBound {
                artifact,
                compatibility,
            } => {
                if compatibility.matches_target(target) {
                    SummaryRedemption::Redeemable {
                        artifact: artifact.clone(),
                    }
                } else {
                    SummaryRedemption::RefusedIncompatible(Box::new(IncompatibleSummary {
                        bound: compatibility.clone(),
                        requested: SummaryCompatibility::bind(target),
                        failure: ContextFailure::CompactionIncompatibleSummary,
                    }))
                }
            }
        }
    }
}

/// The durable compatibility identity a provider-bound summary is bound to.
///
/// # Why this exists, and why it is not a `ModelRef`
///
/// F4B1a bound a provider-held summary to a [`ModelRef`]. That was correct
/// for an in-memory contract and is the WRONG durable carrier, because a
/// `ModelRef`'s equality authority is its catalog SNAPSHOT — an `Arc` minted
/// fresh by every `ModelCatalog::load`, even for a repeated version string.
/// Issuance identity is process-local by design and cannot survive
/// durability. A durable fact that claimed to carry it would be forging
/// exactly what that seam exists to prevent.
///
/// What survives is compatibility DATA: the provider, the model, and the
/// variant where one is bound. Amendment §4.3 asks for "an exact
/// provider/model compatibility identity" and §4.2 for "provider and model
/// identity when any artifact is provider-bound" — that is this, and no more.
///
/// # The catalog version is audit provenance and reads nothing
///
/// [`Self::catalog_version`] records WHICH catalog release vouched for the
/// coordinates when the fact was authored. No rule consults it, and that is
/// deliberate in both directions: requiring equality would refuse a
/// legitimately redeemable summary after every ordinary catalog reload, while
/// a hostile catalog could simply copy the string. It gates nothing, so it is
/// recorded as history rather than pretending to be authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryCompatibility {
    provider: ProviderId,
    model: ModelId,
    variant: Option<ModelVariantId>,
    catalog_version: ModelCatalogVersion,
}

impl SummaryCompatibility {
    /// BIND ROUTE — derive the durable value from a live catalog-vouched
    /// target while authoring the fact.
    ///
    /// This is the only route by which authored data enters from a
    /// `ModelRef`, so at the bind seam the coordinates are the catalog's and
    /// never a caller's strings.
    pub fn bind(target: &ModelRef) -> Self {
        Self {
            provider: target.provider().clone(),
            model: target.model().clone(),
            variant: target.variant().cloned(),
            catalog_version: target.catalog_version().clone(),
        }
    }

    /// RECONSTITUTION ROUTE — rebuild the value from persisted coordinates
    /// that a strict codec has already validated.
    ///
    /// **This route conveys ZERO catalog authority.** It is inert data. It
    /// does not assert that any catalog vouched for these bytes, it cannot
    /// restore the issuance the fact was authored under, and no `ModelRef`
    /// is or can be constructed from it — there is no such conversion in
    /// either direction, because `ModelRef`'s only public constructor takes a
    /// `ModelCatalogEntry` that only a loaded catalog mints.
    ///
    /// The distinction is load-bearing rather than decorative: a decoded
    /// value that could present itself as catalog-vouched would let stored
    /// bytes speak with a live catalog's authority. What a decoded value CAN
    /// do is exactly one thing — be compared, coordinate for coordinate,
    /// against a target the caller already holds. See
    /// [`Self::matches_target`].
    pub fn from_validated_parts(
        provider: ProviderId,
        model: ModelId,
        variant: Option<ModelVariantId>,
        catalog_version: ModelCatalogVersion,
    ) -> Self {
        Self {
            provider,
            model,
            variant,
            catalog_version,
        }
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn model(&self) -> &ModelId {
        &self.model
    }

    pub fn variant(&self) -> Option<&ModelVariantId> {
        self.variant.as_ref()
    }

    /// Audit provenance only. Nothing in this crate branches on it.
    pub fn catalog_version(&self) -> &ModelCatalogVersion {
        &self.catalog_version
    }

    /// Whether a live target carries the same provider/model/variant
    /// coordinates this summary is bound to.
    ///
    /// Coordinates only. The catalog version is excluded by construction, and
    /// this is deliberately NOT
    /// [`ModelRef::is_same_issuance`]: that predicate answers a process-local
    /// routing-authority question about two live refs, and demanding it here
    /// would make every provider-bound summary unredeemable after a restart —
    /// which is to say, after exactly the event compaction exists to survive.
    pub fn matches_target(&self, target: &ModelRef) -> bool {
        self.provider == *target.provider()
            && self.model == *target.model()
            && self.variant.as_ref() == target.variant()
    }
}

/// The outcome of redeeming a compaction summary against an exact target.
///
/// Closed on purpose. An `Option<ArtifactRef>` here would erase the difference
/// between "there is nothing to redeem" and "there is content that this exact
/// issuance may not replay", and §4.3 turns on precisely that difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryRedemption {
    /// The summary may be replayed against the requested target.
    Redeemable { artifact: ArtifactRef },
    /// A provider-bound summary cannot be replayed against this target.
    ///
    /// The previous durable history remains recoverable: refusing to redeem a
    /// summary settles the compaction with a typed failure and does not consume
    /// or rewrite the retained lineage the descriptor references.
    RefusedIncompatible(Box<IncompatibleSummary>),
}

/// The exact incompatibility behind a refused provider-bound summary.
///
/// Its coordinates are the two issuances the refusal is ABOUT: the one the
/// artifact is bound to, and the one redemption was attempted for. A refusal
/// that named neither would be a bare no, which is the thing §4.3 cannot use.
///
/// It is boxed into [`SummaryRedemption::RefusedIncompatible`] rather than
/// carried inline. A `ModelRef` is a sealed catalog issuance and two of them
/// made the refusal arm roughly five times the size of the success arm, so
/// every redemption call — including the common redeemable one — paid for the
/// refusal's payload. The indirection is on the refusal path only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompatibleSummary {
    bound: SummaryCompatibility,
    requested: SummaryCompatibility,
    failure: ContextFailure,
}

impl IncompatibleSummary {
    /// The compatibility coordinates the artifact is bound to, as stored.
    pub fn bound(&self) -> &SummaryCompatibility {
        &self.bound
    }

    /// The compatibility coordinates redemption was attempted for, derived
    /// from the live target.
    pub fn requested(&self) -> &SummaryCompatibility {
        &self.requested
    }

    /// The typed outcome the owner settles when it cannot recover.
    pub fn failure(&self) -> ContextFailure {
        self.failure
    }
}

impl SummaryRedemption {
    /// The redeemable artifact, if this target may replay it.
    ///
    /// Deliberately `None` on refusal: a caller that wants to continue must
    /// take the provider-neutral recovery path explicitly, not by unwrapping.
    pub fn artifact(&self) -> Option<&ArtifactRef> {
        match self {
            SummaryRedemption::Redeemable { artifact } => Some(artifact),
            SummaryRedemption::RefusedIncompatible(_) => None,
        }
    }

    /// The exact incompatibility, when this redemption refused.
    pub fn incompatibility(&self) -> Option<&IncompatibleSummary> {
        match self {
            SummaryRedemption::Redeemable { .. } => None,
            SummaryRedemption::RefusedIncompatible(refusal) => Some(refusal),
        }
    }

    /// The typed context failure to settle when recovery is not taken.
    pub fn failure(&self) -> Option<ContextFailure> {
        match self {
            SummaryRedemption::Redeemable { .. } => None,
            SummaryRedemption::RefusedIncompatible(refusal) => Some(refusal.failure()),
        }
    }

    /// Whether the provider-neutral strategy must rebuild this summary.
    pub fn needs_neutral_recovery(&self) -> bool {
        matches!(self, SummaryRedemption::RefusedIncompatible(_))
    }
}

/// How retained history was selected.
///
/// Either an explicit reference list or a deterministic descriptor that
/// reproduces the selection. Neither copies the history itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetainedHistory {
    /// Exact retained references.
    References { artifacts: Vec<ArtifactRef> },
    /// A deterministic selection that rebuilds the same set from durable
    /// truth, recorded instead of materializing it.
    DeterministicSelection { descriptor: ArtifactRef },
}

/// The one atomic durable descriptor a successful compaction commits.
///
/// Law §4.2: it references retained lineage and artifacts. It does **not**
/// inline duplicate copies of the complete retained history, and a bounded
/// materialized checkpoint may exist only as a rebuildable cache, never as an
/// independent source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionDescriptor {
    id: CompactionId,
    source_session: SessionId,
    source_epoch: ContextEpoch,
    target_window: ContextWindowId,
    target_epoch: ContextEpoch,
    trigger: CompactionTrigger,
    retention_policy: RetentionPolicyVersion,
    summary: CompactionSummary,
    retained: RetainedHistory,
    baseline: ArtifactRef,
    next_request: NextRequestRef,
}

/// The Step/request-context reference required to reassemble the next request.
///
/// Law §4.2 lists this among the coordinates one atomic compaction descriptor
/// must contain or reference: "Step/request-context reference required to
/// reassemble the next request". F4A landed the other nine and this one was
/// absent, which the F4B preflight recorded as a gap.
///
/// It is REQUIRED rather than optional, and that is the whole point. A
/// descriptor is the single atomic fact that ends one context epoch and starts
/// the next; if the coordinate needed to rebuild the next request could be
/// absent, a compaction could commit successfully and still leave the Session
/// unable to issue its next Step, with nothing typed to say why. Making it a
/// plain field of the parts struct means the compiler refuses a descriptor that
/// cannot reassemble, so absence is unrepresentable rather than merely
/// discouraged.
///
/// It binds two things because either alone is ambiguous: the [`StepNumber`]
/// says WHICH logical call position resumes, and the [`ArtifactRef`] is the
/// exact bounded prepared request-context to rebuild it from. The artifact is a
/// reference, never inline content — §4.2 forbids the descriptor from inlining
/// duplicate retained history, and that applies to the request context too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextRequestRef {
    step: StepNumber,
    context: ArtifactRef,
}

impl NextRequestRef {
    /// Bind the resuming Step to its exact prepared request-context artifact.
    pub fn new(step: StepNumber, context: ArtifactRef) -> Self {
        Self { step, context }
    }

    /// The logical call position the next request resumes at.
    pub fn step(&self) -> StepNumber {
        self.step
    }

    /// The exact bounded request-context artifact to rebuild from.
    pub fn context(&self) -> &ArtifactRef {
        &self.context
    }
}

/// Constructor inputs for [`CompactionDescriptor`], kept as a struct so the
/// ten coordinates cannot be transposed positionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionDescriptorParts {
    pub id: CompactionId,
    pub source_session: SessionId,
    pub source_epoch: ContextEpoch,
    pub target_window: ContextWindowId,
    pub target_epoch: ContextEpoch,
    pub trigger: CompactionTrigger,
    pub retention_policy: RetentionPolicyVersion,
    pub summary: CompactionSummary,
    pub retained: RetainedHistory,
    pub baseline: ArtifactRef,
    /// Law §4.2: the Step/request-context reference required to reassemble the
    /// next request. Required, so a descriptor that cannot reassemble is
    /// unrepresentable rather than merely discouraged.
    pub next_request: NextRequestRef,
}

impl CompactionDescriptor {
    /// Build a descriptor, refusing a target epoch that does not advance.
    ///
    /// Law §4.4: failure, cancellation, incompatible summary and **no-progress**
    /// are typed outcomes. A compaction that did not advance the epoch
    /// reclaimed nothing, so the refusal carries
    /// [`ContextFailure::CompactionNoProgress`] rather than a bare `None`.
    ///
    /// The error type is the existing terminal vocabulary on purpose: the
    /// caller can carry this value straight into
    /// `StepTerminalReason::ContextFailure` or
    /// `AttemptTerminalReason::ContextFailure` with no restatement, so the
    /// reason cannot be lost or re-derived from a bare absence at the seam.
    pub fn new(parts: CompactionDescriptorParts) -> Result<Self, ContextFailure> {
        if parts.target_epoch <= parts.source_epoch {
            return Err(ContextFailure::CompactionNoProgress);
        }
        Ok(Self {
            id: parts.id,
            source_session: parts.source_session,
            source_epoch: parts.source_epoch,
            target_window: parts.target_window,
            target_epoch: parts.target_epoch,
            trigger: parts.trigger,
            retention_policy: parts.retention_policy,
            summary: parts.summary,
            retained: parts.retained,
            baseline: parts.baseline,
            next_request: parts.next_request,
        })
    }

    pub fn id(&self) -> &CompactionId {
        &self.id
    }

    pub fn source_session(&self) -> &SessionId {
        &self.source_session
    }

    pub fn source_epoch(&self) -> ContextEpoch {
        self.source_epoch
    }

    pub fn target_window(&self) -> &ContextWindowId {
        &self.target_window
    }

    pub fn target_epoch(&self) -> ContextEpoch {
        self.target_epoch
    }

    pub fn trigger(&self) -> CompactionTrigger {
        self.trigger
    }

    pub fn retention_policy(&self) -> &RetentionPolicyVersion {
        &self.retention_policy
    }

    pub fn summary(&self) -> &CompactionSummary {
        &self.summary
    }

    pub fn retained(&self) -> &RetainedHistory {
        &self.retained
    }

    /// The Step/request-context reference that reassembles the next request.
    ///
    /// Law §4.2. Not optional: see [`NextRequestRef`].
    pub fn next_request(&self) -> &NextRequestRef {
        &self.next_request
    }

    pub fn baseline(&self) -> &ArtifactRef {
        &self.baseline
    }
}

/// How much a context contribution may be trusted.
///
/// Law E4: trust and provenance are mandatory contribution fields, not roles a
/// supplier can claim for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrustLabel {
    /// Authored by the user or the harness itself.
    Trusted,
    /// Read from the project workspace.
    ProjectContent,
    /// Retrieved from outside the workspace. Never authority.
    Untrusted,
}

/// Availability of one contribution's content.
///
/// Law §9: `Absent` means the contract proves no value exists; `Unavailable`
/// means it could not be loaded. Collapsing the two is how a missing required
/// input becomes an empty one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContributionState {
    /// The contract proves there is no value.
    Absent,
    /// A value should exist but could not be resolved.
    Unavailable { reason: ContributionUnavailable },
    /// Exact bounded content.
    Available { artifact: ArtifactRef },
}

/// Why a contribution could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContributionUnavailable {
    /// The source could not be read.
    SourceUnreadable,
    /// The content exceeded its declared bound.
    ExceededBound,
    /// Policy refused to admit it.
    PolicyRefused,
}

impl ContributionState {
    /// The exact content, if any.
    ///
    /// Deliberately `None` for both `Absent` and `Unavailable`: callers that
    /// need to tell them apart must match the variants.
    pub fn artifact(&self) -> Option<&ArtifactRef> {
        match self {
            ContributionState::Available { artifact } => Some(artifact),
            ContributionState::Absent | ContributionState::Unavailable { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
