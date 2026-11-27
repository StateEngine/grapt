//! The concrete production [`SessionApplication`]: the Session Inbox, the
//! Step driver, the Session store, and the Location service map wired into one
//! adapter the [`SessionCoordinator`](crate::SessionCoordinator) can drive.
//! Law: `SECODE_RUST_FOUNDATIONAL_TYPES_AND_LIFECYCLES_2026-08-07.md`
//! §"Session lifecycle"; `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! rules 1 and 6; and the delivery law in `AGENTS.md`: placement is discovered
//! "through `SessionStore` plus `LocationServiceMap.get(session.location)` only
//! when a drain starts", steers "promote at the next safe step boundary while
//! the current drain requires continuation", a queued input "remains pending
//! until the Session would otherwise become idle", and "promoting any new user
//! input resets the selected agent's step allowance; a batch of steers resets it
//! once".
//! # One drain, one Location: [`RuntimeDrain`]
//! [`SessionApplication::begin_drain`] reads the Session's seed, canonicalizes
//! it to a [`LocationId`], resolves that Location's runtime through the service
//! map, and hands back a context that OWNS the resolved
//! `Arc<`[`LocationRuntime`]`>` for the drain's whole life.
//! Nothing below it may resolve again. An earlier version of this adapter
//! called [`LocationServiceMap::get`] once per promotion, which is not a cache
//! question: two resolutions of one drain's placement are two chances to
//! disagree, and a drain that promoted three inputs could run them on three
//! different runtimes. Everything Location-scoped — projected history, model,
//! tools, permissions — is reached through the one resolved runtime.
//! The Location therefore boots when the drain starts, not when a prompt is
//! admitted. Admission never touches this seam at all: the inbox is
//! Session-scoped durable state in the one store, so admitting work into a
//! Session whose Location has never run boots nothing.
//! # A drain's boundaries are Steps, not promotions
//! [`SessionApplication::advance`] runs exactly ONE logical Step and returns.
//! A Step that ends still calling tools returns [`RunOutcome::Continue`], the
//! coordinator promotes every pending steer, and the continuation is built from
//! history reloaded after those promotions. Hiding the whole chain inside one
//! call made `Continue` dead and made a steer arriving mid-chain wait for the
//! entire tool loop to finish — the exact behaviour the delivery law forbids.
//! What a promotion does to the chain lives here rather than in the
//! coordinator:
//! - No chain in flight: the promotion's own position in projected history is
//!   verified and the Location opens a chain at that cursor.
//! - Chain in flight: the promotion is a steer joining it. Nothing is reopened,
//!   because its durable user message is already committed — the reload before
//!   the next Step is what makes it model-visible.
//!
//! Either way the promotion is admitted to the chain's current step-allowance
//! BATCH, and a batch is granted its allowance exactly once. The batch is a
//! structural boundary — it opens on admission and closes when a Step is taken
//! against it — rather than a convention resting on the reset happening to be a
//! no-op the second time.
//! # Promotion is transactional and one-at-a-time
//! Each eligibility pass promotes AT MOST ONE durable input
//! ([`Inbox::promote_next`]) inside one immediate transaction that also projects
//! the user message. A batch is several such passes at one boundary, never one
//! transaction spanning several inputs.
//! Before a chain is opened, the adapter re-reads the promotion's own position
//! in projected history. A promotion that is not there is a typed failure: a
//! Step must never be built on a transcript boundary that did not commit.
//! # One stream per physical attempt
//! This adapter does not re-prove contract rule 1 — [`StepDriver`] enforces it
//! through the sealed [`crate::AttemptPreparation`] that [`AttemptSource::prepare`]
//! returns. The adapter's obligation is to use one [`AttemptSource`] for the
//! whole chain; until a later authenticated join it has no sealed continuation,
//! so every public attempt source fails closed before any identity or stream.
//! # No legacy loop, and no implicit retry
//! An interrupt ends the chain with no further Step, and a failed Step
//! propagates rather than being retried here — the retry law lives inside
//! [`StepDriver`] and stops at its policy bound.

use std::sync::{Arc, Mutex, MutexGuard};

use grapt_artifact::ArtifactStore;
use grapt_inbox::{Inbox, InboxError, PromoteScope};
use grapt_provider::NormalizedFinish;
use grapt_session::{SessionStore, SessionStoreError};
use grapt_store::AggregateId;
use grapt_types::{
    EventId, LocationId, PendingInputId, ProviderCompletionDisposition, SessionId, StepNumber,
    StepTerminalReason,
};

use crate::coordinator::{
    Boundary, CoordinatorError, Placement, Promotion, RunOutcome, SessionApplication,
};
use crate::owner::AttemptOwner;
use crate::residency::Cancellation;
use crate::settlement::{StepSettlementContext, StepSettlementOwner};
use crate::step::{
    AttemptSource, HistorySource, StepDriver, StepPolicy, StepResult, StepTerminalHandoff,
};
use crate::supervisor::{BoxFuture, ToolRunner};
use crate::tool_protocol::{GovernedToolExecutor, GovernedToolRunner};

mod allowance;
mod run_budget;

pub use allowance::StepAllowance;
use allowance::{AllowanceExhausted, AllowanceLedger};

/// Where a promoted input sits in projected history.
///
/// Carried explicitly so the Location runtime builds its history at or after
/// the promotion, rather than being told "the latest" and hoping the write it
/// depends on has landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct HistoryCursor(u64);

impl HistoryCursor {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LocationError {
    /// The map has no runtime for this Location and could not make one. A
    /// Session whose placement cannot be served does not run.
    #[error("no Location runtime for {location}: {reason}")]
    Unavailable { location: String, reason: String },
    /// The Location is served, but this Session's Step could not be built.
    #[error("Location {location} could not open a Step for {session}: {reason}")]
    Refused {
        location: String,
        session: String,
        reason: String,
    },
}

/// One Location's execution services: everything a Step needs that is scoped
/// to where the work runs.
///
/// The associated types are concrete rather than boxed on purpose. A
/// [`ProviderReader`](crate::supervisor::ProviderReader) is created by a
/// one-shot factory and a [`HistorySource`] is `&mut`-driven across a Step;
/// erasing them would put a trait object between the driver and the two seams
/// whose exact ownership the contract cares about.
pub trait LocationRuntime: Send + Sync + 'static {
    type Attempts: AttemptSource + Send;
    type History: HistorySource + Send;
    type Tools: ToolRunner;
    type Owner: AttemptOwner + Send;

    /// A location's selected-agent cap narrows the admitted input's own bound.
    fn input_step_limit(
        &self,
        _session: &SessionId,
    ) -> Result<Option<std::num::NonZeroU32>, LocationError> {
        Ok(None)
    }

    /// Open the Step chain for a promotion that found no chain in flight.
    ///
    /// Called after the promotion is durable and after its position in
    /// projected history has been verified. The returned history must already
    /// be loaded at or after `cursor`: the first Step of this chain is built
    /// from it without a further reload, unless another input joins the chain
    /// before that Step runs.
    fn open_chain(
        &self,
        session: &SessionId,
        promotion: &Promotion,
        cursor: HistoryCursor,
    ) -> Result<StepChain<Self>, LocationError>;
}

/// The per-chain inputs the driver consumes, owned for the whole chain.
///
/// One [`AttemptSource`] serves every Step of the chain, so attempt identities
/// stay unique across retries and continuations alike.
#[derive(Debug)]
pub struct StepChain<L: LocationRuntime + ?Sized> {
    /// The first durable Step number this chain owns. A new chain for an
    /// existing Session continues after its last terminal rather than
    /// colliding with Step one from an earlier prompt.
    pub initial_step: StepNumber,
    pub attempts: L::Attempts,
    pub history: L::History,
    pub tools: Arc<L::Tools>,
    pub owner: L::Owner,
}

/// Non-test Location composition that replaces every caller-supplied tool
/// runner with the one governed, currently fail-closed protocol.
#[derive(Debug)]
pub struct GovernedLocationRuntime<L, E, S> {
    inner: Arc<L>,
    tools: Arc<GovernedToolRunner<E, S>>,
}

impl<L, E, S> LocationRuntime for GovernedLocationRuntime<L, E, S>
where
    L: LocationRuntime,
    E: GovernedToolExecutor<S>,
    S: ArtifactStore + Send + Sync + 'static,
{
    type Attempts = L::Attempts;
    type History = L::History;
    type Tools = GovernedToolRunner<E, S>;
    type Owner = L::Owner;

    fn input_step_limit(
        &self,
        session: &SessionId,
    ) -> Result<Option<std::num::NonZeroU32>, LocationError> {
        self.inner.input_step_limit(session)
    }

    fn open_chain(
        &self,
        session: &SessionId,
        promotion: &Promotion,
        cursor: HistoryCursor,
    ) -> Result<StepChain<Self>, LocationError> {
        let StepChain {
            initial_step,
            attempts,
            history,
            tools: _,
            owner,
        } = self.inner.open_chain(session, promotion, cursor)?;
        Ok(StepChain {
            initial_step,
            attempts,
            history,
            tools: Arc::clone(&self.tools),
            owner,
        })
    }
}

/// Resolves a Location to its runtime. Keyed and lazy: a Location's services
/// boot when a drain starts on it, never when a prompt is admitted.
pub trait LocationServiceMap: Send + Sync + 'static {
    type Runtime: LocationRuntime;

    fn get(&self, location: &LocationId) -> Result<Arc<Self::Runtime>, LocationError>;
}

/// Production map adapter that makes the fail-closed governed runner
/// structural for every resolved Location.
#[derive(Debug)]
pub struct GovernedLocationServiceMap<M, E, S> {
    inner: Arc<M>,
    tools: Arc<GovernedToolRunner<E, S>>,
}

impl<M, E, S> LocationServiceMap for GovernedLocationServiceMap<M, E, S>
where
    M: LocationServiceMap,
    E: GovernedToolExecutor<S>,
    S: ArtifactStore + Send + Sync + 'static,
{
    type Runtime = GovernedLocationRuntime<M::Runtime, E, S>;

    fn get(&self, location: &LocationId) -> Result<Arc<Self::Runtime>, LocationError> {
        Ok(Arc::new(GovernedLocationRuntime {
            inner: self.inner.get(location)?,
            tools: Arc::clone(&self.tools),
        }))
    }
}

/// Mints durable event identities. Infallible by signature: a promotion
/// boundary must never end part-complete for want of an id.
pub trait EventIds: std::fmt::Debug + Send + Sync + 'static {
    fn mint(&self) -> EventId;
}

/// Wall-clock milliseconds for durable `created` stamps.
pub trait Clock: std::fmt::Debug + Send + Sync + 'static {
    fn now_ms(&self) -> i64;
}

/// How far a chain has got. Diagnostics only: a drain never exposes the
/// provider-side state of a chain in flight, just its progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChainProgress {
    next_step: u32,
    steps_run: u32,
    spent: u32,
    allowance: Option<u32>,
    /// How many batches this chain has granted an allowance to — one per user
    /// turn it has answered.
    grants: u32,
}

/// One chain of Steps answering the inputs promoted into it.
struct ActiveChain<L: LocationRuntime + ?Sized> {
    attempts: L::Attempts,
    history: L::History,
    tools: Arc<L::Tools>,
    /// Moved into the driver for the duration of a Step and returned. It is
    /// `None` only between those two points, which nothing else can observe;
    /// finding it absent is a typed failure rather than a panic.
    owner: Option<L::Owner>,
    /// 1-based number of the NEXT Step in this chain.
    next_step: u32,
    /// Steps already run. A chain that has run one owes a reload before the
    /// next.
    steps_run: u32,
    /// A user input joined after history was loaded, so the next Step must
    /// reload even if none has run yet. Without this a steer promoted into a
    /// brand-new chain's batch would be invisible to that chain's first Step.
    joined_since_load: bool,
    /// None: no step allowance, so the chain runs until the model stops
    /// calling tools, the user interrupts, or a Step fails.
    allowance: Option<AllowanceLedger>,
    run_budget: Option<run_budget::RunBudget>,
}

impl<L: LocationRuntime> ActiveChain<L> {
    fn open(
        chain: StepChain<L>,
        allowance: Option<StepAllowance>,
        maximum: Option<std::num::NonZeroU32>,
    ) -> Self {
        let next_step = initial_step_value(chain.initial_step);
        Self {
            attempts: chain.attempts,
            history: chain.history,
            tools: chain.tools,
            owner: Some(chain.owner),
            next_step,
            steps_run: 0,
            joined_since_load: false,
            allowance: allowance.map(AllowanceLedger::new),
            run_budget: maximum.map(run_budget::RunBudget::new),
        }
    }

    fn progress(&self) -> ChainProgress {
        ChainProgress {
            next_step: self.next_step,
            steps_run: self.steps_run,
            spent: self.allowance.as_ref().map_or(0, |ledger| ledger.spent),
            allowance: self
                .allowance
                .as_ref()
                .map(|ledger| ledger.allowance.value()),
            grants: self.allowance.as_ref().map_or(0, |ledger| ledger.grants),
        }
    }
}

fn initial_step_value(step: StepNumber) -> u32 {
    step.value()
}

/// One drain's owned context: its placement, the Location runtime serving that
/// placement, and the Step chain in flight.
///
/// Constructed only by [`RuntimeApplication::begin_drain`] and never cloned:
/// one drain, one Location, one chain over the Session's transcript.
pub struct RuntimeDrain<L: LocationRuntime> {
    session: SessionId,
    placement: Placement,
    runtime: Arc<L>,
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<dyn EventIds>,
    clock: Arc<dyn Clock>,
    chain: Option<ActiveChain<L>>,
}

/// Written by hand rather than derived: the Location runtime and a chain's
/// provider-side state are not diagnostics, and requiring them to be `Debug`
/// would put that bound on every Location implementation.
impl<L: LocationRuntime> std::fmt::Debug for RuntimeDrain<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeDrain")
            .field("session", &self.session)
            .field("placement", &self.placement)
            .field("chain", &self.chain.as_ref().map(ActiveChain::progress))
            .finish()
    }
}

impl<L: LocationRuntime> RuntimeDrain<L> {
    /// The placement this drain resolved. Read-only: there is no seam here for
    /// resolving a second one.
    pub fn placement(&self) -> &Placement {
        &self.placement
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    fn run_failure(&self, reason: String) -> CoordinatorError {
        CoordinatorError::Run {
            session: self.session.as_str().to_owned(),
            reason,
        }
    }

    /// Run one logical Step, then decide whether the chain survives it.
    ///
    /// Only [`RunOutcome::Continue`] keeps a chain: a terminal, an interrupt
    /// and a failure all end it, which is what stops the coordinator offering a
    /// Step for a chain that is over.
    async fn step(
        &mut self,
        policy: StepPolicy,
        cancellation: Cancellation,
    ) -> Result<RunOutcome, CoordinatorError> {
        let cleanup_deadline = policy.no_progress_timeout();
        let outcome = self.run_step(policy, cancellation).await;
        if !matches!(outcome, Ok(RunOutcome::Continue)) {
            let closed = match self.chain.as_mut() {
                Some(chain) => {
                    tokio::time::timeout(cleanup_deadline, chain.attempts.shutdown()).await
                }
                None => Ok(Ok(())),
            };
            self.chain = None;
            match closed {
                Ok(Ok(())) => {}
                Ok(Err(reason)) => return Err(self.run_failure(reason)),
                Err(_) => {
                    return Err(self.run_failure("context owner shutdown deadline exceeded".into()))
                }
            }
        }
        outcome
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn run_step(
        &mut self,
        policy: StepPolicy,
        cancellation: Cancellation,
    ) -> Result<RunOutcome, CoordinatorError> {
        let mut cancel = cancellation.signal();
        let session = self.session.clone();
        let inbox = Arc::clone(&self.inbox);
        let ids = Arc::clone(&self.ids);
        let clock = Arc::clone(&self.clock);
        let failure = |reason: String| CoordinatorError::Run {
            session: session.as_str().to_owned(),
            reason,
        };
        let Some(chain) = self.chain.as_mut() else {
            return Err(failure(
                "a Step was asked for while no chain was in flight".to_owned(),
            ));
        };
        let index = chain.next_step;

        if chain.steps_run > 0 {
            // An interrupt that lands between Steps must not buy another one.
            // Checked before the reload as well as before the Step, so a
            // cancelled chain does no further store work either.
            if cancel.borrow_and_update().is_some() {
                return Ok(RunOutcome::Interrupted);
            }
        }

        // Reload projected history BEFORE the durable continuation: the
        // previous Step's tool terminals and any input promoted since this
        // chain loaded are model-visible facts, and a Step built from history
        // that predates them would ask the model to answer with them hidden.
        if chain.steps_run > 0 || chain.joined_since_load {
            let previous = chain.history.generation();
            let reloaded = chain.history.reload().map_err(|error| {
                failure(format!(
                    "history reload before step {index} failed: {}",
                    error.reason
                ))
            })?;
            // A generation that stood still means the reload did not deliver
            // the facts this Step depends on. Continuing anyway would re-ask
            // the model with the same history and could loop.
            if reloaded <= previous {
                return Err(failure(format!(
                    "history reload before step {index} returned generation {} which does not advance past {}",
                    reloaded.value(),
                    previous.value()
                )));
            }
            chain.joined_since_load = false;
        }

        // Only opt-in caps stop a chain: an input's or agent's step maximum,
        // and an allowance a caller composed (spent per Step, reset per input).
        if let Some(budget) = chain.run_budget.as_mut() {
            budget.spend(&session)?;
        }
        if let Some(ledger) = chain.allowance.as_mut() {
            ledger
                .take()
                .map_err(|AllowanceExhausted| CoordinatorError::StepBudgetExhausted {
                    session: session.clone(),
                    policy: ledger.allowance.policy().clone(),
                    spent: ledger.spent,
                    granted: ledger.allowance.value(),
                })?;
        }

        let step =
            StepNumber::new(index).ok_or_else(|| failure("step numbers are 1-based".to_owned()))?;
        let aggregate = AggregateId::Session(session.clone());
        let writer = {
            let inbox = lock_shared_inbox(&inbox);
            inbox.store().fencing_token(&aggregate).map_err(|error| {
                failure(format!(
                    "step {index} has no already-elected Session writer: {error}"
                ))
            })?
        };
        // These are Step coordinates, not attempt or terminal coordinates.
        // Capture each authority exactly once after the Step is admitted and
        // before `StepDriver` can open its first provider reader. The terminal
        // identity is minted into locals FIRST because it has two consumers
        // that must agree byte-for-byte: the settlement context that commits
        // the terminal, and the attempt source whose durable receipts persist
        // it at reserve.
        let terminal_event = ids.mint();
        let terminal_created_ms = clock.now_ms();
        let settlement = StepSettlementContext::new(
            session.clone(),
            step,
            terminal_event.clone(),
            terminal_created_ms,
            writer,
            Arc::clone(&ids),
            Arc::clone(&clock),
        );
        // Handed over BEFORE the driver runs, so the source can fail closed on
        // a prepare whose Step it was never told about.
        chain.attempts.begin_step(StepTerminalHandoff::new(
            step,
            terminal_event,
            terminal_created_ms,
        ));
        let mut owner = chain
            .owner
            .take()
            .ok_or_else(|| failure(format!("step {index} has no attempt owner")))?;
        owner
            .begin_step(step)
            .map_err(|error| failure(format!("step {index} owner preparation failed: {error}")))?;

        let owner = StepSettlementOwner::new(owner, inbox, settlement)
            .with_receipts(chain.attempts.receipt_settlement());
        let (returned, report) = StepDriver::run(
            step,
            policy,
            &mut chain.attempts,
            &mut chain.history,
            Arc::clone(&chain.tools),
            owner,
            cancellation,
        )
        .await;
        chain.owner = Some(returned.into_inner());
        chain.steps_run += 1;
        chain.next_step = index.saturating_add(1);
        chain.attempts.output_continuation(false);

        match report.result {
            // The provider's finish reason is normalized into the typed
            // disposition BEFORE it routes anything. The old `finish ==
            // ToolCalls` test made every other finish — including a truncated
            // stream — fall into the same `Idle` bucket as a clean completion.
            StepResult::Ended { finish } => match finish.normalize() {
                NormalizedFinish::Completed {
                    completion: ProviderCompletionDisposition::AssertedContinue,
                } => {
                    // The model asked for tools and the supervisor settled
                    // them. Feeding the results back is a new Step, taken after
                    // the coordinator has had its boundary.
                    Ok(RunOutcome::Continue)
                }
                // The model asserted it was done. That ends the owned Step
                // chain and nothing more: it does not settle an objective.
                NormalizedFinish::Completed {
                    completion: ProviderCompletionDisposition::AssertedComplete,
                } => Ok(RunOutcome::Idle),
                // The provider said nothing about why it stopped. The fallback
                // is "no unsettled work", which is the same disposition-free
                // reading the code had before typing — kept deliberately.
                NormalizedFinish::Completed {
                    completion: ProviderCompletionDisposition::Unstated,
                } => Ok(RunOutcome::Idle),
                // Retain the partial answer, then pass through the coordinator's
                // ordinary boundary before another Step. Each physical attempt,
                // including retries and compaction, keeps its existing receipts.
                NormalizedFinish::Completed {
                    completion: ProviderCompletionDisposition::Truncated,
                } => {
                    if !chain.attempts.output_continuation(true) {
                        return Err(failure(format!(
                            "step {index} was truncated at the provider output ceiling; \
                             this source cannot retain and continue the partial answer"
                        )));
                    }
                    Ok(RunOutcome::Continue)
                }
                // A finish that is failure-shaped rather than a disposition.
                NormalizedFinish::Failed { class } => Err(failure(format!(
                    "step {index} finished with a provider failure: {class:?}"
                ))),
            },
            // The user stopped the chain: no retry, no further promotion, and
            // not an error — the Step terminal is already durable. This is a
            // typed match on the reason the driver wrote, so a watchdog stall
            // (`ProviderFailure { NoProgressTimeout }`) can no longer reach it,
            // which is exactly what the `aborted` string comparison allowed.
            StepResult::Failed { failure: step }
                if matches!(step.reason(), StepTerminalReason::Interrupted { .. }) =>
            {
                Ok(RunOutcome::Interrupted)
            }
            StepResult::Failed { failure: step } => {
                let reason = format!(
                    "step {index} failed: {:?}: {}",
                    step.reason(),
                    step.detail().as_str()
                );
                Err(match step.reason() {
                    StepTerminalReason::ProviderFailure {
                        class: grapt_types::ProviderFailureClass::Quota,
                    } => CoordinatorError::ProviderLimit {
                        session: session.clone(),
                        reset: match step.diagnostic() {
                            Some(grapt_types::failure_diagnostic::StepFailureDiagnostic::ProviderLimit { reset }) => reset,
                            _ => None,
                        },
                    },
                    // The detail names the status, the code and a redacted
                    // excerpt, so it is quoted rather than flattened into an
                    // opaque reason. Everything else keeps its existing path.
                    StepTerminalReason::ProviderFailure { .. } => {
                        CoordinatorError::ProviderFailed {
                            session: session.clone(),
                            summary: step.detail().as_str().to_owned(),
                        }
                    }
                    StepTerminalReason::ContextFailure { failure: context } => {
                        CoordinatorError::Context {
                            session: session.clone(),
                            failure: *context,
                            continuity: None,
                            reason,
                        }
                    }
                    _ => failure(reason),
                })
            }
            // The store refused a fact. Nothing downstream may proceed on
            // unwritten state, and it must not be relabelled as a provider
            // failure.
            StepResult::OwnerRejected { error } => Err(failure(format!(
                "step {index} owner refused: {}",
                error.reason
            ))),
            StepResult::AdmissionRefused {
                refusal,
                failure: step,
            } => {
                if matches!(&refusal, crate::AttemptAdmissionRefusal::Saturated { .. }) {
                    return Err(CoordinatorError::ProviderCapacity {
                        session: session.clone(),
                    });
                }
                let reason = format!("step {index} attempt admission refused: {refusal}");
                Err(match step.reason() {
                    StepTerminalReason::ContextFailure { failure: context } => {
                        CoordinatorError::Context {
                            session: session.clone(),
                            failure: *context,
                            continuity: match &refusal {
                                crate::AttemptAdmissionRefusal::ContextUnavailable {
                                    continuity,
                                    ..
                                } => *continuity,
                                _ => None,
                            },
                            reason,
                        }
                    }
                    _ => failure(reason),
                })
            }
        }
    }
}

/// The production application seam.
///
/// Holds the store behind a synchronous mutex. Every guard below is taken and
/// dropped inside one synchronous function — none survives an `.await`, which
/// is the sync/async constitution's rule, made structural by using
/// [`std::sync::Mutex`] rather than an async one.
#[derive(Debug)]
pub struct RuntimeApplication<M: LocationServiceMap> {
    inbox: Arc<Mutex<Inbox>>,
    locations: Arc<M>,
    ids: Arc<dyn EventIds>,
    clock: Arc<dyn Clock>,
    policy: StepPolicy,
    allowance: Option<StepAllowance>,
}

impl<M: LocationServiceMap> RuntimeApplication<M> {
    pub(crate) fn new_internal(
        inbox: Arc<Mutex<Inbox>>,
        locations: Arc<M>,
        ids: Arc<dyn EventIds>,
        clock: Arc<dyn Clock>,
        policy: StepPolicy,
        allowance: Option<StepAllowance>,
    ) -> Self {
        Self {
            inbox,
            locations,
            ids,
            clock,
            policy,
            allowance,
        }
    }

    /// Compose a Location whose [`LocationRuntime::Tools`] is already adapted
    /// to the product's external authority owner.
    ///
    /// Runtime remains authority-neutral: this constructor does not receive a
    /// lease, permission decision, or issuer. The supplied Location must keep
    /// its concrete effect invocation inside that owner's consume-and-run
    /// closure; [`Self::new_governed`] remains the fail-closed fallback.
    pub fn new_authority_adapted(
        inbox: Arc<Mutex<Inbox>>,
        locations: Arc<M>,
        ids: Arc<dyn EventIds>,
        clock: Arc<dyn Clock>,
        execution: (StepPolicy, Option<StepAllowance>),
    ) -> Self {
        let (policy, allowance) = execution;
        Self::new_internal(inbox, locations, ids, clock, policy, allowance)
    }

    /// A poisoned store mutex means a panic inside a store operation. Recover
    /// the guard rather than propagating the panic: one Session's failure must
    /// not wedge every other Session in the process.
    fn lock_inbox(&self) -> MutexGuard<'_, Inbox> {
        lock_shared_inbox(&self.inbox)
    }

    /// The Session's placement, from its durable seed.
    ///
    /// Placement is a fact about the Session, so it is read fresh at every
    /// drain start rather than cached across drains. No durable fact in the
    /// accepted evidence moves a Session after creation (see the scope note in
    /// this crate's witnesses), but a drain that re-read the seed is the seam
    /// where such a fact would take effect — one that cached it would not.
    fn discover_placement(&self, session: &SessionId) -> Result<Placement, CoordinatorError> {
        let reference = {
            let inbox = self.lock_inbox();
            SessionStore::over(inbox.store()).placement(session)
        };
        match reference {
            Ok(reference) => Ok(Placement {
                location: reference.canonical_id(),
            }),
            // The seed vanished between the existence check and here. That is
            // a truthful unknown Session, not a placement fault.
            Err(SessionStoreError::Unknown { session }) => {
                Err(CoordinatorError::UnknownSession { session })
            }
            Err(error @ SessionStoreError::Corrupt { .. }) => Err(CoordinatorError::Placement {
                session: session.as_str().to_owned(),
                reason: error.to_string(),
            }),
            Err(error @ SessionStoreError::Store { .. }) => Err(store_error(session, error)),
        }
    }

    /// Authorize and promote under the same Inbox lock. A false predicate or
    /// error leaves every pending input untouched. The caller owns the policy;
    /// runtime neither retains the predicate nor grants it any authority.
    pub fn next_eligible_if(
        &self,
        drain: &mut RuntimeDrain<M::Runtime>,
        boundary: Boundary,
        authorize: impl FnOnce(&Inbox) -> Result<bool, CoordinatorError>,
    ) -> Result<Option<Promotion>, CoordinatorError> {
        let scope = match boundary {
            Boundary::Steer => PromoteScope::Steer,
            Boundary::Idle => PromoteScope::InputAtIdle,
        };
        let created_ms = self.clock.now_ms();
        let ids = Arc::clone(&self.ids);
        let mut mint = move || ids.mint();
        let promoted = {
            let mut inbox = self.lock_inbox();
            if !authorize(&inbox)? {
                return Ok(None);
            }
            inbox
                .promote_next(&drain.session, scope, &mut mint, created_ms)
                .map_err(|error| CoordinatorError::Inbox {
                    session: drain.session.as_str().to_owned(),
                    reason: error.to_string(),
                })?
        };
        let Some(promoted) = promoted else {
            return Ok(None);
        };
        let promotion = Promotion {
            input: PendingInputId::new(promoted.input),
            delivery: promoted.delivery,
        };
        self.join(drain, &promotion)?;
        Ok(Some(promotion))
    }

    /// Attach a durable promotion to the drain's chain.
    ///
    /// A promotion either opens the chain or joins the one in flight. Opening
    /// grants the first batch its step allowance; joining is admitted to the
    /// batch already in progress, which has been granted its own.
    fn join(
        &self,
        drain: &mut RuntimeDrain<M::Runtime>,
        promotion: &Promotion,
    ) -> Result<(), CoordinatorError> {
        let session = drain.session.as_str().to_owned();
        let (cursor, maximum) = {
            let inbox = self.lock_inbox();
            let input = inbox
                .promoted_input(&drain.session, promotion.input.message())
                .map_err(|error| run_error(&drain.session, error))?;
            let maximum = grapt_inbox::admission::options::max_steps(input.metadata.as_ref())
                .map_err(|error| drain.run_failure(error.into()))?;
            (input.seq, maximum)
        };
        let agent_maximum = drain
            .runtime
            .input_step_limit(&drain.session)
            .map_err(|error| drain.run_failure(error.to_string()))?;
        let maximum = match (maximum, agent_maximum) {
            (Some(input), Some(agent)) => Some(input.min(agent)),
            (input, agent) => input.or(agent),
        };
        if let Some(chain) = drain.chain.as_mut() {
            if let Some(maximum) = maximum {
                match chain.run_budget.as_mut() {
                    Some(budget) => budget.constrain(maximum),
                    None => chain.run_budget = Some(run_budget::RunBudget::new(maximum)),
                }
            }
            // A steer joining a chain in flight rebinds the chain's context
            // coordinate to it. Nothing is reopened: its user message is
            // already durable; the rebind plus the reload before the next
            // Step make it the current input of that Step's request, with
            // its own rows exact after it.
            chain
                .attempts
                .promote_input(promotion.input.message())
                .and_then(|()| chain.history.promote_input(promotion.input.message()))
                .map_err(|reason| CoordinatorError::Run { session, reason })?;
            if let Some(ledger) = chain.allowance.as_mut() {
                ledger.admit_input();
            }
            // A new human steer supersedes the old answer-continuation cue.
            chain.attempts.output_continuation(false);
            chain.joined_since_load = true;
            return Ok(());
        }

        let opened = drain
            .runtime
            .open_chain(&drain.session, promotion, HistoryCursor::new(cursor))
            .map_err(|error| drain.run_failure(error.to_string()))?;
        drain.chain = Some(ActiveChain::open(opened, self.allowance.clone(), maximum));
        Ok(())
    }
}

impl<M, E, S> RuntimeApplication<GovernedLocationServiceMap<M, E, S>>
where
    M: LocationServiceMap,
    E: GovernedToolExecutor<S>,
    S: ArtifactStore + Send + Sync + 'static,
{
    /// The sole public production constructor. The wrapped Location may supply
    /// attempts/history/ownership, but its tool runner is discarded and cannot
    /// bypass the governed no-issuer boundary.
    pub fn new_governed(
        inbox: Arc<Mutex<Inbox>>,
        locations: Arc<M>,
        executor: Arc<E>,
        artifacts: Arc<S>,
        ids: Arc<dyn EventIds>,
        clock: Arc<dyn Clock>,
        execution: (StepPolicy, Option<StepAllowance>),
    ) -> Self {
        let (policy, allowance) = execution;
        let tools = Arc::new(GovernedToolRunner::new(executor, artifacts));
        Self::new_internal(
            inbox,
            Arc::new(GovernedLocationServiceMap {
                inner: locations,
                tools,
            }),
            ids,
            clock,
            policy,
            allowance,
        )
    }
}

fn run_error(session: &SessionId, error: InboxError) -> CoordinatorError {
    CoordinatorError::Run {
        session: session.as_str().to_owned(),
        reason: error.to_string(),
    }
}

fn store_error(session: &SessionId, error: SessionStoreError) -> CoordinatorError {
    // `Inbox` is the coordinator's store-read failure variant: a read that
    // failed is a store error and must never be reported as an unknown
    // Session, which would tell a caller their Session does not exist because
    // a disk hiccuped.
    CoordinatorError::Inbox {
        session: session.as_str().to_owned(),
        reason: error.to_string(),
    }
}

impl<M: LocationServiceMap> SessionApplication for RuntimeApplication<M> {
    type Drain = RuntimeDrain<M::Runtime>;

    fn session_exists(&self, session: &SessionId) -> Result<bool, CoordinatorError> {
        let inbox = self.lock_inbox();
        SessionStore::over(inbox.store())
            .exists(session)
            .map_err(|error| store_error(session, error))
    }

    fn begin_drain(&self, session: &SessionId) -> Result<Self::Drain, CoordinatorError> {
        let placement = self.discover_placement(session)?;
        // The ONE resolution. Everything Location-scoped for this drain is
        // reached through the runtime it returns.
        let runtime = self.locations.get(&placement.location).map_err(|error| {
            CoordinatorError::Placement {
                session: session.as_str().to_owned(),
                reason: error.to_string(),
            }
        })?;
        Ok(RuntimeDrain {
            session: session.clone(),
            placement,
            runtime,
            inbox: Arc::clone(&self.inbox),
            ids: Arc::clone(&self.ids),
            clock: Arc::clone(&self.clock),
            chain: None,
        })
    }

    fn continuation_owed(&self, drain: &Self::Drain) -> bool {
        drain.chain.is_some()
    }

    fn next_eligible(
        &self,
        drain: &mut Self::Drain,
        boundary: Boundary,
    ) -> Result<Option<Promotion>, CoordinatorError> {
        self.next_eligible_if(drain, boundary, |_| Ok(true))
    }

    fn advance<'a>(
        &'a self,
        drain: &'a mut Self::Drain,
        cancellation: Cancellation,
    ) -> BoxFuture<'a, Result<RunOutcome, CoordinatorError>> {
        let policy = self.policy.clone();
        Box::pin(async move { drain.step(policy, cancellation).await })
    }
}

fn lock_shared_inbox(inbox: &Mutex<Inbox>) -> MutexGuard<'_, Inbox> {
    match inbox.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests;
