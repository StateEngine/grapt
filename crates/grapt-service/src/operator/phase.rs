#[cfg(doc)]
use super::DifferentialPlan;
use super::{OperatorRefusal, RunAuthorization};

/// The ordered phases of an attended run.
///
/// The order is the law. Two orderings in particular are load-bearing and are
/// witnessed separately from the general ordering rule:
///
/// * **reversal precedes stopping.** On success, on failure, on timeout and on
///   abort the first action is to return the selector to the retained engine
///   and read it back. Only then are engines stopped, and only then is anything
///   diagnosed. A run that diagnosed first would leave the daily driver on a
///   borrowed engine for the length of the investigation.
/// * **enumeration precedes the closing host record.** The leftovers are
///   recorded as observed, before the run declares itself finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RunPhase {
    /// Announce the window on the floor. The only phase a parked plan reaches.
    Announce,
    /// Record free memory, process count and window start.
    RecordHostStateAtStart,
    /// Start both engines, inside the window, by the operator.
    StartEngines,
    /// Mint the selection receipts and admit the pair.
    AdmitAttachments,
    /// Exercise the declared subset against each engine.
    ExerciseSubset,
    /// Clear `defaultServerUrl` and read it back.
    RevertSelector,
    /// Stop the engines through their own lifecycles, PID-scoped.
    StopEngines,
    /// List the state home and record every file present.
    EnumerateLeftovers,
    /// Record free memory, process count and wall clock consumed.
    RecordHostStateAtStop,
}

impl RunPhase {
    pub fn label(self) -> &'static str {
        match self {
            RunPhase::Announce => "announce",
            RunPhase::RecordHostStateAtStart => "record-host-state-at-start",
            RunPhase::StartEngines => "start-engines",
            RunPhase::AdmitAttachments => "admit-attachments",
            RunPhase::ExerciseSubset => "exercise-subset",
            RunPhase::RevertSelector => "revert-selector",
            RunPhase::StopEngines => "stop-engines",
            RunPhase::EnumerateLeftovers => "enumerate-leftovers",
            RunPhase::RecordHostStateAtStop => "record-host-state-at-stop",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        RunPhase::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// This phase's position in the run.
    ///
    /// Derived from the declaration order rather than written as a second
    /// literal, so the ordinal and the sequence cannot disagree.
    pub fn ordinal(self) -> usize {
        RunPhase::ALL
            .into_iter()
            .position(|candidate| candidate == self)
            .unwrap_or(usize::MAX)
    }

    /// Whether this phase requires an authorization to execute.
    ///
    /// The announcement does not: announcing a window is how a parked plan is
    /// published, and refusing to let a parked plan even be announced would make
    /// the park invisible.
    pub fn requires_execution_authority(self) -> bool {
        self != RunPhase::Announce
    }

    pub const ALL: [RunPhase; 9] = [
        RunPhase::Announce,
        RunPhase::RecordHostStateAtStart,
        RunPhase::StartEngines,
        RunPhase::AdmitAttachments,
        RunPhase::ExerciseSubset,
        RunPhase::RevertSelector,
        RunPhase::StopEngines,
        RunPhase::EnumerateLeftovers,
        RunPhase::RecordHostStateAtStop,
    ];
}

/// How a gated run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateConclusion {
    /// The plan was announced and never executed. The correct terminal state
    /// for every plan this crate can build today.
    ParkedAtAnnouncement,
    /// Every phase ran, cleanup included.
    CompletedWithCleanup,
}

impl GateConclusion {
    pub fn label(self) -> &'static str {
        match self {
            GateConclusion::ParkedAtAnnouncement => "parked-at-announcement",
            GateConclusion::CompletedWithCleanup => "completed-with-cleanup",
        }
    }
}

/// The gate an attended run passes through, phase by phase.
///
/// Holding one that has reached [`RunPhase::RecordHostStateAtStop`] is the claim
/// that the run's cleanup actually happened — which is why
/// [`conclude`](Self::conclude) refuses a run that started and stopped in
/// between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseGate {
    authorization: RunAuthorization,
    entered: Vec<RunPhase>,
}

impl PhaseGate {
    /// Open a gate for a run under this authorization.
    pub fn open(authorization: RunAuthorization) -> Self {
        Self {
            authorization,
            entered: Vec::new(),
        }
    }

    /// Enter a phase, or refuse.
    ///
    /// Four laws, in refusal order:
    ///
    /// 1. **the run is over** — once the closing host record is entered there is
    ///    no next phase, and anything offered afterwards is refused as being
    ///    after the last rather than as a repeat. This check is **first** on
    ///    purpose: with it second, every post-run attempt would be caught by the
    ///    re-entry rule instead, and `PhaseAfterTheLast` would be a variant no
    ///    input could ever reach — a law that cannot fire is a law nothing
    ///    witnesses;
    /// 2. **no re-entry** — a phase entered twice would let a run record two
    ///    cleanups, or two reversals, and a projection counting phases could not
    ///    tell the difference;
    /// 3. **strict order** — each phase follows exactly the one before it, so
    ///    "reversal precedes stopping" is enforced rather than intended;
    /// 4. **execution authority** — every phase past the announcement requires
    ///    a scheduled window. This is where the tier ruling becomes mechanical:
    ///    the tier is declared, its caps are data, and its execution is refused.
    ///
    /// Authority is checked **last** so that a plan wrong in two ways names the
    /// structural defect before the authorization one — the same ordering
    /// convention the landed [`DifferentialPlan::admit`] uses.
    ///
    /// The next expected phase is `entered.len()` rather than the previous
    /// phase's ordinal plus one. The two agree only because entries are pushed
    /// in strict order, and taking it from the length means the invariant is
    /// read off the structure that enforces it rather than recomputed from a
    /// value that could drift.
    pub fn enter(&mut self, phase: RunPhase) -> Result<(), OperatorRefusal> {
        let expected = match RunPhase::ALL.get(self.entered.len()) {
            Some(next) => *next,
            None => return Err(OperatorRefusal::PhaseAfterTheLast { phase }),
        };
        if self.entered.contains(&phase) {
            return Err(OperatorRefusal::PhaseAlreadyEntered { phase });
        }
        if phase != expected {
            return Err(OperatorRefusal::PhaseOutOfOrder {
                attempted: phase,
                expected,
            });
        }
        if phase.requires_execution_authority() && !self.authorization.admits_execution() {
            return Err(OperatorRefusal::ExecutionNotAuthorized {
                phase,
                park_reason: self.authorization.park_reason(),
            });
        }
        self.entered.push(phase);
        Ok(())
    }

    pub fn entered(&self) -> &[RunPhase] {
        &self.entered
    }

    /// Conclude the run, or refuse the conclusion.
    ///
    /// Exactly two legal terminal states. A run that never left the
    /// announcement is parked, which is correct and expected. A run that
    /// completed every phase cleaned up. **Anything in between is refused**: a
    /// run that started engines and stopped before its cleanup phases left
    /// processes and files on a host shared with live seats, and no receipt it
    /// produced should be readable as a completed run.
    pub fn conclude(&self) -> Result<GateConclusion, OperatorRefusal> {
        match self.entered.last() {
            None => Err(OperatorRefusal::RunNeverAnnounced),
            Some(RunPhase::Announce) => Ok(GateConclusion::ParkedAtAnnouncement),
            Some(RunPhase::RecordHostStateAtStop) => Ok(GateConclusion::CompletedWithCleanup),
            Some(reached) => Err(OperatorRefusal::CleanupPhasesNotReached { reached: *reached }),
        }
    }
}

/// One component's contribution to the run receipt.
///
/// Produced by the model from what it was actually declared, never written by
/// hand. That is the same discipline the landed corpus uses for its expected
/// surfaces, and for the same reason: a hand-written receipt goes stale the
/// first time a declaration changes, and nothing notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentReceipt {
    component: &'static str,
    declared: String,
}

impl ComponentReceipt {
    pub fn new(component: &'static str, declared: impl Into<String>) -> Self {
        Self {
            component,
            declared: declared.into(),
        }
    }

    pub fn component(&self) -> &'static str {
        self.component
    }

    pub fn declared(&self) -> &str {
        &self.declared
    }
}
