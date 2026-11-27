use super::{
    ArtifactClass, BoundaryKind, DifferentialMatrix, DifferentialRefusal, EntrypointCapability,
    Error, EvidenceTier, NegativeControl, Operation, OwnerCoordinate, RetainedSource, RunPhase,
};

/// Why the operator contract refused.
///
/// [`Differential`](Self::Differential) carries R4D-2A's landed refusal
/// unchanged — which in turn carries R4D-1's — so all three lanes speak one
/// vocabulary per fact and this lane mints no second spelling of a landed law.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OperatorRefusal {
    /// A landed differential or attachment law refused, in its own words.
    #[error(transparent)]
    Differential(#[from] DifferentialRefusal),

    // ---- tier and authorization ----
    /// The declared tier has no caps of its own to bound a live run.
    #[error(
        "the {} tier carries no caps of its own: a live run must be bounded by the tier it is executed under",
        tier.label()
    )]
    TierCarriesNoCapsOfItsOwn { tier: EvidenceTier },

    /// A phase past the announcement was attempted without a scheduled window.
    #[error(
        "the {} phase requires a scheduled attended window and none was granted (parked: {}): the tier is declared and its execution is withheld",
        phase.label(),
        park_reason.map(|reason| reason.label()).unwrap_or("unstated")
    )]
    ExecutionNotAuthorized {
        phase: RunPhase,
        park_reason: Option<crate::tier::ParkReason>,
    },

    // ---- entrypoint and matrix scope ----
    /// The matrix cannot be served by the declared entrypoint.
    #[error(
        "the {} matrix is unservable at the {} entrypoint, which cannot serve {missing:?}: the landed minimum subset decides this, and narrowing it would delete the law that makes a differential mean anything",
        matrix.label(),
        entrypoint.label()
    )]
    MatrixUnservableAtEntrypoint {
        matrix: DifferentialMatrix,
        entrypoint: EntrypointCapability,
        missing: Vec<Operation>,
    },

    /// Sleep/resume is parked pending a host-level decision.
    #[error(
        "the sleep-resume matrix is parked: a real host sleep suspends every seat on this machine, so it needs a scheduled fleet quiesce or a second host, and that decision is not the operator's"
    )]
    SleepResumeParked,

    /// A substitute was offered in place of a host sleep.
    #[error(
        "the {} boundary was offered in place of a host sleep: a process suspend or a network drop is a different matrix wearing the sleep matrix's label, and the receipt would be false in the one place the matrix exists to be true",
        offered.label()
    )]
    SubstituteBoundaryRefused { offered: BoundaryKind },

    /// The declared boundary is not the one the matrix crosses.
    #[error(
        "the {} matrix crosses the {} boundary, not {}: a matrix measured across the wrong boundary measured something else",
        matrix.label(),
        required.label(),
        offered.label()
    )]
    BoundaryDoesNotMatchMatrix {
        matrix: DifferentialMatrix,
        offered: BoundaryKind,
        required: BoundaryKind,
    },

    // ---- pre-entrypoint construction ----
    /// No host has reported its bounds, so none can be recorded.
    #[error(
        "no host has reported its resource bounds: a recording admits only host-reported provenance and there is no ready host to read one from, so no differential plan is constructible yet -- by the contract's design, not by omission"
    )]
    HostBoundsUnavailableBeforeEntrypoint,

    // ---- phase gating ----
    /// A phase was entered twice.
    #[error(
        "the {} phase was entered twice: a run cannot record two of the same phase and a projection counting them could not tell which was real",
        phase.label()
    )]
    PhaseAlreadyEntered { phase: RunPhase },

    /// A phase was entered out of order.
    #[error(
        "the {} phase was entered where {} was expected: reversal precedes diagnosis, and an out-of-order run can leave the daily driver on a borrowed engine",
        attempted.label(),
        expected.label()
    )]
    PhaseOutOfOrder {
        attempted: RunPhase,
        expected: RunPhase,
    },

    /// A phase was attempted after the last one.
    #[error(
        "the {} phase was attempted after the run's final phase: there is nothing after the closing host record",
        phase.label()
    )]
    PhaseAfterTheLast { phase: RunPhase },

    /// A run was concluded without ever being announced.
    #[error(
        "the run was concluded without an announcement: an attended window is a custody claim and an unannounced run cannot make it"
    )]
    RunNeverAnnounced,

    /// A run stopped between starting and cleaning up.
    #[error(
        "the run stopped at the {} phase without reaching its cleanup: a run that started engines and stopped early left processes and files on a host shared with live seats, and no receipt it produced reads as complete",
        reached.label()
    )]
    CleanupPhasesNotReached { reached: RunPhase },

    // ---- negative controls ----
    /// The plan omits at least one deliberate refusal.
    #[error(
        "the plan omits the negative control(s) {missing:?}: a run that only produces greens has not tested its instrument"
    )]
    NegativeControlOmitted { missing: Vec<NegativeControl> },

    // ---- custody ----
    /// An artifact was declared with no name.
    #[error(
        "an artifact was declared with no name: an unnamed artifact cannot be pulled before cleanup removes it"
    )]
    ArtifactUnnamed,

    /// An artifact of an inadmissible class was declared.
    #[error(
        "an artifact of class {} may not be retained: session bodies are workspace state and registrations carry a password",
        class.label()
    )]
    ArtifactClassNotRetainable { class: ArtifactClass },

    /// An artifact was declared without a byte budget.
    #[error(
        "the artifact {name} was declared with a zero byte budget: an unbounded artifact is not a bounded one"
    )]
    ArtifactBudgetUnbounded { name: String },

    /// Two artifacts share a name.
    #[error(
        "the artifact {name} was declared twice: a cleanup that pulls one of two same-named artifacts destroys the other"
    )]
    ArtifactDeclaredTwice { name: String },

    /// A receipt root or repository root was unnamed.
    #[error(
        "a receipt root was declared without naming both the repository it must stay out of and its own path: the law is a relation between the two"
    )]
    ReceiptRootUnnamed,

    /// The receipt root lies inside the repository.
    #[error(
        "the receipt root {receipt_root} lies inside the repository: writing any file into the tree while a battery runs voids the run, and the receipt-root default is inside the tree"
    )]
    ReceiptRootInsideRepository { receipt_root: String },

    /// Custody retains nothing at all.
    #[error(
        "the plan retains no artifact: a run that keeps nothing has no evidence, and that state must not be reachable by omission"
    )]
    CustodyRetainsNothing,

    /// The declared budgets exceed the evidence ceiling.
    #[error(
        "the declared artifact budget of {declared} byte(s) exceeds the {ceiling}-byte evidence ceiling: evidence is a digest and a sample, never a raw stream"
    )]
    CustodyBudgetExceeded { declared: u64, ceiling: u64 },

    /// The declared budgets do not sum to a representable byte total.
    ///
    /// Distinct from [`CustodyBudgetExceeded`](Self::CustodyBudgetExceeded)
    /// because the declared total is not a number at all: it is larger than any
    /// `u64`, so no honest `declared` field can be filled in. Reporting it as an
    /// over-ceiling declaration of `u64::MAX` would name a figure nobody
    /// declared, and it would make a saturating sum indistinguishable from the
    /// checked one — the substitution this refusal exists to keep falsifiable.
    #[error(
        "the declared artifact budgets do not sum to a representable byte total: the {preceding} byte(s) already declared plus {name}'s own budget overflow a u64, so the declaration is refused before any comparison with the ceiling -- a wrapping sum would report a small total here and a saturating one would report u64::MAX, and neither is what was declared"
    )]
    CustodyBudgetNotRepresentable { name: String, preceding: u64 },

    // ---- cleanup ----
    /// A kill by image name was declared.
    #[error(
        "a kill by image name ({image}) was declared: the node and bash processes on this host are other seats, and an image-name kill takes the fleet out with the run"
    )]
    ImageNameKillRefused { image: String },

    /// A kill target that is not a process was declared.
    #[error(
        "the kill target is not a process this run started: a zero pid makes the cleanup unattributable"
    )]
    KillTargetNotAProcess,

    /// A deletion of a path the run did not record creating.
    #[error(
        "the path {path} was not recorded as created by this run: the state home is shared with live seats and a cleanup may remove only what it recorded creating"
    )]
    DeletionOfUnrecordedPath { path: String },

    // ---- timeouts ----
    /// The whole-run timeout is zero.
    #[error(
        "the whole-run timeout is zero: an unbounded window is a fleet outage nobody agreed to"
    )]
    RunTimeoutUnbounded,

    /// The per-matrix timeout is zero.
    #[error(
        "the per-matrix timeout is zero: a matrix without a bound cannot report which cap ended it"
    )]
    MatrixTimeoutUnbounded,

    /// A declared timeout exceeds the tier's cap.
    #[error(
        "a declared timeout of {declared_ms} ms exceeds the tier cap of {cap_ms} ms: a plan may be tighter than its tier and never looser"
    )]
    TimeoutExceedsTierCap { declared_ms: u64, cap_ms: u64 },

    /// The per-matrix timeout leaves no room for reversal and cleanup.
    #[error(
        "the per-matrix timeout of {per_matrix_ms} ms leaves no room inside the {whole_run_ms} ms run: a cap satisfiable only by skipping cleanup is not a cap"
    )]
    MatrixTimeoutLeavesNoRoomForCleanup {
        per_matrix_ms: u64,
        whole_run_ms: u64,
    },

    // ---- engines ----
    /// Fewer engine processes were declared than a differential requires.
    ///
    /// One refusal for `0` and for `1`, because they are one defect: the
    /// 2026-08-14 ruling requires exactly two engine processes in total, both
    /// started by the operator inside the attended window, and a retained
    /// engine that was already running satisfies neither half of that. The
    /// earlier zero-only spelling stated the two-engine law in its message
    /// while admitting a one-engine plan, which is the defect this variant
    /// exists to make unrepresentable.
    #[error(
        "{declared} engine process(es) were declared where a differential requires {required}: fewer than {required} is not a comparison, and a retained engine already running is not one of the {required} the operator must start inside the window"
    )]
    TooFewEnginesForDifferential { declared: u32, required: u32 },

    /// More engines than the tier admits.
    #[error(
        "{declared} engine processes were declared against a cap of {cap}: a third listener makes attribution a guess"
    )]
    TooManyEnginesForAttribution { declared: u32, cap: u32 },

    // ---- retained attribution ----
    /// The retained mapping receipt is absent.
    #[error(
        "no retained owner mapping receipt was declared: the retained side's owner identity is a declaration and the run fails closed without one rather than attributing it by guesswork"
    )]
    RetainedMappingReceiptAbsent,

    /// A mapping receipt was recorded with no id.
    #[error("the mapping receipt carries no id: a receipt that names nothing cannot be cited")]
    MappingReceiptUnnamed,

    /// A mapping claimed to be read from the wire.
    #[error(
        "the mapping claims to be read from the wire: the frozen surface carries no engine identity at all, and the one correlation near it correlates on pid -- the exact coordinate D1 forbids as identity"
    )]
    MappingReadFromWire,

    /// A pid was offered as an identity coordinate.
    #[error(
        "a process identifier was offered as the {} coordinate: pid is an attribute under D1, and a reused pid must never inherit ownership",
        coordinate.label()
    )]
    PidOfferedAsIdentity { coordinate: OwnerCoordinate },

    /// An endpoint was offered as an identity coordinate.
    #[error(
        "an endpoint was offered as the {} coordinate: the endpoint is an attribute under D1, and one owner keeps its slot across a new ephemeral port",
        coordinate.label()
    )]
    EndpointOfferedAsIdentity { coordinate: OwnerCoordinate },

    /// A version string was offered as an identity coordinate.
    #[error(
        "a version was offered as the {} coordinate: two independent engines can report one version and one engine changes it across an upgrade without changing owner",
        coordinate.label()
    )]
    VersionOfferedAsIdentity { coordinate: OwnerCoordinate },

    /// An identity-bearing source was offered for the wrong coordinate.
    ///
    /// The field is `offered` rather than `source`: `thiserror` treats a field
    /// literally named `source` as the error's `Error::source`, which would
    /// require `RetainedSource` to be an error type it is not. Naming it for
    /// what it is avoids teaching a vocabulary enum to impersonate an error.
    #[error(
        "the {} coordinate was sourced from {}: every source in this declaration is identity-bearing, so only the pairing tells a crossed mapping from a sound one",
        coordinate.label(),
        offered.label()
    )]
    CoordinateSourceMismatch {
        coordinate: OwnerCoordinate,
        offered: RetainedSource,
    },

    /// The retained registration carries no instance identity.
    #[error(
        "the retained registration carries no id: the field is optional in its schema, and with it absent there is no instance identity -- the run stops rather than synthesizing one"
    )]
    RetainedInstanceIdentityAbsent,

    /// The retained slot is ambiguous without a pinned state home.
    #[error(
        "the retained slot is ambiguous: four registration files matching the frozen discovery regex were observed in one state home, and discovery takes the first that probes in readdir order, so the run must pin an isolated state home"
    )]
    RetainedSlotAmbiguous,

    /// A body comparison was declared with no store-equivalence proof.
    #[error(
        "a body-content comparison was declared with no proof the engines share a session store: two engines reading different data roots differ in every row, and the differential would record a data difference as an engine difference"
    )]
    BodyContentComparisonUnproven,

    /// An observation was recorded without a body digest.
    #[error(
        "the observation of {operation_id} carries no body digest: it records that a request happened and nothing about what came back, and two such observations compare equal for the wrong reason"
    )]
    ObservationDigestAbsent { operation_id: String },
}

impl OperatorRefusal {
    /// The stable label used in the corpus.
    ///
    /// The differential case delegates to R4D-2A's own label rather than
    /// minting a new word for a landed law, exactly as that lane delegates to
    /// R4D-1's. That delegation is this lane's no-duplication proof, and it is
    /// checked mechanically by
    /// `this_lanes_labels_are_disjoint_from_every_landed_one`.
    pub fn label(&self) -> &'static str {
        match self {
            OperatorRefusal::Differential(inner) => inner.label(),
            OperatorRefusal::TierCarriesNoCapsOfItsOwn { .. } => "tier-carries-no-caps-of-its-own",
            OperatorRefusal::ExecutionNotAuthorized { .. } => "execution-not-authorized",
            OperatorRefusal::MatrixUnservableAtEntrypoint { .. } => {
                "matrix-unservable-at-entrypoint"
            }
            OperatorRefusal::SleepResumeParked => "sleep-resume-parked",
            OperatorRefusal::SubstituteBoundaryRefused { .. } => "substitute-boundary-refused",
            OperatorRefusal::BoundaryDoesNotMatchMatrix { .. } => "boundary-does-not-match-matrix",
            OperatorRefusal::HostBoundsUnavailableBeforeEntrypoint => {
                "host-bounds-unavailable-before-entrypoint"
            }
            OperatorRefusal::PhaseAlreadyEntered { .. } => "phase-already-entered",
            OperatorRefusal::PhaseOutOfOrder { .. } => "phase-out-of-order",
            OperatorRefusal::PhaseAfterTheLast { .. } => "phase-after-the-last",
            OperatorRefusal::RunNeverAnnounced => "run-never-announced",
            OperatorRefusal::CleanupPhasesNotReached { .. } => "cleanup-phases-not-reached",
            OperatorRefusal::NegativeControlOmitted { .. } => "negative-control-omitted",
            OperatorRefusal::ArtifactUnnamed => "artifact-unnamed",
            OperatorRefusal::ArtifactClassNotRetainable { .. } => "artifact-class-not-retainable",
            OperatorRefusal::ArtifactBudgetUnbounded { .. } => "artifact-budget-unbounded",
            OperatorRefusal::ArtifactDeclaredTwice { .. } => "artifact-declared-twice",
            OperatorRefusal::ReceiptRootUnnamed => "receipt-root-unnamed",
            OperatorRefusal::ReceiptRootInsideRepository { .. } => "receipt-root-inside-repository",
            OperatorRefusal::CustodyRetainsNothing => "custody-retains-nothing",
            OperatorRefusal::CustodyBudgetExceeded { .. } => "custody-budget-exceeded",
            OperatorRefusal::CustodyBudgetNotRepresentable { .. } => {
                "custody-budget-not-representable"
            }
            OperatorRefusal::ImageNameKillRefused { .. } => "image-name-kill-refused",
            OperatorRefusal::KillTargetNotAProcess => "kill-target-not-a-process",
            OperatorRefusal::DeletionOfUnrecordedPath { .. } => "deletion-of-unrecorded-path",
            OperatorRefusal::RunTimeoutUnbounded => "run-timeout-unbounded",
            OperatorRefusal::MatrixTimeoutUnbounded => "matrix-timeout-unbounded",
            OperatorRefusal::TimeoutExceedsTierCap { .. } => "timeout-exceeds-tier-cap",
            OperatorRefusal::MatrixTimeoutLeavesNoRoomForCleanup { .. } => {
                "matrix-timeout-leaves-no-room-for-cleanup"
            }
            OperatorRefusal::TooFewEnginesForDifferential { .. } => {
                "too-few-engines-for-differential"
            }
            OperatorRefusal::TooManyEnginesForAttribution { .. } => {
                "too-many-engines-for-attribution"
            }
            OperatorRefusal::RetainedMappingReceiptAbsent => "retained-mapping-receipt-absent",
            OperatorRefusal::MappingReceiptUnnamed => "mapping-receipt-unnamed",
            OperatorRefusal::MappingReadFromWire => "mapping-read-from-wire",
            OperatorRefusal::PidOfferedAsIdentity { .. } => "pid-offered-as-identity",
            OperatorRefusal::EndpointOfferedAsIdentity { .. } => "endpoint-offered-as-identity",
            OperatorRefusal::VersionOfferedAsIdentity { .. } => "version-offered-as-identity",
            OperatorRefusal::CoordinateSourceMismatch { .. } => "coordinate-source-mismatch",
            OperatorRefusal::RetainedInstanceIdentityAbsent => "retained-instance-identity-absent",
            OperatorRefusal::RetainedSlotAmbiguous => "retained-slot-ambiguous",
            OperatorRefusal::BodyContentComparisonUnproven => "body-content-comparison-unproven",
            OperatorRefusal::ObservationDigestAbsent { .. } => "observation-digest-absent",
        }
    }

    /// How many labels this lane mints of its own.
    pub const OWN_LABEL_COUNT: usize = 42;

    /// Every label this lane mints, in [`own_label_index`](Self::own_label_index) order.
    ///
    /// # Why this array cannot go stale
    ///
    /// The binding is [`own_label_index`](Self::own_label_index), whose `match`
    /// is exhaustive over this enum. A new variant makes that match
    /// non-exhaustive and the **crate stops compiling**, so a law cannot be
    /// added without being given a label here — and
    /// `every_own_label_is_produced_by_a_real_variant` then forces the array to
    /// grow with it. That is a compile-time obligation, not a test that can be
    /// left un-run.
    ///
    /// The landed labels are deliberately **not** included. They are
    /// [`DifferentialRefusal::ALL_LABELS`]' to enumerate, and copying them here
    /// would create the second spelling this lane exists not to create.
    pub const OWN_LABELS: [&'static str; Self::OWN_LABEL_COUNT] = [
        "tier-carries-no-caps-of-its-own",
        "execution-not-authorized",
        "matrix-unservable-at-entrypoint",
        "sleep-resume-parked",
        "substitute-boundary-refused",
        "boundary-does-not-match-matrix",
        "host-bounds-unavailable-before-entrypoint",
        "phase-already-entered",
        "phase-out-of-order",
        "phase-after-the-last",
        "run-never-announced",
        "cleanup-phases-not-reached",
        "negative-control-omitted",
        "artifact-unnamed",
        "artifact-class-not-retainable",
        "artifact-budget-unbounded",
        "artifact-declared-twice",
        "receipt-root-unnamed",
        "receipt-root-inside-repository",
        "custody-retains-nothing",
        "custody-budget-exceeded",
        "custody-budget-not-representable",
        "image-name-kill-refused",
        "kill-target-not-a-process",
        "deletion-of-unrecorded-path",
        "run-timeout-unbounded",
        "matrix-timeout-unbounded",
        "timeout-exceeds-tier-cap",
        "matrix-timeout-leaves-no-room-for-cleanup",
        "too-few-engines-for-differential",
        "too-many-engines-for-attribution",
        "retained-mapping-receipt-absent",
        "mapping-receipt-unnamed",
        "mapping-read-from-wire",
        "pid-offered-as-identity",
        "endpoint-offered-as-identity",
        "version-offered-as-identity",
        "coordinate-source-mismatch",
        "retained-instance-identity-absent",
        "retained-slot-ambiguous",
        "body-content-comparison-unproven",
        "observation-digest-absent",
    ];

    /// This refusal's position in [`OWN_LABELS`](Self::OWN_LABELS), or `None`
    /// when it is a landed refusal carried verbatim.
    ///
    /// The exhaustive match is the compile-time obligation described on
    /// `OWN_LABELS`.
    pub fn own_label_index(&self) -> Option<usize> {
        let index = match self {
            OperatorRefusal::Differential(_) => return None,
            OperatorRefusal::TierCarriesNoCapsOfItsOwn { .. } => 0,
            OperatorRefusal::ExecutionNotAuthorized { .. } => 1,
            OperatorRefusal::MatrixUnservableAtEntrypoint { .. } => 2,
            OperatorRefusal::SleepResumeParked => 3,
            OperatorRefusal::SubstituteBoundaryRefused { .. } => 4,
            OperatorRefusal::BoundaryDoesNotMatchMatrix { .. } => 5,
            OperatorRefusal::HostBoundsUnavailableBeforeEntrypoint => 6,
            OperatorRefusal::PhaseAlreadyEntered { .. } => 7,
            OperatorRefusal::PhaseOutOfOrder { .. } => 8,
            OperatorRefusal::PhaseAfterTheLast { .. } => 9,
            OperatorRefusal::RunNeverAnnounced => 10,
            OperatorRefusal::CleanupPhasesNotReached { .. } => 11,
            OperatorRefusal::NegativeControlOmitted { .. } => 12,
            OperatorRefusal::ArtifactUnnamed => 13,
            OperatorRefusal::ArtifactClassNotRetainable { .. } => 14,
            OperatorRefusal::ArtifactBudgetUnbounded { .. } => 15,
            OperatorRefusal::ArtifactDeclaredTwice { .. } => 16,
            OperatorRefusal::ReceiptRootUnnamed => 17,
            OperatorRefusal::ReceiptRootInsideRepository { .. } => 18,
            OperatorRefusal::CustodyRetainsNothing => 19,
            OperatorRefusal::CustodyBudgetExceeded { .. } => 20,
            OperatorRefusal::CustodyBudgetNotRepresentable { .. } => 21,
            OperatorRefusal::ImageNameKillRefused { .. } => 22,
            OperatorRefusal::KillTargetNotAProcess => 23,
            OperatorRefusal::DeletionOfUnrecordedPath { .. } => 24,
            OperatorRefusal::RunTimeoutUnbounded => 25,
            OperatorRefusal::MatrixTimeoutUnbounded => 26,
            OperatorRefusal::TimeoutExceedsTierCap { .. } => 27,
            OperatorRefusal::MatrixTimeoutLeavesNoRoomForCleanup { .. } => 28,
            OperatorRefusal::TooFewEnginesForDifferential { .. } => 29,
            OperatorRefusal::TooManyEnginesForAttribution { .. } => 30,
            OperatorRefusal::RetainedMappingReceiptAbsent => 31,
            OperatorRefusal::MappingReceiptUnnamed => 32,
            OperatorRefusal::MappingReadFromWire => 33,
            OperatorRefusal::PidOfferedAsIdentity { .. } => 34,
            OperatorRefusal::EndpointOfferedAsIdentity { .. } => 35,
            OperatorRefusal::VersionOfferedAsIdentity { .. } => 36,
            OperatorRefusal::CoordinateSourceMismatch { .. } => 37,
            OperatorRefusal::RetainedInstanceIdentityAbsent => 38,
            OperatorRefusal::RetainedSlotAmbiguous => 39,
            OperatorRefusal::BodyContentComparisonUnproven => 40,
            OperatorRefusal::ObservationDigestAbsent { .. } => 41,
        };
        Some(index)
    }
}
