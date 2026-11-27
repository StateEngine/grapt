//! R4D-2E — artifact custody, the run's own timeouts, and the cleanup
//! discipline a run on a shared host owes the seats it borrowed the host from.
//!
//! # Why custody is a type and not a checklist
//!
//! The attended tier's defining property is that its artifacts never pass
//! through `remote-compute`, so they inherit none of that profile's caps,
//! redaction or cleanup. Everything the governed profile would have done for
//! free has to be declared here instead — and a declaration that lives in prose
//! is a declaration nothing can refuse.
//!
//! Three of the laws in this module exist because breaking them has already
//! cost this campaign a run:
//!
//! * a receipt root **inside** the repository voids the run it was recording,
//!   because writing any file into the tree mid-battery fails the custody check
//!   the battery ends with — and the receipt-root default is inside the tree;
//! * a kill **by image name** on this host takes out other seats, because the
//!   `node` and `bash` processes here are the fleet, not the run;
//! * a cleanup list **derived from source** is provably incomplete, because the
//!   observed leftovers include a file with no constructor anywhere in the
//!   frozen tree.
//!
//! # What this module deliberately does NOT do
//!
//! It touches no filesystem. Nothing here opens, reads, writes, stats or deletes
//! a path: a [`CleanupInventory`] is a record of what an operator reported
//! observing, and [`ArtifactCustody`] is a declaration checked as a value. The
//! crate that eventually performs the deletion is not this one, and this module
//! exists so that when it does, the list it works from was already refused if it
//! was wrong.

use std::num::{NonZeroU32, NonZeroU64};

use crate::operator::OperatorRefusal;

/// What kind of thing a declared artifact is.
///
/// Closed, and the closure is the point: two of these variants are inadmissible,
/// and they exist so that an inadmissible artifact has to be **named** rather
/// than being unrepresentable. A law nothing can express is a law nothing can
/// witness — the same reasoning the landed [`crate::BoundProvenance`] applies to
/// its two inadmissible sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArtifactClass {
    /// The run's own receipt: window, caps observed, phases entered, outcomes.
    RunReceipt,
    /// A recorded owner mapping and its refusal, with no wire read in it.
    OwnerMapping,
    /// Status codes, header names and body **digests** for the exercised
    /// operations. Admissible precisely because it is shape and status, never
    /// content.
    ShapeAndStatusObservation,
    /// A cleanup enumeration: what was present, by path, with its mtime.
    CleanupEnumeration,
    /// Response body content from a session listing. **Inadmissible.** Session
    /// bodies are workspace state; comparing them would record a data
    /// difference as an engine difference, and retaining them would put a
    /// user's workspace content in a run receipt.
    SessionBodyContent,
    /// A service registration file. **Inadmissible.** Every registration
    /// observed on this host carries a `password` field, so copying one moves a
    /// credential into the evidence tree.
    CredentialBearingRegistration,
}

impl ArtifactClass {
    pub fn label(self) -> &'static str {
        match self {
            ArtifactClass::RunReceipt => "run-receipt",
            ArtifactClass::OwnerMapping => "owner-mapping",
            ArtifactClass::ShapeAndStatusObservation => "shape-and-status-observation",
            ArtifactClass::CleanupEnumeration => "cleanup-enumeration",
            ArtifactClass::SessionBodyContent => "session-body-content",
            ArtifactClass::CredentialBearingRegistration => "credential-bearing-registration",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        ArtifactClass::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// Whether an artifact of this class may be retained at all.
    ///
    /// Stated as a property of the class rather than checked at the call site,
    /// so a seventh class cannot be added without a reviewer deciding which side
    /// of this line it falls on.
    pub fn is_retainable(self) -> bool {
        match self {
            ArtifactClass::RunReceipt
            | ArtifactClass::OwnerMapping
            | ArtifactClass::ShapeAndStatusObservation
            | ArtifactClass::CleanupEnumeration => true,
            ArtifactClass::SessionBodyContent | ArtifactClass::CredentialBearingRegistration => {
                false
            }
        }
    }

    pub const ALL: [ArtifactClass; 6] = [
        ArtifactClass::RunReceipt,
        ArtifactClass::OwnerMapping,
        ArtifactClass::ShapeAndStatusObservation,
        ArtifactClass::CleanupEnumeration,
        ArtifactClass::SessionBodyContent,
        ArtifactClass::CredentialBearingRegistration,
    ];
}

/// One artifact a run declares it will retain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredArtifact {
    name: String,
    class: ArtifactClass,
    max_bytes: NonZeroU64,
}

impl DeclaredArtifact {
    /// Declare an artifact, or refuse it.
    ///
    /// Two laws, refused in this order: an unnamed artifact cannot be pulled
    /// before cleanup removes it, and an inadmissible class cannot be retained
    /// at any size. The class check runs second so a declaration wrong in both
    /// ways names the naming defect first — a nameless row cannot be reported
    /// against its class in any useful way.
    pub fn declare(
        name: impl Into<String>,
        class: ArtifactClass,
        max_bytes: u64,
    ) -> Result<Self, OperatorRefusal> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(OperatorRefusal::ArtifactUnnamed);
        }
        if !class.is_retainable() {
            return Err(OperatorRefusal::ArtifactClassNotRetainable { class });
        }
        let max_bytes = match NonZeroU64::new(max_bytes) {
            Some(value) => value,
            None => return Err(OperatorRefusal::ArtifactBudgetUnbounded { name }),
        };
        Ok(Self {
            name,
            class,
            max_bytes,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn class(&self) -> ArtifactClass {
        self.class
    }

    pub fn max_bytes(&self) -> NonZeroU64 {
        self.max_bytes
    }
}

/// Where a run writes what it keeps.
///
/// Carries the repository root alongside the receipt root because the law is a
/// *relation* between the two, and a receipt root that did not know which tree
/// it had to stay out of could not be checked at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptRoot {
    repository_root: String,
    receipt_root: String,
}

impl ReceiptRoot {
    /// Declare a receipt root outside the repository, or refuse it.
    ///
    /// The refusal exists because the default is wrong: the receipt-root flag
    /// defaults **inside** the tree, and a battery that writes any file into the
    /// repository while it runs fails its own post-run custody check — for a
    /// reason that has nothing to do with what it was measuring.
    pub fn declare(
        repository_root: impl Into<String>,
        receipt_root: impl Into<String>,
    ) -> Result<Self, OperatorRefusal> {
        let repository_root = repository_root.into();
        let receipt_root = receipt_root.into();
        if repository_root.trim().is_empty() || receipt_root.trim().is_empty() {
            return Err(OperatorRefusal::ReceiptRootUnnamed);
        }
        if is_under(&repository_root, &receipt_root) {
            return Err(OperatorRefusal::ReceiptRootInsideRepository {
                receipt_root: receipt_root.clone(),
            });
        }
        Ok(Self {
            repository_root,
            receipt_root,
        })
    }

    pub fn repository_root(&self) -> &str {
        &self.repository_root
    }

    pub fn receipt_root(&self) -> &str {
        &self.receipt_root
    }
}

/// Whether `candidate` lies inside `root`, compared by path COMPONENT.
///
/// # Why this is not `candidate.starts_with(root)`
///
/// A raw string prefix answers a different question. With the repository at
/// `C:\workspace\repo`, the sibling directory `C:\workspace\repo-receipts` starts
/// with the repository path and is **not** inside it — a string prefix would
/// refuse a perfectly legal receipt root, and the operator's recorded way of
/// getting past a refusal like that is to move the directory, which is a custody
/// change made to satisfy a gate.
///
/// Components also make the separator irrelevant, so a root written with forward
/// slashes and a candidate written with backslashes compare correctly rather
/// than by accident.
///
/// Comparison is ASCII-case-insensitive because these are Windows paths and
/// `C:\Workspace\repo` names the same directory as `C:\workspace\repo`. A
/// case-sensitive compare would report that directory as outside the repository,
/// which is the one answer this function must never get wrong.
fn is_under(root: &str, candidate: &str) -> bool {
    let root: Vec<&str> = split_components(root);
    let candidate: Vec<&str> = split_components(candidate);
    if root.is_empty() || candidate.len() < root.len() {
        return false;
    }
    root.iter()
        .zip(candidate.iter())
        .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

/// Path components, separator-agnostic and free of empty segments.
fn split_components(path: &str) -> Vec<&str> {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// The complete custody declaration for a run's retained artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactCustody {
    root: ReceiptRoot,
    artifacts: Vec<DeclaredArtifact>,
}

impl ArtifactCustody {
    /// The total byte ceiling this lane adopts for retained evidence.
    ///
    /// 1 MiB, adopted from the governed profile's `evidence.max_total_bytes`
    /// rather than invented here. The attended tier inherits none of that
    /// profile's machinery, but there is no reason for its evidence to be
    /// *larger* than what the governed tier considers a complete evidence set,
    /// and adopting the ruled number means one ceiling rather than two.
    ///
    /// It is also what forces the shape-and-status discipline to be real: a run
    /// that tried to retain session bodies would breach this long before it
    /// breached anything else.
    pub const MAX_TOTAL_BYTES: u64 = 1_048_576;

    /// Declare custody, or refuse it.
    ///
    /// Laws, in refusal order:
    ///
    /// 1. at least one artifact — a run that retains nothing has no evidence,
    ///    and "the run produced no artifacts" must not be reachable by omission;
    /// 2. no two artifacts share a name, because a cleanup that pulls "the"
    ///    artifact of a duplicated name pulls one and destroys the other;
    /// 3. the declared budgets sum within [`MAX_TOTAL_BYTES`](Self::MAX_TOTAL_BYTES).
    ///
    /// The sum is computed with checked arithmetic. A saturating or wrapping sum
    /// of attacker-shaped budgets could report a small total for an enormous
    /// declaration, which is the one way this check could pass while being
    /// wrong.
    ///
    /// # Why an unrepresentable total is its own refusal
    ///
    /// A declaration whose budgets do not fit in a `u64` at all is refused as
    /// [`CustodyBudgetNotRepresentable`](OperatorRefusal::CustodyBudgetNotRepresentable)
    /// rather than being reported as an ordinary over-ceiling declaration of
    /// `u64::MAX`. Two reasons, and the second is why the first is not merely
    /// tidiness:
    ///
    /// * `u64::MAX` is not what was declared. The declared total is larger than
    ///   any `u64`, and naming a number that was never declared puts a false
    ///   figure in a receipt whose whole purpose is to be checkable.
    /// * a **saturating** sum produces exactly that same over-ceiling refusal
    ///   carrying exactly that same `u64::MAX`. While the overflow arm reported
    ///   it too, substituting `saturating_add` for `checked_add` changed no
    ///   observable answer, so the hazard this doc names could not be
    ///   witnessed — it was an unfalsifiable claim. Refusing the two cases with
    ///   different words is what makes the checked arithmetic testable.
    pub fn declare(
        root: ReceiptRoot,
        artifacts: Vec<DeclaredArtifact>,
    ) -> Result<Self, OperatorRefusal> {
        if artifacts.is_empty() {
            return Err(OperatorRefusal::CustodyRetainsNothing);
        }
        for (index, artifact) in artifacts.iter().enumerate() {
            if artifacts[..index]
                .iter()
                .any(|earlier| earlier.name() == artifact.name())
            {
                return Err(OperatorRefusal::ArtifactDeclaredTwice {
                    name: artifact.name().to_owned(),
                });
            }
        }

        let mut total: u64 = 0;
        for artifact in &artifacts {
            total = match total.checked_add(artifact.max_bytes().get()) {
                Some(value) => value,
                None => {
                    return Err(OperatorRefusal::CustodyBudgetNotRepresentable {
                        name: artifact.name().to_owned(),
                        preceding: total,
                    })
                }
            };
        }
        if total > Self::MAX_TOTAL_BYTES {
            return Err(OperatorRefusal::CustodyBudgetExceeded {
                declared: total,
                ceiling: Self::MAX_TOTAL_BYTES,
            });
        }

        Ok(Self { root, artifacts })
    }

    pub fn root(&self) -> &ReceiptRoot {
        &self.root
    }

    pub fn artifacts(&self) -> &[DeclaredArtifact] {
        &self.artifacts
    }

    /// The declared byte total, recomputed rather than stored.
    ///
    /// Stored totals drift from the list they summarise the first time anyone
    /// edits the list. Recomputing costs nothing at these sizes and cannot
    /// disagree with the artifacts it is derived from.
    pub fn declared_bytes(&self) -> u64 {
        self.artifacts.iter().fold(0u64, |total, artifact| {
            total.saturating_add(artifact.max_bytes().get())
        })
    }
}

/// How a run may stop a process it started.
///
/// One admissible variant, and the inadmissible one is named rather than
/// omitted so that it can be refused explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillDiscipline {
    /// Stop exactly the process this run recorded starting, by its own PID.
    PidScoped(u32),
    /// Stop every process with a given image name. **Inadmissible on this
    /// host**: the 52 `node` and 30 `bash` processes measured here are other
    /// seats, and an image-name kill takes the fleet out with the run.
    ByImageName(String),
}

impl KillDiscipline {
    /// Admit a kill discipline, or refuse it.
    ///
    /// A zero PID is refused separately from an image-name kill: they are
    /// different defects. Zero is not a process this run started, and recording
    /// it would make the cleanup unattributable rather than dangerous.
    pub fn admit(self) -> Result<Self, OperatorRefusal> {
        match &self {
            KillDiscipline::ByImageName(image) => Err(OperatorRefusal::ImageNameKillRefused {
                image: image.clone(),
            }),
            KillDiscipline::PidScoped(0) => Err(OperatorRefusal::KillTargetNotAProcess),
            KillDiscipline::PidScoped(_) => Ok(self),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            KillDiscipline::PidScoped(_) => "pid-scoped",
            KillDiscipline::ByImageName(_) => "by-image-name",
        }
    }
}

/// The run's own time budget, in milliseconds.
///
/// Distinct from [`crate::TierCaps`]: the caps are what the *tier* permits, this
/// is what a *plan* declares for itself. A plan may be tighter than its tier and
/// may never be looser, which is the relation [`declare`](Self::declare) checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutBudget {
    whole_run_ms: NonZeroU64,
    per_matrix_ms: NonZeroU64,
}

impl TimeoutBudget {
    /// Declare a budget, or refuse it.
    ///
    /// Four laws, each with its own refusal:
    ///
    /// * neither bound is zero — an unbounded run is not a bounded one;
    /// * the per-matrix bound is strictly smaller than the whole-run bound, so
    ///   reversal and cleanup have somewhere to happen;
    /// * neither bound exceeds the tier's cap.
    ///
    /// The tier cap is a parameter rather than a lookup so this function has no
    /// opinion about which tier it is serving; the plan that owns both supplies
    /// the pairing.
    ///
    /// # Disclosed limitation: the pairing is not re-checked by the plan
    ///
    /// An earlier version of this doc said the plan is where a tier mismatch is
    /// refused. It is not: [`crate::OperatorPlan::declare`] performs no such
    /// check, and neither this budget nor [`EngineProcessCount`] records which
    /// tier's caps it was measured against.
    ///
    /// The mismatch is **currently unrepresentable** rather than merely
    /// unchecked. `TierCaps` has private fields and exactly two ways out —
    /// [`crate::TierCaps::ATTENDED`] and [`crate::TierCaps::for_tier`] — and
    /// `for_tier` answers `None` for both other tiers, which
    /// `OperatorPlan::declare` refuses before it reaches any component. Every
    /// cap value that can reach this function today is therefore the attended
    /// one, and a re-binding guard would be a refusal no input could produce.
    /// This lane does not ship one: an unreachable guard is a law that cannot
    /// be witnessed, and shipping it would put an unfalsifiable claim in a
    /// contract whose whole point is that its claims are falsifiable.
    ///
    /// The limitation becomes live the moment a second tier carries caps. The
    /// lane that adds one owns the binding — it must make each component record
    /// its tier and make the plan refuse a component measured against another
    /// tier's numbers, with a witness that a real input can reach.
    pub fn declare(
        whole_run_ms: u64,
        per_matrix_ms: u64,
        cap: crate::TierCaps,
    ) -> Result<Self, OperatorRefusal> {
        let whole_run = match NonZeroU64::new(whole_run_ms) {
            Some(value) => value,
            None => return Err(OperatorRefusal::RunTimeoutUnbounded),
        };
        let per_matrix = match NonZeroU64::new(per_matrix_ms) {
            Some(value) => value,
            None => return Err(OperatorRefusal::MatrixTimeoutUnbounded),
        };
        if whole_run.get() > cap.whole_run_ms().get() {
            return Err(OperatorRefusal::TimeoutExceedsTierCap {
                declared_ms: whole_run.get(),
                cap_ms: cap.whole_run_ms().get(),
            });
        }
        if per_matrix.get() > cap.per_matrix_ms().get() {
            return Err(OperatorRefusal::TimeoutExceedsTierCap {
                declared_ms: per_matrix.get(),
                cap_ms: cap.per_matrix_ms().get(),
            });
        }
        if per_matrix.get() >= whole_run.get() {
            return Err(OperatorRefusal::MatrixTimeoutLeavesNoRoomForCleanup {
                per_matrix_ms: per_matrix.get(),
                whole_run_ms: whole_run.get(),
            });
        }
        Ok(Self {
            whole_run_ms: whole_run,
            per_matrix_ms: per_matrix,
        })
    }

    pub fn whole_run_ms(self) -> NonZeroU64 {
        self.whole_run_ms
    }

    pub fn per_matrix_ms(self) -> NonZeroU64 {
        self.per_matrix_ms
    }
}

/// Which cap ended a matrix that did not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapThatFired {
    /// The whole-run wall clock.
    WholeRun,
    /// The per-matrix wall clock.
    PerMatrix,
    /// Host free physical memory fell below the floor that protects the seats.
    HostFreeFloor,
}

impl CapThatFired {
    pub fn label(self) -> &'static str {
        match self {
            CapThatFired::WholeRun => "whole-run",
            CapThatFired::PerMatrix => "per-matrix",
            CapThatFired::HostFreeFloor => "host-free-floor",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        CapThatFired::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    pub const ALL: [CapThatFired; 3] = [
        CapThatFired::WholeRun,
        CapThatFired::PerMatrix,
        CapThatFired::HostFreeFloor,
    ];
}

/// What a matrix produced.
///
/// # Why `Inconclusive` is a variant and not a flavour of failure
///
/// A matrix stopped by a cap measured nothing. Recording it as a failure blames
/// the engine for the operator's clock; recording it as a pass claims a
/// comparison that never ran. Both are false in the one place the matrix exists
/// to be true, so the third state is typed — and it carries **which** cap fired,
/// because a run that stopped on the whole-run clock and a run that stopped
/// because the host ran out of memory are different observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatrixOutcome {
    /// The matrix ran and its plan was admitted.
    Admitted,
    /// A cap fired before the matrix finished.
    Inconclusive { cap: CapThatFired },
    /// The contract refused, naming the law. The label is the landed
    /// vocabulary's own word, never a new spelling.
    Refused { label: String },
}

impl MatrixOutcome {
    /// Whether this outcome may be read as evidence that the engines agreed.
    ///
    /// Only [`Admitted`](Self::Admitted) may. Both other variants return false,
    /// which is the whole reason the distinction is typed: a projection that
    /// counted inconclusive matrices as passes would report a green run that
    /// measured nothing.
    pub fn is_evidence_of_agreement(&self) -> bool {
        matches!(self, MatrixOutcome::Admitted)
    }

    /// Whether this outcome blames the engine.
    ///
    /// An inconclusive matrix does not. This is the dual of
    /// [`is_evidence_of_agreement`](Self::is_evidence_of_agreement) and it is
    /// stated separately because "not a pass" and "a failure" are different
    /// facts, and collapsing them is how a timed-out run becomes a bug report
    /// against an engine that was never measured.
    pub fn blames_the_engine(&self) -> bool {
        matches!(self, MatrixOutcome::Refused { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            MatrixOutcome::Admitted => "admitted",
            MatrixOutcome::Inconclusive { .. } => "inconclusive",
            MatrixOutcome::Refused { .. } => "refused",
        }
    }
}

/// A path an operator reported observing at cleanup time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedPath {
    path: String,
    created_by_this_run: bool,
}

impl ObservedPath {
    pub fn new(path: impl Into<String>, created_by_this_run: bool) -> Self {
        Self {
            path: path.into(),
            created_by_this_run,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn created_by_this_run(&self) -> bool {
        self.created_by_this_run
    }
}

/// What cleanup found, and what it is allowed to remove.
///
/// # Why the inventory is enumerated and never derived
///
/// A cleanup list computed from the frozen source is provably incomplete: the
/// leftover `.stale-<pid>` registration observed on this host has **no
/// constructor anywhere in the frozen tree** — it was written by the packaged
/// native CLI, outside the tree the derivation would have read. A derived list
/// would have reported a clean state home while that file sat in it.
///
/// So the operator enumerates, and this type records the enumeration. Its one
/// law is about deletion, not discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupInventory {
    observed: Vec<ObservedPath>,
}

impl CleanupInventory {
    /// Record an enumeration.
    ///
    /// Total: an empty state home is a legitimate observation, and refusing it
    /// would push an operator toward inventing a row. What is refused is
    /// *deleting* something that was not recorded as this run's, which is
    /// [`removable`](Self::removable)'s job.
    pub fn enumerate(observed: Vec<ObservedPath>) -> Self {
        Self { observed }
    }

    pub fn observed(&self) -> &[ObservedPath] {
        &self.observed
    }

    /// The paths this run may remove: exactly those it recorded creating.
    ///
    /// Everything else is recorded and left. The state home is shared with live
    /// seats and this run does not own it — a cleanup that removed an
    /// unrecognised file would be deleting another seat's state to tidy up after
    /// itself.
    pub fn removable(&self) -> Vec<&ObservedPath> {
        self.observed
            .iter()
            .filter(|entry| entry.created_by_this_run())
            .collect()
    }

    /// Check a proposed deletion against the enumeration.
    ///
    /// Refuses a path that was never observed *and* a path observed but not
    /// created by this run, with the same refusal: from the run's point of view
    /// they are one law — you may delete what you recorded creating, and nothing
    /// else.
    pub fn admit_deletion(&self, path: &str) -> Result<(), OperatorRefusal> {
        let recorded = self
            .observed
            .iter()
            .any(|entry| entry.path() == path && entry.created_by_this_run());
        if recorded {
            Ok(())
        } else {
            Err(OperatorRefusal::DeletionOfUnrecordedPath {
                path: path.to_owned(),
            })
        }
    }
}

/// A count of engine processes a run declares it will start.
///
/// Wrapped rather than passed as a bare `u32` so the tier's "exactly two"
/// cap has something to be compared against that cannot be confused with any
/// other count in the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineProcessCount(NonZeroU32);

impl EngineProcessCount {
    /// Declare a count equal to the tier's cap, or refuse it.
    ///
    /// The cap is exact rather than an upper bound in one direction only: a run
    /// that started *fewer* than two engines is not running a differential at
    /// all, and one that started more makes attribution a guess. Both are
    /// refused, and they are refused with different words.
    ///
    /// # Why the lower bound counts processes and not engines that answer
    ///
    /// The ruling of 2026-08-14 settles what "two engines" means: an
    /// attended differential requires exactly two engine **processes in total**,
    /// both started by the operator inside the attended window. A retained
    /// engine that was already running is not a second engine for this purpose,
    /// so declaring one process is not a differential minus a formality — it is
    /// a comparison with nothing to compare against, and `1` is refused for
    /// exactly the same reason `0` is.
    ///
    /// The two refusals below are therefore the whole law, and the count is
    /// non-zero because it equals a [`NonZeroU32`] cap rather than because a
    /// third check says so: after both bounds have passed, `count` is
    /// `required`, and re-testing it for zero would add an arm no input could
    /// ever reach.
    pub fn declare(count: u32, cap: crate::TierCaps) -> Result<Self, OperatorRefusal> {
        let required = cap.engine_processes();
        if count < required.get() {
            return Err(OperatorRefusal::TooFewEnginesForDifferential {
                declared: count,
                required: required.get(),
            });
        }
        if count > required.get() {
            return Err(OperatorRefusal::TooManyEnginesForAttribution {
                declared: count,
                cap: required.get(),
            });
        }
        Ok(Self(required))
    }

    pub fn get(self) -> NonZeroU32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sibling_directory_sharing_the_repository_prefix_is_not_inside_it() {
        assert!(
            !is_under("C:\\workspace\\repo", "C:\\workspace\\repo-receipts"),
            "a string prefix would refuse a legal receipt root; components must not"
        );
        assert!(
            is_under("C:\\workspace\\repo", "C:\\workspace\\repo\\receipts"),
            "a genuine child must still be recognised"
        );
    }

    #[test]
    fn the_separator_and_the_ascii_case_do_not_change_the_answer() {
        assert!(
            is_under("C:/workspace/repo", "C:\\Workspace\\REPO\\out"),
            "these name one Windows directory; reporting otherwise is the answer this must never get wrong"
        );
    }

    #[test]
    fn a_shorter_candidate_is_never_inside_a_longer_root() {
        assert!(!is_under("C:\\workspace\\repo", "C:\\workspace"));
        assert!(!is_under("C:\\workspace\\repo", ""));
    }

    #[test]
    fn every_artifact_class_label_round_trips() {
        for class in ArtifactClass::ALL {
            assert_eq!(ArtifactClass::from_label(class.label()), Some(class));
        }
    }

    #[test]
    fn every_cap_label_round_trips() {
        for cap in CapThatFired::ALL {
            assert_eq!(CapThatFired::from_label(cap.label()), Some(cap));
        }
    }

    #[test]
    fn an_inconclusive_matrix_is_neither_agreement_nor_a_charge_against_the_engine() {
        let timed_out = MatrixOutcome::Inconclusive {
            cap: CapThatFired::PerMatrix,
        };
        assert!(
            !timed_out.is_evidence_of_agreement(),
            "a matrix stopped by a cap measured nothing and cannot show agreement"
        );
        assert!(
            !timed_out.blames_the_engine(),
            "a matrix stopped by the operator's clock is not a charge against the engine"
        );
    }
}
