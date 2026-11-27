//! R4D-2A — the retained-desktop differential **driver contract**.
//!
//! # What this module is, and what it deliberately is not
//!
//! This is the contract a differential driver must satisfy *before* it may
//! claim it compared two engines. It is not the driver. Nothing here opens a
//! socket, starts a process, serves a route, drives the desktop, or measures a
//! running engine — and no value in this module asserts that any engine was
//! ever contacted.
//!
//! That boundary is not modesty, it is the finding that produced this lane. At
//! the current canonical the live differential cannot execute: the workspace
//! declares no binary, the host serves no product route (not even `/health`),
//! and a production build answers `401` to every request by ruling. Those are
//! three separate landings away and none is this lane's to make. What *is*
//! authorable now is the contract — the matrices, their exact minimum operation
//! subsets, the admissibility laws, the restart and reversibility laws, and the
//! shape of the receipt — expressed against the landed vocabulary, so that the
//! driver, when it becomes possible, cannot quietly claim parity it did not
//! measure.
//!
//! # It consumes R4D-1's vocabulary rather than restating it
//!
//! Attachment-level law already landed. [`ProbeOutcome`], [`SelectionReceipt`],
//! [`AdmittedDifferential`], [`SelectorState`] and — critically —
//! [`AttachmentRefusal`] are R4D-1's, and this module **consumes** them. The
//! four attachment refusals this lane's admission names (health-only,
//! stale-selector, reused-PID and same-owner) are exactly
//! [`AttachmentRefusal::Unattributed`], [`AttachmentRefusal::NotTheRecordedOwner`]
//! (twice — a reused PID and a stale selector are the same law seen from two
//! sides) and [`AttachmentRefusal::SameOwnerOnBothSides`]. None of them is
//! re-spelled here.
//!
//! [`DifferentialRefusal::Attachment`] carries the landed refusal verbatim and
//! [`DifferentialRefusal::label`] delegates to it, so the recorded corpus shows
//! R4D-1's own words. A reviewer can therefore check the no-duplication claim
//! mechanically: every other variant below names a law R4D-1 does not own.
//!
//! # The laws this lane adds
//!
//! 1. **Subset completeness.** A matrix may not be claimed while measuring less
//!    than its pinned minimum operation subset. A driver that quietly narrows
//!    its subset is claiming parity it did not measure, and the subset is
//!    pinned here and in the corpus rather than in prose.
//! 2. **Restart identity.** A restart requires a *new* `ServiceInstanceId` on
//!    the *same* slot. An engine that came back with its instance identity
//!    intact was never restarted, and one that came back on a different slot is
//!    a different service.
//! 3. **Resume re-admission.** Sleep/resume must re-admit from a fresh probe.
//!    Reusing the pre-sleep receipt assumes across the boundary exactly what
//!    the boundary exists to re-establish.
//! 4. **Bounded long sessions.** "Long" is stated in counts and bytes, never as
//!    a word.
//! 5. **Reversibility.** A plan must leave the selector on the retained engine.
//!    R4D-1 proved reverting is always *available*; this proves the driver is
//!    *required* to use it, so no run can strand the daily driver on an engine
//!    it was only borrowing.

use std::num::{NonZeroU32, NonZeroU64};

use grapt_types::service::OwnerReceipt;
use thiserror::Error;

use crate::bounds::{BoundProvenance, HostBoundName, RecordedHostBounds};
use crate::receipt::{AdmittedDifferential, AttachmentRefusal, SelectionReceipt, SelectorState};

/// A V2 operation a matrix may exercise.
///
/// Closed, and deliberately tiny. The frozen V2 surface carries 101 paths and
/// 120 operations; this lane owns attachment and the matrices, never the V2
/// server. Naming an operation here is a claim that a matrix's *minimum* subset
/// needs it, so the set stays at the two operations the matrices actually
/// require. Widening it is a reviewed change, not a convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Operation {
    /// `GET /api/health` — the only operation the frozen desktop's probe needs.
    HealthGet,
    /// `GET /api/session` — the smallest read-only listing with a comparable
    /// body.
    SessionList,
}

impl Operation {
    /// The operation's exact `operationId` in the frozen OpenAPI.
    ///
    /// Verified against the frozen surface rather than recalled: `/api/health`
    /// is `v2.health.get` with responses 200/400/401, and `/api/session` is
    /// `v2.session.list`.
    pub fn operation_id(self) -> &'static str {
        match self {
            Operation::HealthGet => "v2.health.get",
            Operation::SessionList => "v2.session.list",
        }
    }

    /// The inverse of [`operation_id`](Self::operation_id). Unknown text is
    /// refused rather than defaulted.
    pub fn from_operation_id(id: &str) -> Option<Self> {
        Operation::ALL
            .into_iter()
            .find(|candidate| candidate.operation_id() == id)
    }

    pub const ALL: [Operation; 2] = [Operation::HealthGet, Operation::SessionList];
}

/// The four matrices R4 owes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DifferentialMatrix {
    /// Two engines, same inputs, compared.
    Differential,
    /// One engine across a restart boundary.
    Restart,
    /// One engine across a host sleep/resume boundary.
    SleepResume,
    /// One engine under a bounded repeated read.
    LongSession,
}

impl DifferentialMatrix {
    pub fn label(self) -> &'static str {
        match self {
            DifferentialMatrix::Differential => "differential",
            DifferentialMatrix::Restart => "restart",
            DifferentialMatrix::SleepResume => "sleep-resume",
            DifferentialMatrix::LongSession => "long-session",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        DifferentialMatrix::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// The exact minimum operation subset this matrix may not go below.
    ///
    /// Each is the smallest set that makes the matrix's claim true:
    ///
    /// * `Differential` needs `v2.health.get` to admit the attachment at all,
    ///   plus one read-only listing so something with a comparable body is
    ///   actually compared. Health alone would compare two liveness answers and
    ///   call it parity.
    /// * `Restart` needs `v2.health.get` on both sides of the boundary; the
    ///   identity change is what the matrix is *for*, and it is carried by
    ///   [`RestartBoundary`] rather than by an operation.
    /// * `SleepResume` needs the health probe at resume — that is the
    ///   re-admission — plus the same listing, because a resume that only
    ///   re-probes proves the engine is alive, not that it still answers.
    /// * `LongSession` needs the repeated read itself. Its bound, not its
    ///   operation count, is the load-bearing part.
    pub fn minimum_subset(self) -> &'static [Operation] {
        match self {
            DifferentialMatrix::Differential => &[Operation::HealthGet, Operation::SessionList],
            DifferentialMatrix::Restart => &[Operation::HealthGet],
            DifferentialMatrix::SleepResume => &[Operation::HealthGet, Operation::SessionList],
            DifferentialMatrix::LongSession => &[Operation::SessionList],
        }
    }

    pub const ALL: [DifferentialMatrix; 4] = [
        DifferentialMatrix::Differential,
        DifferentialMatrix::Restart,
        DifferentialMatrix::SleepResume,
        DifferentialMatrix::LongSession,
    ];
}

/// The operations a matrix declares it will exercise.
///
/// A subset may be *wider* than the minimum — measuring more is never a false
/// claim — but it may never be narrower, and every operation in it is named
/// explicitly, so widening is a visible act rather than a silent one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSubset {
    matrix: DifferentialMatrix,
    operations: Vec<Operation>,
}

impl OperationSubset {
    /// Declare a subset, or refuse it.
    ///
    /// Refuses a duplicate before an omission: a list holding one operation
    /// twice may well contain the right *number* of entries while omitting a
    /// required one, and reporting only the omission would hide the reason the
    /// list was wrong.
    pub fn declare(
        matrix: DifferentialMatrix,
        operations: Vec<Operation>,
    ) -> Result<Self, DifferentialRefusal> {
        for (index, operation) in operations.iter().enumerate() {
            if operations[..index].contains(operation) {
                return Err(DifferentialRefusal::OperationDeclaredTwice {
                    matrix,
                    operation: *operation,
                });
            }
        }

        let missing: Vec<Operation> = matrix
            .minimum_subset()
            .iter()
            .copied()
            .filter(|required| !operations.contains(required))
            .collect();
        if !missing.is_empty() {
            return Err(DifferentialRefusal::MatrixSubsetIncomplete { matrix, missing });
        }

        Ok(Self { matrix, operations })
    }

    pub fn matrix(&self) -> DifferentialMatrix {
        self.matrix
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }
}

/// One engine observed on both sides of a restart.
///
/// Owner identity is `{ServiceSlotId, ServiceInstanceId, EnvironmentReceipt}`.
/// A restart keeps the slot and mints a new instance; that is the whole law,
/// and it is why PID is not consulted — a restarted process that inherited its
/// predecessor's PID is still a new instance, and one that reused the instance
/// identity did not restart no matter what the PID says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartBoundary {
    before: OwnerReceipt,
    after: OwnerReceipt,
}

impl RestartBoundary {
    /// Observe a restart, or refuse the claim that one happened.
    ///
    /// Three checks, and the third is why `after` is *carried* rather than
    /// merely adjacent. Owner identity is `{slot, instance, environment}`. A
    /// restart keeps the slot and the environment and mints a new instance, so
    /// checking only the slot would leave `after` free to name a service with a
    /// different environment receipt — a different owner on the same slot, which
    /// is a replacement rather than a restart and has its own landed vocabulary
    /// (`ReplacementOutcome`, which this crate never produces). With the
    /// environment pinned here, binding `before` to an admitted engine in
    /// [`DifferentialPlan::admit`] transitively binds `after` to it too, on two
    /// of the three identity coordinates, with the third required to differ.
    pub fn observe(before: OwnerReceipt, after: OwnerReceipt) -> Result<Self, DifferentialRefusal> {
        if before.slot() != after.slot() {
            return Err(DifferentialRefusal::RestartChangedTheSlot);
        }
        if before.environment() != after.environment() {
            return Err(DifferentialRefusal::RestartChangedTheEnvironment);
        }
        if before.instance() == after.instance() {
            return Err(DifferentialRefusal::RestartKeptTheInstanceIdentity);
        }
        Ok(Self { before, after })
    }

    pub fn before(&self) -> &OwnerReceipt {
        &self.before
    }

    pub fn after(&self) -> &OwnerReceipt {
        &self.after
    }
}

/// A sleep/resume boundary crossed by re-admitting rather than by assuming.
///
/// The refusal here is value equality against the pre-sleep receipt. A genuine
/// re-admission runs a fresh probe through [`SelectionReceipt::admit`] and
/// takes a fresh instant from the injected clock, so it can never be equal to
/// the receipt it replaced; reusing the recorded one is therefore exactly the
/// case this refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeReadmission {
    before_sleep: SelectionReceipt,
    after_resume: SelectionReceipt,
}

impl ResumeReadmission {
    pub fn readmit(
        before_sleep: SelectionReceipt,
        after_resume: SelectionReceipt,
    ) -> Result<Self, DifferentialRefusal> {
        if before_sleep == after_resume {
            return Err(DifferentialRefusal::ResumeReusedTheRecordedReceipt);
        }
        Ok(Self {
            before_sleep,
            after_resume,
        })
    }

    pub fn before_sleep(&self) -> &SelectionReceipt {
        &self.before_sleep
    }

    pub fn after_resume(&self) -> &SelectionReceipt {
        &self.after_resume
    }
}

/// The bound that makes a long session long in counts and bytes.
///
/// These are the *driver's* bounds on its own reads, not the host's — the
/// host's eight live in [`crate::RecordedHostBounds`] and are recorded, never
/// chosen (the module itself is private; the re-export is the public name). This
/// one is genuinely this lane's to declare, and the ratified law is that it be
/// stated in bytes and counts rather than as a duration or an adjective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongSessionBound {
    max_reads: NonZeroU32,
    max_bytes: NonZeroU64,
}

impl LongSessionBound {
    pub fn declare(max_reads: u32, max_bytes: u64) -> Result<Self, DifferentialRefusal> {
        let max_reads = match NonZeroU32::new(max_reads) {
            Some(value) => value,
            None => return Err(DifferentialRefusal::LongSessionReadsUnbounded),
        };
        let max_bytes = match NonZeroU64::new(max_bytes) {
            Some(value) => value,
            None => return Err(DifferentialRefusal::LongSessionBytesUnbounded),
        };
        Ok(Self {
            max_reads,
            max_bytes,
        })
    }

    pub fn max_reads(self) -> NonZeroU32 {
        self.max_reads
    }

    pub fn max_bytes(self) -> NonZeroU64 {
        self.max_bytes
    }
}

/// The evidence a matrix carries beyond its admitted engine pair.
///
/// Modelled as a closed enum keyed to the matrix rather than as four optional
/// fields, so "a restart plan without a restart boundary" is a shape the type
/// system can refuse instead of a `None` somebody has to remember to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatrixEvidence {
    /// The differential matrix needs nothing past the admitted pair.
    None,
    /// The restart matrix carries its boundary.
    Restart(RestartBoundary),
    /// The sleep/resume matrix carries its re-admission.
    Resume(ResumeReadmission),
    /// The long-session matrix carries its bound.
    LongSession(LongSessionBound),
}

impl MatrixEvidence {
    /// The matrix this evidence belongs to.
    pub fn matrix(&self) -> DifferentialMatrix {
        match self {
            MatrixEvidence::None => DifferentialMatrix::Differential,
            MatrixEvidence::Restart(_) => DifferentialMatrix::Restart,
            MatrixEvidence::Resume(_) => DifferentialMatrix::SleepResume,
            MatrixEvidence::LongSession(_) => DifferentialMatrix::LongSession,
        }
    }
}

/// A differential run the contract admits.
///
/// Holding one is the claim that a driver *may* run this matrix — never that it
/// did, and never that any engine was measured. Every value inside is either a
/// landed R4D-1 fact or a declaration this contract has checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifferentialPlan {
    matrix: DifferentialMatrix,
    subset: OperationSubset,
    engines: AdmittedDifferential,
    rust_bounds: RecordedHostBounds,
    evidence: MatrixEvidence,
    terminal_selector: SelectorState,
}

impl DifferentialPlan {
    /// Admit a plan, or refuse it.
    ///
    /// The component laws have already refused at their own constructors —
    /// an unadmitted engine pair, an incomplete subset, an asserted bound and
    /// an impossible restart cannot reach this function. What is left is
    /// exactly the relations *between* the components, which no component can
    /// check alone:
    ///
    /// 1. the subset was declared for this matrix and not another;
    /// 2. the evidence belongs to this matrix;
    /// 3. **the evidence describes an engine this plan actually admitted**;
    /// 4. the plan returns the selector to the retained engine.
    ///
    /// Check 3 is the one a component provably cannot make. A
    /// [`RestartBoundary`] is a well-formed restart of *something*, and a
    /// [`ResumeReadmission`] is a genuine re-admission of *something*; neither
    /// constructor is given the engine pair, so neither can tell whether the
    /// something is one of the two engines under comparison. Without it a plan
    /// is admissible whose evidence describes a third, unrelated service — the
    /// matrix would then report a restart or a resume that never happened to
    /// either engine it claims to compare. The relation is carried out by the
    /// private `evidence_names_an_admitted_engine`, whose own comment states
    /// what each matrix requires.
    ///
    /// The reversibility check runs last so that a plan wrong in two ways names
    /// the structural defect before the housekeeping one.
    pub fn admit(
        matrix: DifferentialMatrix,
        subset: OperationSubset,
        engines: AdmittedDifferential,
        rust_bounds: RecordedHostBounds,
        evidence: MatrixEvidence,
        terminal_selector: SelectorState,
    ) -> Result<Self, DifferentialRefusal> {
        if subset.matrix() != matrix {
            return Err(DifferentialRefusal::SubsetDeclaredForAnotherMatrix {
                declared: subset.matrix(),
                planned: matrix,
            });
        }
        if evidence.matrix() != matrix {
            return Err(DifferentialRefusal::MatrixEvidenceMismatch {
                planned: matrix,
                evidence: evidence.matrix(),
            });
        }
        Self::evidence_names_an_admitted_engine(&engines, &evidence)?;
        if terminal_selector != SelectorState::Retained {
            return Err(DifferentialRefusal::LeavesSelectorAttached);
        }
        Ok(Self {
            matrix,
            subset,
            engines,
            rust_bounds,
            evidence,
            terminal_selector,
        })
    }

    /// Whether the matrix evidence describes an engine this plan admitted.
    ///
    /// Uses the landed [`OwnerReceipt::is_same_owner`] — the same comparison
    /// `still_current` and [`AdmittedDifferential::admit`] already use — so no
    /// identity is invented here and no coordinate is compared that R4D-1 does
    /// not already treat as identity. PID and endpoint are deliberately not
    /// consulted, for the reason they never are: a reused PID is not ownership.
    ///
    /// * `None` and `LongSession` carry no owner, so there is nothing to relate
    ///   and nothing is refused. That is not a gap: a long-session bound is a
    ///   statement about the driver's own reads, not about an engine.
    /// * `Restart` requires `before` to be one of the two admitted owners.
    ///   [`RestartBoundary::observe`] has already pinned `after` to the same
    ///   slot and environment with a new instance, so `after` is carried by
    ///   `before` and needs no separate membership test — by construction it is
    ///   that owner's successor, and it cannot be that owner.
    /// * `Resume` requires **both** receipts to name the *same* admitted owner,
    ///   **on both coordinates of the admitted pairing** — the same owner *and*
    ///   that owner's admitted engine selection.
    ///
    /// # Why the resume rule is stated against the admitted RECEIPT
    ///
    /// An earlier form of this law asked for two weaker things: that both halves
    /// name one admitted *owner*, and that the halves agree with *each other* on
    /// the selection. The R2 closure review showed both can hold while the plan
    /// is still false. With `Rust ← rust_owner` and `Retained ← retained_owner`
    /// admitted, resume evidence in which **both** halves name `rust_owner` and
    /// **both** select `Retained` satisfies each half — one admitted owner, and
    /// they do agree — yet it attributes the Rust engine's owner receipt to the
    /// Retained engine. The durable row would then carry
    /// `selection = "retained"` beside the Rust engine's identity coordinates.
    ///
    /// That case is the exact dual of the one the old rule *did* catch. Agreement
    /// between the two halves is a statement about the halves; it says nothing
    /// about the pairing they are supposed to describe. So the rule now looks for
    /// a single admitted [`SelectionReceipt`] that both halves match on owner
    /// **and** selection, which is the pairing itself.
    ///
    /// The two failures stay separately named because they are different facts:
    /// evidence about a service that is not under comparison (or split across the
    /// two that are) is [`DifferentialRefusal::MatrixEvidenceNamesAnotherEngine`],
    /// while evidence about the right owner attributed to the wrong engine is
    /// [`DifferentialRefusal::ResumeContradictsTheAdmittedPairing`].
    fn evidence_names_an_admitted_engine(
        engines: &AdmittedDifferential,
        evidence: &MatrixEvidence,
    ) -> Result<(), DifferentialRefusal> {
        let admitted = [engines.left(), engines.right()];
        match evidence {
            MatrixEvidence::None | MatrixEvidence::LongSession(_) => Ok(()),
            MatrixEvidence::Restart(boundary) => {
                if admitted
                    .iter()
                    .any(|receipt| receipt.owner().is_same_owner(boundary.before()))
                {
                    Ok(())
                } else {
                    Err(DifferentialRefusal::MatrixEvidenceNamesAnotherEngine)
                }
            }
            MatrixEvidence::Resume(readmission) => {
                let before = readmission.before_sleep();
                let after = readmission.after_resume();

                // First: is there ONE admitted receipt whose owner owns both
                // halves? Failing this means the evidence is about a service
                // that is not under comparison, or is split across the two that
                // are — neither describes a single engine's sleep boundary.
                let paired = admitted.iter().find(|receipt| {
                    receipt.owner().is_same_owner(before.owner())
                        && receipt.owner().is_same_owner(after.owner())
                });
                let Some(paired) = paired else {
                    return Err(DifferentialRefusal::MatrixEvidenceNamesAnotherEngine);
                };

                // Then: both halves must name the engine that owner was admitted
                // as. Checked against `paired`, never against each other — two
                // halves agreeing on the wrong engine agree about nothing that
                // matters.
                if before.selection() != paired.selection()
                    || after.selection() != paired.selection()
                {
                    return Err(DifferentialRefusal::ResumeContradictsTheAdmittedPairing);
                }
                Ok(())
            }
        }
    }

    pub fn matrix(&self) -> DifferentialMatrix {
        self.matrix
    }

    pub fn subset(&self) -> &OperationSubset {
        &self.subset
    }

    pub fn engines(&self) -> &AdmittedDifferential {
        &self.engines
    }

    pub fn rust_bounds(&self) -> &RecordedHostBounds {
        &self.rust_bounds
    }

    pub fn evidence(&self) -> &MatrixEvidence {
        &self.evidence
    }

    pub fn terminal_selector(&self) -> &SelectorState {
        &self.terminal_selector
    }
}

/// Why the differential contract refused.
///
/// [`Attachment`](Self::Attachment) carries R4D-1's landed refusal unchanged —
/// this lane owns no second spelling of the attachment laws. Every other
/// variant is a law R4D-1 does not own.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DifferentialRefusal {
    /// An attachment-level law refused, in R4D-1's own words.
    #[error(transparent)]
    Attachment(#[from] AttachmentRefusal),

    /// The declared subset is narrower than the matrix's pinned minimum.
    #[error(
        "the {} matrix declares a subset missing {missing:?}: a matrix measured below its minimum subset claims parity it did not measure",
        matrix.label()
    )]
    MatrixSubsetIncomplete {
        matrix: DifferentialMatrix,
        missing: Vec<Operation>,
    },

    /// One operation appears twice in a declared subset.
    #[error(
        "the {} matrix declares {} twice: one operation cannot stand in for another",
        matrix.label(),
        operation.operation_id()
    )]
    OperationDeclaredTwice {
        matrix: DifferentialMatrix,
        operation: Operation,
    },

    /// A subset declared for one matrix was offered to a plan for another.
    #[error(
        "a subset declared for the {} matrix cannot be planned as {}: a matrix must be measured by its own subset",
        declared.label(),
        planned.label()
    )]
    SubsetDeclaredForAnotherMatrix {
        declared: DifferentialMatrix,
        planned: DifferentialMatrix,
    },

    /// The evidence offered belongs to a different matrix.
    #[error(
        "{} evidence cannot support a {} plan: each matrix carries its own evidence",
        evidence.label(),
        planned.label()
    )]
    MatrixEvidenceMismatch {
        planned: DifferentialMatrix,
        evidence: DifferentialMatrix,
    },

    /// A bound was not read from the host's own receipt.
    #[error(
        "the bound {} was {} rather than reported by the host receipt: a differential records the engine's bounds, it does not state them",
        name.field_name(),
        provenance.label()
    )]
    BoundNotReportedByHost {
        name: HostBoundName,
        provenance: BoundProvenance,
    },

    /// The host bound recording is missing at least one field.
    #[error(
        "the host bound recording is missing {missing:?}: an unrecorded bound is a gap, not a default"
    )]
    BoundSetIncomplete { missing: Vec<HostBoundName> },

    /// One bound was recorded twice.
    #[error(
        "the bound {} was recorded twice: one reading cannot stand in for another",
        name.field_name()
    )]
    BoundRecordedTwice { name: HostBoundName },

    /// A bound carries an empty rendering.
    #[error(
        "the bound {} was recorded with an empty rendering: an empty value records nothing while looking like a reading",
        name.field_name()
    )]
    BoundRenderingEmpty { name: HostBoundName },

    /// The engine came back with its instance identity intact.
    #[error(
        "the observed instance identity is unchanged across the boundary: an engine that kept its ServiceInstanceId did not restart"
    )]
    RestartKeptTheInstanceIdentity,

    /// The engine came back on a different slot.
    #[error(
        "the observed slot changed across the boundary: a different registration slot is a different service, not a restart"
    )]
    RestartChangedTheSlot,

    /// The engine came back with a different environment receipt.
    #[error(
        "the observed environment receipt changed across the boundary: an owner differing in environment is a replacement on the same slot, not a restart"
    )]
    RestartChangedTheEnvironment,

    /// The matrix evidence describes a service the plan did not admit.
    #[error(
        "the matrix evidence does not describe one engine this plan admitted: a restart or resume of some other service is not evidence about either engine under comparison"
    )]
    MatrixEvidenceNamesAnotherEngine,

    /// Resume evidence names an admitted owner as the wrong engine.
    #[error(
        "the resume evidence attributes an admitted owner receipt to an engine it was not admitted as: two halves agreeing on the wrong engine agree about nothing that matters"
    )]
    ResumeContradictsTheAdmittedPairing,

    /// Resume reused the pre-sleep receipt instead of re-admitting.
    #[error(
        "resume reused the recorded receipt: crossing a sleep boundary must re-admit from a fresh probe, not assume the recorded attachment survived"
    )]
    ResumeReusedTheRecordedReceipt,

    /// A long session declared an unbounded read count.
    #[error(
        "the long-session matrix declared zero maximum reads: `long` is stated in counts and bytes, and an unbounded count is not a bound"
    )]
    LongSessionReadsUnbounded,

    /// A long session declared an unbounded byte budget.
    #[error(
        "the long-session matrix declared zero maximum bytes: `long` is stated in counts and bytes, and an unbounded budget is not a bound"
    )]
    LongSessionBytesUnbounded,

    /// The plan does not return the selector to the retained engine.
    #[error(
        "the plan leaves the selector attached: a differential run must return the desktop to its retained engine, never strand it on one it was borrowing"
    )]
    LeavesSelectorAttached,
}

impl DifferentialRefusal {
    /// The stable label used in the corpus.
    ///
    /// The attachment case delegates to R4D-1's own label rather than minting a
    /// new word for a landed law. That delegation is the mechanical proof of
    /// the no-duplication claim: R4D-1's four refusal words reach this lane's
    /// projection unchanged, and no variant below re-spells one of them.
    pub fn label(&self) -> &'static str {
        match self {
            DifferentialRefusal::Attachment(inner) => inner.label(),
            DifferentialRefusal::MatrixSubsetIncomplete { .. } => "matrix-subset-incomplete",
            DifferentialRefusal::OperationDeclaredTwice { .. } => "operation-declared-twice",
            DifferentialRefusal::SubsetDeclaredForAnotherMatrix { .. } => {
                "subset-declared-for-another-matrix"
            }
            DifferentialRefusal::MatrixEvidenceMismatch { .. } => "matrix-evidence-mismatch",
            DifferentialRefusal::BoundNotReportedByHost { .. } => "bound-not-reported-by-host",
            DifferentialRefusal::BoundSetIncomplete { .. } => "bound-set-incomplete",
            DifferentialRefusal::BoundRecordedTwice { .. } => "bound-recorded-twice",
            DifferentialRefusal::BoundRenderingEmpty { .. } => "bound-rendering-empty",
            DifferentialRefusal::RestartKeptTheInstanceIdentity => {
                "restart-kept-the-instance-identity"
            }
            DifferentialRefusal::RestartChangedTheSlot => "restart-changed-the-slot",
            DifferentialRefusal::RestartChangedTheEnvironment => "restart-changed-the-environment",
            DifferentialRefusal::MatrixEvidenceNamesAnotherEngine => {
                "matrix-evidence-names-another-engine"
            }
            DifferentialRefusal::ResumeContradictsTheAdmittedPairing => {
                "resume-contradicts-the-admitted-pairing"
            }
            DifferentialRefusal::ResumeReusedTheRecordedReceipt => {
                "resume-reused-the-recorded-receipt"
            }
            DifferentialRefusal::LongSessionReadsUnbounded => "long-session-reads-unbounded",
            DifferentialRefusal::LongSessionBytesUnbounded => "long-session-bytes-unbounded",
            DifferentialRefusal::LeavesSelectorAttached => "leaves-selector-attached",
        }
    }

    /// How many distinct labels this contract can produce.
    pub const LABEL_COUNT: usize = 21;

    /// Every label this contract can produce, in [`label_index`](Self::label_index) order.
    ///
    /// # Why this exists, and why it is not just another hand-written list
    ///
    /// The R1 review falsified the previous completeness guard: the corpus test
    /// and the no-duplication test each compared against their *own* 18-entry
    /// literal, so a nineteenth variant with a label and no corpus row left the
    /// whole suite green. A literal that nothing derives from the enum cannot
    /// notice the enum growing.
    ///
    /// The binding is [`label_index`](Self::label_index), whose `match` is
    /// **exhaustive over both** this enum and the [`AttachmentRefusal`] it
    /// carries. A new variant in either one makes that match non-exhaustive and
    /// the **crate stops compiling** — so a law cannot be added without being
    /// given a label here, and `every_label_in_the_carrier_is_produced_by_a_real_variant`
    /// then forces the array to grow with it. That is a compile-time
    /// obligation, not a test that can be left un-run.
    ///
    /// `label()` deliberately keeps its own delegating `match` rather than being
    /// rewritten as `ALL_LABELS[self.label_index()]`: the delegation to R4D-1's
    /// `inner.label()` *is* the no-duplication proof and QS18 mutates exactly
    /// that arm. The two are cross-checked against each other by
    /// `the_carrier_and_the_label_agree_for_every_variant`, so they cannot drift
    /// apart silently.
    pub const ALL_LABELS: [&'static str; Self::LABEL_COUNT] = [
        // R4D-1's four, in its own spelling.
        "unattributed",
        "not-the-recorded-owner",
        "same-owner-on-both-sides",
        "same-selection-on-both-sides",
        // This lane's sixteen.
        "matrix-subset-incomplete",
        "operation-declared-twice",
        "subset-declared-for-another-matrix",
        "matrix-evidence-mismatch",
        "matrix-evidence-names-another-engine",
        "resume-contradicts-the-admitted-pairing",
        "bound-not-reported-by-host",
        "bound-set-incomplete",
        "bound-recorded-twice",
        "bound-rendering-empty",
        "restart-kept-the-instance-identity",
        "restart-changed-the-slot",
        "restart-changed-the-environment",
        "resume-reused-the-recorded-receipt",
        "long-session-reads-unbounded",
        "long-session-bytes-unbounded",
        "leaves-selector-attached",
    ];

    /// This refusal's position in [`ALL_LABELS`](Self::ALL_LABELS).
    ///
    /// The exhaustive match here is the compile-time obligation described on
    /// `ALL_LABELS`. Adding a variant to either this enum or [`AttachmentRefusal`]
    /// breaks it, which is the whole point: a law with no label and no corpus
    /// path must not be able to compile, let alone stay green.
    pub fn label_index(&self) -> usize {
        match self {
            DifferentialRefusal::Attachment(inner) => match inner {
                AttachmentRefusal::Unattributed => 0,
                AttachmentRefusal::NotTheRecordedOwner => 1,
                AttachmentRefusal::SameOwnerOnBothSides => 2,
                AttachmentRefusal::SameSelectionOnBothSides => 3,
            },
            DifferentialRefusal::MatrixSubsetIncomplete { .. } => 4,
            DifferentialRefusal::OperationDeclaredTwice { .. } => 5,
            DifferentialRefusal::SubsetDeclaredForAnotherMatrix { .. } => 6,
            DifferentialRefusal::MatrixEvidenceMismatch { .. } => 7,
            DifferentialRefusal::MatrixEvidenceNamesAnotherEngine => 8,
            DifferentialRefusal::ResumeContradictsTheAdmittedPairing => 9,
            DifferentialRefusal::BoundNotReportedByHost { .. } => 10,
            DifferentialRefusal::BoundSetIncomplete { .. } => 11,
            DifferentialRefusal::BoundRecordedTwice { .. } => 12,
            DifferentialRefusal::BoundRenderingEmpty { .. } => 13,
            DifferentialRefusal::RestartKeptTheInstanceIdentity => 14,
            DifferentialRefusal::RestartChangedTheSlot => 15,
            DifferentialRefusal::RestartChangedTheEnvironment => 16,
            DifferentialRefusal::ResumeReusedTheRecordedReceipt => 17,
            DifferentialRefusal::LongSessionReadsUnbounded => 18,
            DifferentialRefusal::LongSessionBytesUnbounded => 19,
            DifferentialRefusal::LeavesSelectorAttached => 20,
        }
    }
}
