use super::{
    CheckpointFidelity, CheckpointLaneId, CheckpointOperationId, CheckpointProviderVersion,
    CheckpointRestorePlanId, WorkspaceCheckpointRef,
};
#[cfg(doc)]
#[allow(unused_imports)]
use super::{RestoreApplicationAdmission, RestoreRefusalReason, RestoreRefusalReceipt};
use crate::authority::{DeterministicAuthorities, Timestamp};
use crate::capability::{CheckpointRestoreCapabilityId, RealizedEnforcement};
use crate::recovery::{RecoveryAnchor, RecoveryAnchorId, WorktreeIdentity};

// ---------------------------------------------------------------------------
// The governed restore application boundary
// ---------------------------------------------------------------------------

/// The exact counted consequences a restore plan binds.
///
/// Part of plan identity, not advisory metadata: a plan that was authorized for
/// three writes is not the plan that deletes three hundred files.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RestoreConsequences {
    writes: u64,
    deletes: u64,
    bytes: u64,
}

impl RestoreConsequences {
    pub fn new(writes: u64, deletes: u64, bytes: u64) -> Self {
        Self {
            writes,
            deletes,
            bytes,
        }
    }

    pub fn writes(&self) -> u64 {
        self.writes
    }

    pub fn deletes(&self) -> u64 {
        self.deletes
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// An unusual condition the planner found and the settling authority answered
/// over.
///
/// Closed: an unusual condition nobody named is not an unusual condition the
/// authority agreed to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UnusualRestoreCondition {
    /// This provider cannot capture a rescue for this root, so the restore has
    /// no pre-write undo. See [`RestoreRescue::waived_as_unsupported`].
    ProviderRescueUnsupported,
    /// The worktree holds changes that were never captured anywhere.
    UncommittedChangesPresent,
    /// The plan deletes paths that no selected lane can restore.
    DeletesOutsideCapturedLanes,
    /// The provider contract version moved since the checkpoint was captured.
    ProviderVersionChangedSinceCapture { captured: CheckpointProviderVersion },
}

/// A provider-side condition that must hold when the plan is applied.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RestorePrecondition {
    /// The provider must still be serving this exact contract version.
    ProviderVersion { version: CheckpointProviderVersion },
    /// The named lane must still be negotiated.
    LaneNegotiated { lane: CheckpointLaneId },
    /// No other governed operation may be writing the root.
    WorktreeQuiescent,
    /// The target workspace must still have the exact VCS baseline observed
    /// when this plan's consequences were computed.
    VcsBaseline { baseline: crate::vcs::VcsBaseline },
}

/// The coordinates a settled restore plan is minted from.
///
/// A doc-hidden carrier for the separate `grapt-checkpoint` owner. Rust crate
/// privacy cannot span two crates, so the workspace architecture gate pins the
/// sole production call to that owner. It exists so the boundary does not take
/// a dozen positional arguments that are easy to transpose.
#[doc(hidden)]
#[derive(Debug)]
pub struct SettledRestorePlanParts {
    pub plan: CheckpointRestorePlanId,
    pub operation: CheckpointOperationId,
    pub checkpoint: WorkspaceCheckpointRef,
    pub root: WorktreeIdentity,
    pub lanes: Vec<CheckpointLaneId>,
    pub consequences: RestoreConsequences,
    pub unusual: Vec<UnusualRestoreCondition>,
    pub preconditions: Vec<RestorePrecondition>,
    pub enforcement: RealizedEnforcement,
    pub expires_at: Timestamp,
}

/// A settled, authorized restore plan: the `Planned`+`Authorized` state of the
/// lifecycle, carrying every coordinate the law says a plan binds.
///
/// Law: "The restore plan binds provider/version, checkpoint, root identity,
/// selected lanes, writes/deletes/bytes, unusual conditions and provider
/// preconditions. **Applying a different plan is unnameable.**"
///
/// Unnameable is enforced structurally rather than by comparison:
///
/// - every plan field is private, so a plan cannot be assembled or edited;
/// - the doc-hidden mint is called exactly once in production, by
///   `grapt-checkpoint`; its architecture gate refuses a second owner;
/// - it is **not `Clone`**, so one settlement applies at most once; and
/// - [`SettledRestorePlan::apply`] takes it **by value** and takes no separate
///   checkpoint, root or lane argument. There is nothing to swap: the entry
///   reads every coordinate out of the plan it consumed.
///
/// `Debug` is derived rather than redacted, deliberately. Unlike
/// [`crate::authority::LivePermissionDecision`], a plan carries no secret and no
/// process-local authority material; its force comes from being unforgeable, not
/// from being unprintable, and a reviewer reading a [`RestoreRefusalReceipt`]
/// or an [`RestoreApplicationAdmission`] must be able to see exactly which plan
/// it names.
///
/// # Owning boundary
///
/// The live route belongs to `grapt-checkpoint`, which is the only component
/// that can enumerate a provider's real consequences and negotiate its lanes.
/// The carrier is public solely to cross that crate boundary; treating it as a
/// general builder is an architecture-gate failure.
#[derive(Debug, PartialEq, Eq)]
pub struct SettledRestorePlan {
    pub(super) plan: CheckpointRestorePlanId,
    operation: CheckpointOperationId,
    checkpoint: WorkspaceCheckpointRef,
    root: WorktreeIdentity,
    lanes: Vec<CheckpointLaneId>,
    consequences: RestoreConsequences,
    unusual: Vec<UnusualRestoreCondition>,
    pub(super) preconditions: Vec<RestorePrecondition>,
    enforcement: RealizedEnforcement,
    pub(super) required_capability: CheckpointRestoreCapabilityId,
    pub(super) settled_at: Timestamp,
    pub(super) expires_at: Timestamp,
}

impl SettledRestorePlan {
    /// Mint a settled plan at the external-provider owner boundary.
    ///
    /// Refuses the states the law says cannot exist:
    ///
    /// - a plan that selects no lane. An empty selection restores nothing, so
    ///   it is not an exact consequence set; it must not be able to stand in
    ///   for a plan that was authorized over real lanes.
    /// - a plan whose expiry is not in the future. `Planned` carries an expiry
    ///   because consequences go stale; a plan born expired is a plan whose
    ///   consequences were never true.
    ///
    /// The required capability is **derived here from the plan's own resolved
    /// coordinates**, never accepted as an argument, so a planner cannot attach
    /// a cheap capability to an expensive restore.
    #[doc(hidden)]
    pub fn settle_at_provider_boundary(
        parts: SettledRestorePlanParts,
        authorities: &DeterministicAuthorities<'_>,
    ) -> Option<Self> {
        if parts.lanes.is_empty() {
            return None;
        }
        let settled_at = authorities.now();
        if parts.expires_at <= settled_at {
            return None;
        }
        let mut lanes = parts.lanes;
        lanes.sort();
        lanes.dedup();
        // `lanes` above is canonicalised by sort-then-dedup. This list cannot
        // be, because `UnusualRestoreCondition` is deliberately not `Ord` — it
        // is a closed vocabulary, not a sortable identity — and `Vec::dedup`
        // alone removes only CONSECUTIVE duplicates, so `[A, B, A]` would keep
        // both `A`s. Dedupe by membership instead, preserving first-occurrence
        // order so the receipt reads in the order the planner reported.
        let mut unusual: Vec<UnusualRestoreCondition> = Vec::new();
        for condition in parts.unusual {
            if !unusual.contains(&condition) {
                unusual.push(condition);
            }
        }
        // ITEM 2. This used to derive `FilesystemWrite + root + enforcement`,
        // which is *generic filesystem authority*: every plan touching the same
        // root derived the SAME capability, so a lease approved for a two-file
        // restore authorized a plan that deleted the tree. Authority for a
        // destructive external effect must name the exact effect, so the
        // identity is derived from the plan's complete canonical form —
        // checkpoint provider/version/id, root, lanes, writes/deletes/bytes,
        // unusual conditions, preconditions and expiry. Change any of them and
        // the plan requires a different capability, which is a different
        // approval.
        let required_capability = CheckpointRestoreCapabilityId::from_canonical(
            canonical_restore_plan_form(&CanonicalPlanCoordinates {
                plan: &parts.plan,
                operation: &parts.operation,
                checkpoint: &parts.checkpoint,
                root: &parts.root,
                lanes: &lanes,
                consequences: &parts.consequences,
                unusual: &unusual,
                preconditions: &parts.preconditions,
                enforcement: &parts.enforcement,
                expires_at: parts.expires_at,
            }),
        );
        Some(Self {
            plan: parts.plan,
            operation: parts.operation,
            checkpoint: parts.checkpoint,
            root: parts.root,
            lanes,
            consequences: parts.consequences,
            unusual,
            preconditions: parts.preconditions,
            enforcement: parts.enforcement,
            required_capability,
            settled_at,
            expires_at: parts.expires_at,
        })
    }

    pub fn plan(&self) -> &CheckpointRestorePlanId {
        &self.plan
    }

    pub fn operation(&self) -> &CheckpointOperationId {
        &self.operation
    }

    pub fn checkpoint(&self) -> &WorkspaceCheckpointRef {
        &self.checkpoint
    }

    pub fn root(&self) -> &WorktreeIdentity {
        &self.root
    }

    pub fn lanes(&self) -> &[CheckpointLaneId] {
        &self.lanes
    }

    pub fn consequences(&self) -> &RestoreConsequences {
        &self.consequences
    }

    pub fn unusual_conditions(&self) -> &[UnusualRestoreCondition] {
        &self.unusual
    }

    pub fn preconditions(&self) -> &[RestorePrecondition] {
        &self.preconditions
    }

    pub fn enforcement(&self) -> &RealizedEnforcement {
        &self.enforcement
    }

    /// The exact capability a lease must name to apply this plan.
    ///
    /// Derived from the plan's resolved coordinates, so it is comparable but
    /// never authority by itself: only the checkpoint owner's explicit
    /// authority port can produce the one-shot lease that names it.
    pub fn required_capability(&self) -> &CheckpointRestoreCapabilityId {
        &self.required_capability
    }

    pub fn settled_at(&self) -> Timestamp {
        self.settled_at
    }

    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }

    /// Whether this plan restores exactly `checkpoint`.
    ///
    /// Takes a [`WorkspaceCheckpointRef`] and nothing else: ownership probe #12
    /// depends on a Git commit, artifact, compaction, Session-fork or
    /// restore-plan identity being unable to reach this question.
    pub fn restores(&self, checkpoint: &WorkspaceCheckpointRef) -> bool {
        &self.checkpoint == checkpoint
    }

    /// Whether the settling authority recorded `condition` for this plan.
    pub fn records_unusual(&self, condition: &UnusualRestoreCondition) -> bool {
        self.unusual.iter().any(|known| known == condition)
    }
}

/// What proved that a rescue existed before the first destructive write.
///
/// Recorded in the receipt (§8.5). This is *evidence*, freely copyable and
/// inspectable; the unforgeable thing is [`RestoreRescue`], which is what
/// [`SettledRestorePlan::apply`] demands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreRescueEvidence {
    /// A rescue checkpoint was captured for this root before the restore.
    Captured {
        anchor: RecoveryAnchorId,
        checkpoint: WorkspaceCheckpointRef,
        fidelity: CheckpointFidelity,
    },
    /// The settled plan itself records that this provider cannot rescue this
    /// root, and the authority settled the plan with that condition visible.
    WaivedProviderUnsupported,
}

/// The rescue an application is entered under.
///
/// Deliberately *not* an `Option` on the apply entry: "rescue happened" and
/// "rescue was impossible and the authority knew" are different facts, and an
/// absent argument would express neither. Both routes below are sealed:
///
/// - `captured` is `pub(crate)`, owned by the checkpoint provider adapter (see
///   the later-lane receipt on [`SettledRestorePlan`]); only the component that
///   actually performed the capture can assert it happened.
/// - [`RestoreRescue::waived_as_unsupported`] is public, but sealed by its
///   input: it returns `Some` only when the *unforgeable settled plan* records
///   [`UnusualRestoreCondition::ProviderRescueUnsupported`]. A caller cannot
///   waive rescue for a plan whose authority never saw that condition.
///
/// Each value is bound to one plan id, so a rescue proven for a cheap plan
/// cannot admit an expensive one — [`SettledRestorePlan::apply`] refuses with
/// [`RestoreRefusalReason::RescueProvenForAnotherPlan`].
#[derive(Debug)]
pub struct RestoreRescue {
    pub(super) plan: CheckpointRestorePlanId,
    pub(super) evidence: RestoreRescueEvidence,
}

impl RestoreRescue {
    /// Assert that a rescue capture completed for this plan's root.
    ///
    /// Doc-hidden owner seam: asserting that a capture happened is the
    /// checkpoint adapter's job. The workspace gate pins its only production
    /// call to `grapt-checkpoint`, where an actual capture receipt is required.
    ///
    /// Refuses two incoherent captures rather than recording them:
    ///
    /// 1. an anchor whose checkpoint **is** the checkpoint being restored. That
    ///    is the restore source, not a rescue of the state about to be
    ///    overwritten.
    /// 2. an anchor captured against a **different worktree** than the plan
    ///    overwrites. Relinking mints a different [`WorktreeIdentity`], so a
    ///    rescue of another tree cannot cover this one.
    #[doc(hidden)]
    pub fn captured_at_provider_boundary(
        plan: &SettledRestorePlan,
        anchor: &RecoveryAnchor,
    ) -> Option<Self> {
        if anchor.checkpoint() == plan.checkpoint() {
            return None;
        }
        if anchor.worktree() != plan.root() {
            return None;
        }
        Some(Self {
            plan: plan.plan().clone(),
            evidence: RestoreRescueEvidence::Captured {
                anchor: anchor.id().clone(),
                checkpoint: anchor.checkpoint().clone(),
                fidelity: anchor.fidelity().clone(),
            },
        })
    }

    /// Waive rescue because the settled plan records that the provider cannot
    /// perform one for this root.
    ///
    /// `None` for every other plan. The waiver is not a decision this function's
    /// caller makes: it is read out of a plan the authority already settled with
    /// [`UnusualRestoreCondition::ProviderRescueUnsupported`] visible.
    pub fn waived_as_unsupported(plan: &SettledRestorePlan) -> Option<Self> {
        if !plan.records_unusual(&UnusualRestoreCondition::ProviderRescueUnsupported) {
            return None;
        }
        Some(Self {
            plan: plan.plan().clone(),
            evidence: RestoreRescueEvidence::WaivedProviderUnsupported,
        })
    }

    /// The plan this rescue was proven for.
    pub fn plan(&self) -> &CheckpointRestorePlanId {
        &self.plan
    }

    pub fn evidence(&self) -> &RestoreRescueEvidence {
        &self.evidence
    }
}

/// Every coordinate that makes one restore plan a different governed effect
/// from another.
struct CanonicalPlanCoordinates<'a> {
    plan: &'a CheckpointRestorePlanId,
    operation: &'a CheckpointOperationId,
    checkpoint: &'a WorkspaceCheckpointRef,
    root: &'a WorktreeIdentity,
    lanes: &'a [CheckpointLaneId],
    consequences: &'a RestoreConsequences,
    unusual: &'a [UnusualRestoreCondition],
    preconditions: &'a [RestorePrecondition],
    enforcement: &'a RealizedEnforcement,
    expires_at: Timestamp,
}

/// Deterministic canonical encoding of a settled restore plan.
///
/// Separators are escaped so two distinct plans cannot encode alike — the same
/// rule `canonical_target_form` follows in `model.rs`. The closed vocabularies
/// are rendered through their derived `Debug`, which is deterministic for a
/// fieldless-or-named closed enum and is the precedent already accepted in
/// `campaign.rs` for ordering stop conditions; the identity-bearing coordinates
/// are rendered from their exact accessors rather than `Debug`.
fn canonical_restore_plan_form(c: &CanonicalPlanCoordinates<'_>) -> String {
    fn push(out: &mut String, label: &str, value: &str) {
        out.push_str(label);
        out.push('=');
        for ch in value.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '|' => out.push_str("\\p"),
                '=' => out.push_str("\\e"),
                ',' => out.push_str("\\c"),
                other => out.push(other),
            }
        }
        out.push('|');
    }

    let mut out = String::from("restore-plan|");
    push(&mut out, "plan", c.plan.as_str());
    push(&mut out, "operation", c.operation.as_str());
    push(&mut out, "provider", c.checkpoint.provider().as_str());
    push(&mut out, "version", c.checkpoint.version().as_str());
    push(&mut out, "checkpoint", c.checkpoint.checkpoint().as_str());
    push(&mut out, "root", &format!("{:?}", c.root));
    let mut lanes = String::new();
    for lane in c.lanes {
        lanes.push_str(lane.as_str());
        lanes.push(',');
    }
    push(&mut out, "lanes", &lanes);
    push(&mut out, "writes", &c.consequences.writes().to_string());
    push(&mut out, "deletes", &c.consequences.deletes().to_string());
    push(&mut out, "bytes", &c.consequences.bytes().to_string());
    let mut unusual = String::new();
    for condition in c.unusual {
        unusual.push_str(&format!("{condition:?}"));
        unusual.push(',');
    }
    push(&mut out, "unusual", &unusual);
    let mut preconditions = String::new();
    for precondition in c.preconditions {
        preconditions.push_str(&format!("{precondition:?}"));
        preconditions.push(',');
    }
    push(&mut out, "preconditions", &preconditions);
    push(&mut out, "enforcement", &format!("{:?}", c.enforcement));
    push(&mut out, "expires", &c.expires_at.as_millis().to_string());
    out
}
