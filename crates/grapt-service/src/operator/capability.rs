#[cfg(doc)]
use super::DifferentialRefusal;
use super::{DifferentialMatrix, Operation, OperatorRefusal};

/// What the Rust engine's entrypoint actually serves.
///
/// Closed, and ordered by capability. The middle variant is the ruled scope of
/// the first entrypoint lane: exactly one served route, health, with a negative
/// control proving a second product route fails the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntrypointCapability {
    /// No entrypoint. The state at this canonical: zero binary targets, no
    /// product route, and every request refused by the credential ruling.
    NoHostRuns,
    /// One route: `v2.health.get`. The first entrypoint lane's ruled scope.
    ExactlyOneHealthRoute,
    /// Health plus the session listing. Requires a separately reviewed session
    /// route lane; it does not exist today and is named so the law that depends
    /// on it can be stated and witnessed now.
    HealthAndSessionList,
}

impl EntrypointCapability {
    pub fn label(self) -> &'static str {
        match self {
            EntrypointCapability::NoHostRuns => "no-host-runs",
            EntrypointCapability::ExactlyOneHealthRoute => "exactly-one-health-route",
            EntrypointCapability::HealthAndSessionList => "health-and-session-list",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        EntrypointCapability::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// The operations this entrypoint serves.
    pub fn serves(self) -> &'static [Operation] {
        match self {
            EntrypointCapability::NoHostRuns => &[],
            EntrypointCapability::ExactlyOneHealthRoute => &[Operation::HealthGet],
            EntrypointCapability::HealthAndSessionList => {
                &[Operation::HealthGet, Operation::SessionList]
            }
        }
    }

    /// Whether this entrypoint can serve a matrix's landed minimum subset.
    ///
    /// # The contradiction this law exists to hold
    ///
    /// Three of the four landed minimum subsets name `v2.session.list`. The
    /// first entrypoint lane is scoped to serve **exactly one** operation. Both
    /// decisions are defensible alone; together they mean an entrypoint-scoped
    /// engine can never satisfy the differential or sleep/resume matrices, and
    /// the long-session matrix has no operation at all. Only `restart`, whose
    /// minimum is health alone, survives.
    ///
    /// The resolution ruled for this lane is to scope the run to `restart` and
    /// leave the subsets untouched — so this function **reads** the landed
    /// subsets and refuses, rather than narrowing them. Amending a minimum
    /// subset would delete the law that makes the differential mean anything,
    /// and that law is not this lane's to delete.
    ///
    /// The missing operations are reported in the landed subset's own order, so
    /// a refusal names them the way the contract enumerates them rather than the
    /// way this function happened to iterate.
    pub fn satisfies(self, matrix: DifferentialMatrix) -> Result<(), OperatorRefusal> {
        let served = self.serves();
        let missing: Vec<Operation> = matrix
            .minimum_subset()
            .iter()
            .copied()
            .filter(|required| !served.contains(required))
            .collect();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(OperatorRefusal::MatrixUnservableAtEntrypoint {
                matrix,
                entrypoint: self,
                missing,
            })
        }
    }

    /// Every matrix this entrypoint can serve, in the landed declaration order.
    ///
    /// Derived by asking [`satisfies`](Self::satisfies) about each matrix rather
    /// than by a second hand-written table. A table would be a place for this
    /// lane's answer to drift from R4D-2A's subsets.
    pub fn servable_matrices(self) -> Vec<DifferentialMatrix> {
        DifferentialMatrix::ALL
            .into_iter()
            .filter(|matrix| self.satisfies(*matrix).is_ok())
            .collect()
    }

    pub const ALL: [EntrypointCapability; 3] = [
        EntrypointCapability::NoHostRuns,
        EntrypointCapability::ExactlyOneHealthRoute,
        EntrypointCapability::HealthAndSessionList,
    ];
}

/// The physical boundary a matrix crosses.
///
/// Named so that a substitute can be refused. Two of these are the substitutes
/// that must never stand in for a host sleep, and they are representable here
/// for exactly the reason the inadmissible [`crate::BoundProvenance`] variants
/// are representable there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BoundaryKind {
    /// No boundary is crossed. What the differential and long-session matrices
    /// use.
    None,
    /// The engine is restarted through its own service lifecycle.
    EngineRestart,
    /// The **host** sleeps and resumes. The only boundary the sleep/resume
    /// matrix names.
    HostSleep,
    /// The engine process is suspended and resumed. **Not a host sleep.**
    ProcessSuspend,
    /// The network is dropped and restored. **Not a host sleep.**
    NetworkDrop,
}

impl BoundaryKind {
    pub fn label(self) -> &'static str {
        match self {
            BoundaryKind::None => "none",
            BoundaryKind::EngineRestart => "engine-restart",
            BoundaryKind::HostSleep => "host-sleep",
            BoundaryKind::ProcessSuspend => "process-suspend",
            BoundaryKind::NetworkDrop => "network-drop",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        BoundaryKind::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// Whether this boundary is being offered in place of a genuine host sleep.
    pub fn is_sleep_substitute(self) -> bool {
        matches!(
            self,
            BoundaryKind::ProcessSuspend | BoundaryKind::NetworkDrop
        )
    }

    pub const ALL: [BoundaryKind; 5] = [
        BoundaryKind::None,
        BoundaryKind::EngineRestart,
        BoundaryKind::HostSleep,
        BoundaryKind::ProcessSuspend,
        BoundaryKind::NetworkDrop,
    ];
}

/// One deliberate refusal an operator must observe for the instrument to be
/// proven.
///
/// # Why these are mandatory rather than recommended
///
/// A run that only produces greens has not tested its instrument. Each control
/// below drives the contract into a refusal an operator can see, and each names
/// a **different** landed refusal — so a plan that declares all of them exercises
/// six independent failure paths rather than one path six times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NegativeControl {
    /// Point both sides of the differential at one engine.
    BothSidesOneEngine,
    /// Admit a probe that answered 2xx but named no owner.
    ProbeNamedNoOwner,
    /// Reuse a recorded receipt after the engine was replaced.
    ReceiptReusedAfterReplacement,
    /// Declare a subset narrower than the matrix's landed minimum.
    SubsetMissingRequiredOperation,
    /// Record a bound the driver supplied itself.
    DriverSuppliedBound,
    /// Finish with the selector still attached.
    SelectorLeftAttached,
}

impl NegativeControl {
    pub fn label(self) -> &'static str {
        match self {
            NegativeControl::BothSidesOneEngine => "both-sides-one-engine",
            NegativeControl::ProbeNamedNoOwner => "probe-named-no-owner",
            NegativeControl::ReceiptReusedAfterReplacement => "receipt-reused-after-replacement",
            NegativeControl::SubsetMissingRequiredOperation => "subset-missing-required-operation",
            NegativeControl::DriverSuppliedBound => "driver-supplied-bound",
            NegativeControl::SelectorLeftAttached => "selector-left-attached",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        NegativeControl::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// The landed refusal word this control must produce.
    ///
    /// Every one of these strings is a member of
    /// [`DifferentialRefusal::ALL_LABELS`], and a test asserts exactly that
    /// against the landed array rather than against a literal of its own. That
    /// is what keeps this table honest: a control expecting a word the contract
    /// cannot produce would be a control that can never fire, and it would look
    /// identical to one that simply passed.
    pub fn expected_refusal_label(self) -> &'static str {
        match self {
            NegativeControl::BothSidesOneEngine => "same-owner-on-both-sides",
            NegativeControl::ProbeNamedNoOwner => "unattributed",
            NegativeControl::ReceiptReusedAfterReplacement => "not-the-recorded-owner",
            NegativeControl::SubsetMissingRequiredOperation => "matrix-subset-incomplete",
            NegativeControl::DriverSuppliedBound => "bound-not-reported-by-host",
            NegativeControl::SelectorLeftAttached => "leaves-selector-attached",
        }
    }

    pub const ALL: [NegativeControl; 6] = [
        NegativeControl::BothSidesOneEngine,
        NegativeControl::ProbeNamedNoOwner,
        NegativeControl::ReceiptReusedAfterReplacement,
        NegativeControl::SubsetMissingRequiredOperation,
        NegativeControl::DriverSuppliedBound,
        NegativeControl::SelectorLeftAttached,
    ];
}
