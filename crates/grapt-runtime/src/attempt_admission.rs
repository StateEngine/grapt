//! Sealed attempt-admission boundary.
//!
//! A downstream caller can construct only a typed refusal. The unopened
//! carrier and its one-shot reader factory stay crate-private; only this module
//! can combine that carrier with durable reservation evidence and enter the
//! private attempt supervisor.

use std::{future::Future, sync::Arc};

use grapt_types::{
    failure_diagnostic::{AdmissionRefusalWord, StepFailureDiagnostic},
    PhysicalAttemptId,
};

use tokio::sync::watch;

use crate::owner::AttemptOwner;
use crate::residency::Residency;
use crate::supervisor::{
    AttemptReport, AttemptSupervisor, InterruptHandle, ProviderReader, ToolRunner,
};
use grapt_types::InterruptionSource;

type ReaderFactory<R> = Box<dyn FnOnce() -> R + Send>;

/// Closed pre-provider refusal. It is operational truth, not an effect result.
///
/// The U2 variants below are the production provider source failing closed:
/// each names a precondition absent BEFORE any identity was minted, receipt
/// reserved or byte staged. Carried detail is diagnosis; nothing branches on it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttemptAdmissionRefusal {
    #[error("no authenticated campaign-attempt admission issuer is installed")]
    NoIssuer,
    /// No current Step terminal handoff, or one naming a different Step: the
    /// receipt this attempt would reserve could not name the terminal its
    /// settlement will commit, so nothing is reserved and nothing opens.
    #[error("the attempt source holds no terminal handoff for this Step: {detail}")]
    MissingStepTerminal { detail: String },
    /// Local read capacity is temporarily occupied; no attempt exists yet.
    #[error("waiting for capacity")]
    CapacityBusy,
    /// The per-attempt plan (authority snapshot plus request) was refused.
    #[error("the attempt plan was refused: {detail}")]
    PlanRefused { detail: String },
    /// A required context boundary refused before reserving any provider attempt.
    #[error("required context preparation failed: {failure:?}")]
    ContextUnavailable {
        failure: grapt_types::ContextFailure,
        /// A closed diagnosis from context refresh, never arbitrary remote text.
        continuity: Option<grapt_session::execution::ContinuityFailure>,
    },
    /// Context preparation exhausted its inactivity budget before a provider opened.
    #[error("context preparation timed out at {stage:?}; send a follow-up prompt to retry")]
    ContextPreparationTimedOut {
        stage: crate::step::ContextPreparationStage,
    },
    /// No attempt exists yet; the owning Step may perform its bounded rebuild.
    #[error("context requires compaction before provider admission: {trigger:?}")]
    CompactionRequired {
        trigger: grapt_types::CompactionTrigger,
    },
    /// The provider opener refused the binding — stale issuance, coordinate
    /// disagreement, configuration or staging fault — with zero provider bytes.
    #[error("the provider opener refused: {detail}")]
    OpenRefused { detail: String },
    /// The adapter's active-attempt capacity stayed full for the bounded wait:
    /// saturation, which passes, and not a fault in the binding (SC-REL-13).
    #[error("the provider adapter has no free attempt slot: {detail}; temporary contention is retryable: resume this session, or use followup_task for a child")]
    Saturated { detail: String },
    /// The store refused to reserve the durable attempt receipt or its send
    /// permit. The staged reader is discarded unpolled: an attempt that cannot
    /// be made durable does not open.
    #[error("the durable attempt receipt was refused: {detail}")]
    StoreRefused { detail: String },
}

impl AttemptAdmissionRefusal {
    /// The closed diagnostic words for the terminal open-path classes. The
    /// context paths and the nonterminal capacity wait keep their own handling.
    pub(crate) fn admission_diagnostic(&self) -> Option<StepFailureDiagnostic> {
        let refusal = match self {
            Self::NoIssuer => AdmissionRefusalWord::NoIssuer,
            Self::MissingStepTerminal { .. } => AdmissionRefusalWord::MissingStepTerminal,
            Self::PlanRefused { .. } => AdmissionRefusalWord::PlanRefused,
            Self::OpenRefused { .. } => AdmissionRefusalWord::OpenRefused,
            Self::Saturated { .. } => AdmissionRefusalWord::Saturated,
            Self::StoreRefused { .. } => AdmissionRefusalWord::StoreRefused,
            Self::CapacityBusy
            | Self::ContextUnavailable { .. }
            | Self::ContextPreparationTimedOut { .. }
            | Self::CompactionRequired { .. } => return None,
        };
        StepFailureDiagnostic::admission_refused(refusal, None)
    }
}

/// Evidence that one ordinary attempt's durable receipt was reserved and its
/// send permit committed before this preparation existed. Crate-private: the
/// only producer is the provider source, after its reserve commit.
#[derive(Debug)]
pub struct ReservedAttemptAdmission {
    attempt: PhysicalAttemptId,
}

impl ReservedAttemptAdmission {
    pub(crate) fn new(attempt: PhysicalAttemptId) -> Self {
        Self { attempt }
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }
}

enum AttemptAdmission {
    /// Ordinary admission, evidenced by the durable reservation.
    Reserved(ReservedAttemptAdmission),
    Refused(AttemptAdmissionRefusal),
}

/// A provider attempt whose factory cannot be observed or invoked by callers.
pub struct UnopenedAttempt<R> {
    attempt: PhysicalAttemptId,
    factory: ReaderFactory<R>,
}

impl<R> core::fmt::Debug for UnopenedAttempt<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnopenedAttempt")
            .field("attempt", &self.attempt)
            .field("factory", &"<sealed>")
            .finish()
    }
}

impl<R> UnopenedAttempt<R> {
    // The `expect(dead_code)` that stood here is gone: the production caller
    // it waited for is `crate::provider_source::ProviderAttemptSource`.
    pub(crate) fn new(
        attempt: PhysicalAttemptId,
        factory: impl FnOnce() -> R + Send + 'static,
    ) -> Self {
        Self {
            attempt,
            factory: Box::new(factory),
        }
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    fn into_parts(self) -> (PhysicalAttemptId, ReaderFactory<R>) {
        (self.attempt, self.factory)
    }
}

/// The only carrier a public attempt source can return.
pub struct AttemptPreparation<R> {
    admission: AttemptAdmission,
    unopened: Option<UnopenedAttempt<R>>,
}

impl<R> AttemptPreparation<R> {
    pub(crate) fn context_preparation_timeout(&self) -> Option<AttemptAdmissionRefusal> {
        match &self.admission {
            AttemptAdmission::Refused(
                refusal @ AttemptAdmissionRefusal::ContextUnavailable {
                    failure: grapt_types::ContextFailure::ContinuityUnavailable,
                    continuity: Some(grapt_session::execution::ContinuityFailure::TimedOut { .. }),
                },
            ) => Some(refusal.clone()),
            _ => None,
        }
    }

    pub(crate) fn compaction_required(&self) -> Option<grapt_types::CompactionTrigger> {
        match &self.admission {
            AttemptAdmission::Refused(AttemptAdmissionRefusal::CompactionRequired { trigger }) => {
                Some(*trigger)
            }
            _ => None,
        }
    }
}

pub(crate) struct PreparedAttempt<O, F> {
    pub(crate) attempt: PhysicalAttemptId,
    pub(crate) interrupt: InterruptHandle,
    pub(crate) run: F,
    pub(crate) owner: core::marker::PhantomData<fn() -> O>,
}

impl<R> core::fmt::Debug for AttemptPreparation<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AttemptPreparation")
            .field("unopened", &self.unopened)
            .finish_non_exhaustive()
    }
}

impl<R> AttemptPreparation<R> {
    /// Production construction today: fail closed without an identity or opener
    /// while the authenticated issuer is unavailable.
    pub fn refused(refusal: AttemptAdmissionRefusal) -> Self {
        Self {
            admission: AttemptAdmission::Refused(refusal),
            unopened: None,
        }
    }

    /// A refusal that ALSO carries an unopened attempt — a shape production
    /// cannot build, because [`Self::refused`] has no factory to carry and a
    /// carrier-bearing preparation is never refused.
    ///
    /// It exists so the refusal arm's law is FALSIFIABLE: against an ordinary
    /// refusal the arm holds nothing, so an arm that invoked what it holds and
    /// one that returns first are indistinguishable. A factory is the only way
    /// to ask whether it reaches for one.
    #[cfg(test)]
    fn refused_carrying(refusal: AttemptAdmissionRefusal, unopened: UnopenedAttempt<R>) -> Self {
        // Bound through a local rather than written inline. The inline form is
        // the production anchor TD25 mutates in `refused` above, and mutation
        // anchors are matched as unique text: a second literal occurrence here
        // scores that row SNIPPET AMBIGUOUS, which aborts the WHOLE battery at
        // preflight rather than failing one row. A test-only constructor must
        // not spell a production anchor.
        let admission = AttemptAdmission::Refused(refusal);
        Self {
            admission,
            unopened: Some(unopened),
        }
    }

    /// Ordinary admission from a durably reserved attempt. Crate-private: the
    /// only caller is the production provider source, and probe 75 continues to
    /// prove a public source cannot manufacture an admitted preparation.
    pub(crate) fn reserved(
        unopened: UnopenedAttempt<R>,
        evidence: ReservedAttemptAdmission,
    ) -> Self {
        Self {
            admission: AttemptAdmission::Reserved(evidence),
            unopened: Some(unopened),
        }
    }
}

/// The sole public attempt boundary. Refusal returns before factory invocation.
#[derive(Debug)]
pub struct AttemptProtocol;

impl AttemptProtocol {
    pub(crate) fn prepare<R, T, O>(
        preparation: AttemptPreparation<R>,
        _tools: Arc<T>,
        owner: O,
        _residency: Residency,
        step: watch::Receiver<Option<InterruptionSource>>,
    ) -> Result<
        PreparedAttempt<O, impl Future<Output = (O, AttemptReport)>>,
        (O, AttemptAdmissionRefusal),
    >
    where
        R: ProviderReader,
        T: ToolRunner,
        O: AttemptOwner,
    {
        match preparation.admission {
            AttemptAdmission::Refused(refusal) => Err((owner, refusal)),
            AttemptAdmission::Reserved(evidence) => {
                let Some(unopened) = preparation.unopened else {
                    return Err((owner, AttemptAdmissionRefusal::NoIssuer));
                };
                // The reservation evidence must name the attempt whose stream
                // this admits: a mismatch means the reserve and the opener
                // parted ways, and the fail-closed answer is a refusal before
                // any factory is invoked.
                if evidence.attempt() != unopened.attempt() {
                    return Err((
                        owner,
                        AttemptAdmissionRefusal::StoreRefused {
                            detail: format!(
                                "the reservation names {} but the opened attempt is {}",
                                evidence.attempt().as_str(),
                                unopened.attempt().as_str()
                            ),
                        },
                    ));
                }
                Ok(Self::prepared(unopened, _tools, owner, _residency, step))
            }
        }
    }

    fn prepared<R, T, O>(
        unopened: UnopenedAttempt<R>,
        tools: Arc<T>,
        owner: O,
        residency: Residency,
        step: watch::Receiver<Option<InterruptionSource>>,
    ) -> PreparedAttempt<O, impl Future<Output = (O, AttemptReport)>>
    where
        R: ProviderReader,
        T: ToolRunner,
        O: AttemptOwner,
    {
        let (attempt, factory) = unopened.into_parts();
        let returned_attempt = attempt.clone();
        let prepared = AttemptSupervisor::prepare(attempt, factory, tools, owner, residency, step);
        PreparedAttempt {
            attempt: returned_attempt,
            interrupt: prepared.0,
            run: prepared.1,
            owner: core::marker::PhantomData,
        }
    }
}

#[cfg(test)]
mod boundary_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use grapt_types::{PhysicalAttemptId, ToolCallId, ToolTerminalOutcome};

    use super::{
        AttemptAdmissionRefusal, AttemptPreparation, AttemptProtocol, ProviderReader, ToolRunner,
        UnopenedAttempt,
    };
    use crate::{BoxFuture, DrainResidency, RecordingOwner};

    /// A Step watch with no sender and no value: these fixtures exercise
    /// admission, so losing the Step sender must leave the attempt running.
    fn step_watch() -> tokio::sync::watch::Receiver<Option<grapt_types::InterruptionSource>> {
        let (sender, receiver) = tokio::sync::watch::channel(None);
        drop(sender);
        receiver
    }

    struct NeverReader;

    impl ProviderReader for NeverReader {
        async fn next(&mut self) -> Option<grapt_provider::AttemptFragment> {
            None
        }
    }

    struct NeverTools;

    impl ToolRunner for NeverTools {
        fn run(
            &self,
            _call: ToolCallId,
            _tool: String,
            _input: crate::tool_protocol::BoundedToolInput,
        ) -> BoxFuture<'static, ToolTerminalOutcome> {
            panic!("attempt-admission refusal must not run a tool")
        }
    }

    fn id<T>(value: Option<T>, label: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("static {label} must be valid"),
        }
    }

    /// A typed refusal returns BEFORE any reader factory is invoked — asked of
    /// an arm that is HOLDING one. The counting factory is the observation the
    /// public witness cannot make: that one constructs an ordinary refusal,
    /// which carries no factory, so it would stay green against an arm that
    /// invoked whatever it held.
    #[test]
    fn a_refusal_carrying_an_unopened_attempt_invokes_zero_reader_factories() {
        let invocations = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&invocations);
        let unopened = UnopenedAttempt::new(
            id(PhysicalAttemptId::new("pat_refusal_carrier"), "attempt"),
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                NeverReader
            },
        );
        let preparation =
            AttemptPreparation::refused_carrying(AttemptAdmissionRefusal::NoIssuer, unopened);
        let result = AttemptProtocol::prepare(
            preparation,
            Arc::new(NeverTools),
            RecordingOwner::new(),
            DrainResidency::new().token(),
            step_watch(),
        );
        match result {
            Err((_owner, refusal)) => assert_eq!(refusal, AttemptAdmissionRefusal::NoIssuer),
            Ok(_) => panic!("a typed refusal must refuse"),
        }
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "a typed refusal must return before any reader factory is invoked"
        );
    }
}

#[cfg(test)]
mod tests;
