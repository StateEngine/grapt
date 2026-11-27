use super::{
    AdmittedDifferential, ArtifactCustody, BoundaryKind, ComparisonScope, ComponentReceipt,
    DifferentialMatrix, DifferentialPlan, EngineProcessCount, EntrypointCapability, EvidenceTier,
    KillDiscipline, MatrixEvidence, NegativeControl, OperationSubset, OperatorRefusal,
    RecordedHostBounds, RetainedOwnerMapping, RunAuthorization, SelectorState, TierCaps,
    TimeoutBudget,
};

/// Everything an operator plan is declared from.
///
/// A parts struct rather than a twelve-argument constructor. The workspace lint
/// budget counts `self`, and bundling is the house answer to argument pressure —
/// reached for here rather than an `allow`, exactly as the permission settlement
/// vocabulary does in `grapt-types`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorPlanParts {
    pub tier: EvidenceTier,
    pub authorization: RunAuthorization,
    pub entrypoint: EntrypointCapability,
    pub matrix: DifferentialMatrix,
    pub boundary: BoundaryKind,
    pub custody: ArtifactCustody,
    pub timeout: TimeoutBudget,
    pub engines: EngineProcessCount,
    pub kill: KillDiscipline,
    pub mapping: Option<RetainedOwnerMapping>,
    pub comparison: ComparisonScope,
    pub controls: Vec<NegativeControl>,
}

/// A declared operator plan the contract admits.
///
/// Holding one is the claim that a run *could* be scheduled, never that it ran
/// and never that any engine exists. The one thing it deliberately cannot become
/// is a [`DifferentialPlan`]: see [`construct_differential_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorPlan {
    parts: OperatorPlanParts,
}

impl OperatorPlan {
    /// Declare a plan, or refuse it.
    ///
    /// Over-budget custody, unbounded timeout, and attribute-sourced mapping
    /// have already been refused by their own constructors. The kill discipline
    /// and comparison scope are admitted here so a caller cannot bypass their
    /// refusals by omission. What is left is the relations between components,
    /// plus the rulings that scope this lane:
    ///
    /// 1. the tier must be one with caps of its own, since a live run has to be
    ///    bounded by something;
    /// 2. a substitute boundary is refused **before** the sleep/resume park,
    ///    because offering a process suspend as a host sleep is the more
    ///    specific defect and reporting only the park would hide it;
    /// 3. sleep/resume is parked pending a fleet quiesce or a second host;
    /// 4. the matrix must be servable at the declared entrypoint — the landed
    ///    minimum subsets decide this, not this lane;
    /// 5. the boundary must match the matrix, so a restart matrix cannot be run
    ///    across no boundary at all;
    /// 6. a retained mapping receipt is required — the run fails closed without
    ///    one rather than attributing the retained side by guesswork;
    /// 7. every negative control is declared.
    pub fn declare(mut parts: OperatorPlanParts) -> Result<Self, OperatorRefusal> {
        if TierCaps::for_tier(parts.tier).is_none() {
            return Err(OperatorRefusal::TierCarriesNoCapsOfItsOwn { tier: parts.tier });
        }

        if parts.boundary.is_sleep_substitute() {
            return Err(OperatorRefusal::SubstituteBoundaryRefused {
                offered: parts.boundary,
            });
        }
        if parts.matrix == DifferentialMatrix::SleepResume {
            return Err(OperatorRefusal::SleepResumeParked);
        }

        parts.entrypoint.satisfies(parts.matrix)?;

        let required_boundary = match parts.matrix {
            DifferentialMatrix::Restart => BoundaryKind::EngineRestart,
            DifferentialMatrix::SleepResume => BoundaryKind::HostSleep,
            DifferentialMatrix::Differential | DifferentialMatrix::LongSession => {
                BoundaryKind::None
            }
        };
        if parts.boundary != required_boundary {
            return Err(OperatorRefusal::BoundaryDoesNotMatchMatrix {
                matrix: parts.matrix,
                offered: parts.boundary,
                required: required_boundary,
            });
        }

        if parts.mapping.is_none() {
            return Err(OperatorRefusal::RetainedMappingReceiptAbsent);
        }

        let missing: Vec<NegativeControl> = NegativeControl::ALL
            .into_iter()
            .filter(|control| !parts.controls.contains(control))
            .collect();
        if !missing.is_empty() {
            return Err(OperatorRefusal::NegativeControlOmitted { missing });
        }

        parts.kill = parts.kill.clone().admit()?;

        // Body-content comparison stays closed until the later common-store
        // owner can supply its sealed proof. External plan callers cannot mint
        // that proof today, so the production seam admits only the governed
        // shape-and-status scope.
        parts.comparison = parts.comparison.admit(None)?;

        Ok(Self { parts })
    }

    pub fn tier(&self) -> EvidenceTier {
        self.parts.tier
    }

    pub fn authorization(&self) -> &RunAuthorization {
        &self.parts.authorization
    }

    pub fn entrypoint(&self) -> EntrypointCapability {
        self.parts.entrypoint
    }

    pub fn matrix(&self) -> DifferentialMatrix {
        self.parts.matrix
    }

    pub fn boundary(&self) -> BoundaryKind {
        self.parts.boundary
    }

    pub fn custody(&self) -> &ArtifactCustody {
        &self.parts.custody
    }

    pub fn timeout(&self) -> TimeoutBudget {
        self.parts.timeout
    }

    pub fn engines(&self) -> EngineProcessCount {
        self.parts.engines
    }

    pub fn kill(&self) -> &KillDiscipline {
        &self.parts.kill
    }

    pub fn mapping(&self) -> Option<&RetainedOwnerMapping> {
        self.parts.mapping.as_ref()
    }

    pub fn comparison(&self) -> ComparisonScope {
        self.parts.comparison
    }

    pub fn controls(&self) -> &[NegativeControl] {
        &self.parts.controls
    }

    /// The receipts every component of this plan contributes.
    ///
    /// Derived from the plan's own answers in a fixed order, so a corpus that
    /// records them cannot go stale relative to what was declared.
    pub fn component_receipts(&self) -> Vec<ComponentReceipt> {
        let mut receipts = vec![
            ComponentReceipt::new("evidence-tier", self.tier().label()),
            ComponentReceipt::new(
                "run-authorization",
                match self.authorization().park_reason() {
                    Some(reason) => reason.label(),
                    None => "scheduled",
                },
            ),
            ComponentReceipt::new("entrypoint", self.entrypoint().label()),
            ComponentReceipt::new("matrix", self.matrix().label()),
            ComponentReceipt::new("boundary", self.boundary().label()),
            ComponentReceipt::new(
                "artifact-custody",
                format!(
                    "{} artifact(s), {} declared byte(s), root {}",
                    self.custody().artifacts().len(),
                    self.custody().declared_bytes(),
                    self.custody().root().receipt_root()
                ),
            ),
            ComponentReceipt::new(
                "timeout-budget",
                format!(
                    "{} ms whole run, {} ms per matrix",
                    self.timeout().whole_run_ms(),
                    self.timeout().per_matrix_ms()
                ),
            ),
            ComponentReceipt::new("engine-processes", format!("{}", self.engines().get())),
            ComponentReceipt::new("kill-discipline", self.kill().label()),
            ComponentReceipt::new("comparison-scope", self.comparison().label()),
        ];
        receipts.push(ComponentReceipt::new(
            "retained-owner-mapping",
            match self.mapping() {
                Some(mapping) => mapping.receipt().receipt_id().to_owned(),
                None => "absent".to_owned(),
            },
        ));
        receipts.push(ComponentReceipt::new(
            "negative-controls",
            format!("{}", self.controls().len()),
        ));
        receipts
    }
}

/// Where a driver's host-bound recording came from.
///
/// The whole reason this enum exists is its first variant. A recording is
/// impossible before a host runs, and an automation that had no way to *say*
/// "no host ran" would have to either fabricate a recording or omit the
/// question — and omitting it is how a pre-entrypoint artifact quietly claims a
/// plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostBoundsSource {
    /// No Rust host has run, so there is no receipt to record from. The state at
    /// this canonical, and the state every pre-entrypoint run is in.
    NoHostHasRun,
    /// A host reported its bounds and the driver recorded them.
    ReportedByHost(RecordedHostBounds),
}

/// Everything [`construct_differential_plan`] needs beyond the operator plan.
///
/// Bundled for the same argument-budget reason as [`OperatorPlanParts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DifferentialConstruction {
    pub bounds: HostBoundsSource,
    pub subset: OperationSubset,
    pub engines: AdmittedDifferential,
    pub evidence: MatrixEvidence,
    pub terminal_selector: SelectorState,
}

/// Construct a [`DifferentialPlan`] from an admitted operator plan, or refuse
/// loudly.
///
/// # This is the function that must refuse today
///
/// It is the only path from this lane's components to R4D-2A's plan, and it
/// gates on the fact that does not exist before an entrypoint lands: **bounds
/// the host itself reported**. `RecordedHostBounds` admits only
/// `ReportedByHostReceipt`, and there is no `limits()` accessor to read until a
/// host is ready. [`HostBoundsSource::NoHostHasRun`] is refused here rather than
/// papered over with a default, because an unreported bound is a gap and never a
/// default — the landed law this refusal defers to.
///
/// # Why there is no separate "no entrypoint" refusal here
///
/// There was one, and it could not fire. An [`OperatorPlan`] naming
/// [`EntrypointCapability::NoHostRuns`] is unconstructible: `NoHostRuns` serves
/// no operation, so [`EntrypointCapability::satisfies`] refuses **every** matrix
/// at declaration with [`OperatorRefusal::MatrixUnservableAtEntrypoint`]. A
/// second check here could therefore never be reached by any input, and a law
/// nothing can violate is a law nothing witnesses. The absent-entrypoint case is
/// carried where it is reachable — at declaration — and witnessed there.
///
/// Past the bounds gate this delegates to [`DifferentialPlan::admit`] and
/// carries its refusal verbatim. This lane adds a precondition to that
/// constructor; it does not re-implement it, and it cannot loosen it.
pub fn construct_differential_plan(
    plan: &OperatorPlan,
    construction: DifferentialConstruction,
) -> Result<DifferentialPlan, OperatorRefusal> {
    let bounds = match construction.bounds {
        HostBoundsSource::NoHostHasRun => {
            return Err(OperatorRefusal::HostBoundsUnavailableBeforeEntrypoint)
        }
        HostBoundsSource::ReportedByHost(bounds) => bounds,
    };
    let admitted = DifferentialPlan::admit(
        plan.matrix(),
        construction.subset,
        construction.engines,
        bounds,
        construction.evidence,
        construction.terminal_selector,
    )?;
    Ok(admitted)
}
