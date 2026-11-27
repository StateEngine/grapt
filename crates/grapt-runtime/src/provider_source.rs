//! The production [`AttemptSource`]: the runtime consumer of the provider's
//! public [`AttemptOpener`].
//!
//! Release `IA0-U2-AUTHORING-R1`: "Runtime consumes it as the production
//! AttemptSource, reserves the existing attempt receipt before open, holds exact
//! custody through every clean/error/cancel durable terminal, settles before
//! release, and publishes only after commit."
//!
//! [`ProviderAttemptSource::prepare`] is synchronous and runs, in order:
//! handoff check, plan, mint, open, reserve + permit, custody into the ledger.
//! Each step carries its own reason at its own arm below; why the ORDER is that
//! order is recorded in this module's introducing commit.
//!
//! It opens no socket, writes no Step terminal (settlement owns both), and
//! never notifies before a commit (the owner seam refuses that order).

use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_inbox::Inbox;
use grapt_provider::{
    AdapterState, AttemptCustody, AttemptFragment, AttemptOpener, AttemptReader,
    ProviderFailureClass,
};
use grapt_store::{
    AggregateId, AttemptRecordCeiling, EventStore, GovernedTransaction, ReserveInput,
    TerminalCoordinates, WriteError,
};
use grapt_types::{
    AttemptOrdinal, BoundedAttemptId, BoundedSessionId, DurableAttemptReceipt, IdAuthority,
    PhysicalAttemptId, RequestSentMarker, ReservedMarker, SessionId, SettledMarker, StepNumber,
    Timestamp,
};

use crate::application::Clock;
use crate::attempt_admission::{
    AttemptAdmissionRefusal, AttemptPreparation, ReservedAttemptAdmission, UnopenedAttempt,
};
use crate::attempt_planning::capacity;
use crate::step::{AttemptSource, HistoryGeneration, StepTerminalHandoff};
use crate::supervisor::ProviderReader;

mod compaction;
mod pending_plan;
mod request_cache;
pub use crate::attempt_planning::{
    AttemptPlan, ContextAdmissionRefusal, ContextPreparationFailure, PlanRefusal, PlanRefusalCause,
    PlannedAttempt,
};
use compaction::CompactionBinding;
pub use compaction::{ProviderCompactionCall, SummaryRefusal, MAX_SUMMARY_CALLS};

/// One reserved attempt's custody, held from reserve until its receipt settles.
struct CustodyEntry {
    attempt: PhysicalAttemptId,
    custody: AttemptCustody,
}

/// The custody ledger the source fills at reserve and the settlement owner
/// drains at the Step-terminal commit, pairing "receipt reserved" with "custody
/// held": an entry exists exactly while a durable receipt awaits its terminal,
/// and its custody is released only after the settling commit.
#[derive(Clone)]
pub struct AttemptCustodyLedger {
    entries: Arc<Mutex<Vec<CustodyEntry>>>,
}

impl core::fmt::Debug for AttemptCustodyLedger {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let held = match self.entries.lock() {
            Ok(entries) => entries.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        };
        f.debug_struct("AttemptCustodyLedger")
            .field("held", &held)
            .finish()
    }
}

impl AttemptCustodyLedger {
    fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<CustodyEntry>> {
        match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn hold(&self, attempt: PhysicalAttemptId, custody: AttemptCustody) {
        self.lock().push(CustodyEntry { attempt, custody });
    }

    /// How many custodies are currently held. Witness surface.
    pub fn held(&self) -> usize {
        self.lock().len()
    }
}

/// The settlement owner's half of the ledger: settle every held receipt inside
/// the Step-terminal transaction, then release custody after that commit.
#[derive(Debug, Clone)]
pub struct ProviderReceiptSettlement {
    ledger: AttemptCustodyLedger,
    ceiling: AttemptRecordCeiling,
}

impl ProviderReceiptSettlement {
    /// Settle every held attempt's durable receipt INSIDE `tx`, the transaction
    /// carrying the Step terminal append. Returns the attempts settled, which
    /// the caller reports to [`Self::release`] only after the commit. Custody
    /// stays in the ledger across this call on purpose: if the commit fails the
    /// receipts are unsettled and the custody is still held, which is true.
    pub(crate) fn settle_within(
        &self,
        tx: &GovernedTransaction<'_>,
        writer: grapt_store::FencedWriter,
        settled_at_ms: i64,
    ) -> Result<Vec<PhysicalAttemptId>, WriteError> {
        let _settle = tx.timing_span(grapt_types::timing::TimingStage::ReceiptSettle);
        let entries = self.ledger.lock();
        let mut settled = Vec::with_capacity(entries.len());
        for entry in entries.iter() {
            // Custody must cover the attempt whose receipt is settled: the
            // pairing made at reserve is re-asserted here, so a ledger holding
            // a foreign custody refuses rather than settling against it.
            debug_assert_eq!(entry.custody.attempt(), &entry.attempt);
            EventStore::settle_attempt_ordinary_terminal_within(
                tx,
                &entry.attempt,
                writer,
                self.ceiling,
                SettledMarker::new(Timestamp::from_millis(settled_at_ms)),
            )?;
            settled.push(entry.attempt.clone());
        }
        Ok(settled)
    }

    /// Drop the custody of every settled attempt. Called only after the
    /// settling transaction committed — this is "settles before release".
    pub(crate) fn release(&self, settled: &[PhysicalAttemptId]) {
        let mut entries = self.ledger.lock();
        entries.retain(|entry| !settled.contains(&entry.attempt));
    }
}

/// Product-owned observer of one volatile 4xx detail, before its failure is forwarded.
/// The reader awaits it without holding any store or Session lock.
pub trait ProviderErrorObserver: Send + Sync {
    fn observe(
        &self,
        attempt: PhysicalAttemptId,
        detail: grapt_provider::ProviderErrorDetail,
    ) -> crate::BoxFuture<'static, ()>;
}

/// The production attempt source. See the module note for the prepare order.
#[allow(
    missing_debug_implementations,
    reason = "it owns an AttemptOpener, which owns credential material"
)]
pub struct ProviderAttemptSource<P: AttemptPlan> {
    opener: Arc<AttemptOpener>,
    error_observer: Option<Arc<dyn ProviderErrorObserver>>,
    capacity_observer: Option<Arc<dyn Fn(Option<usize>) + Send + Sync>>,
    capacity_error: Option<AttemptAdmissionRefusal>,
    capacity_progress: tokio::sync::watch::Sender<()>,
    child_capacity: Option<std::num::NonZeroUsize>,
    inbox: Arc<Mutex<Inbox>>,
    ids: Arc<dyn IdAuthority + Send + Sync>,
    clock: Arc<dyn Clock>,
    session: SessionId,
    ceiling: AttemptRecordCeiling,
    planner: P,
    plan_state: pending_plan::PlanState,
    ledger: AttemptCustodyLedger,
    terminal: Option<StepTerminalHandoff>,
    compaction: Arc<Mutex<CompactionBinding>>,
    /// The place [`AttemptSource::reserve_capacity`] holds for the next attempt.
    capacity: grapt_provider::AttemptCapacity,
}

impl<P: AttemptPlan> ProviderAttemptSource<P> {
    pub fn new(
        opener: Arc<AttemptOpener>,
        inbox: Arc<Mutex<Inbox>>,
        ids: Arc<dyn IdAuthority + Send + Sync>,
        clock: Arc<dyn Clock>,
        session: SessionId,
        ceiling: AttemptRecordCeiling,
        planner: P,
    ) -> Self {
        Self {
            opener,
            error_observer: None,
            capacity_observer: None,
            capacity_error: None,
            child_capacity: None,
            capacity_progress: tokio::sync::watch::channel(()).0,
            inbox,
            ids,
            clock,
            session,
            ceiling,
            planner,
            plan_state: pending_plan::PlanState::default(),
            ledger: AttemptCustodyLedger::new(),
            terminal: None,
            compaction: Arc::new(Mutex::new(CompactionBinding::default())),
            capacity: grapt_provider::AttemptCapacity::default(),
        }
    }

    /// Install the product observer on this source's exact physical attempts.
    pub fn with_error_observer(mut self, observer: Arc<dyn ProviderErrorObserver>) -> Self {
        self.error_observer = Some(observer);
        self
    }

    /// Observe actual capacity waits, including their cancellation, without
    /// changing admission policy or minting an attempt.
    pub fn with_capacity_observer(
        mut self,
        observer: Arc<dyn Fn(Option<usize>) + Send + Sync>,
    ) -> Self {
        self.capacity_observer = Some(observer);
        self
    }

    /// Use the shared child pool; the root pool remains independent.
    pub fn with_child_capacity(mut self, capacity: std::num::NonZeroUsize) -> Self {
        self.child_capacity = Some(capacity);
        self
    }

    /// A summary-call port sharing this source's admission and receipt custody.
    /// Every port of this source shares one binding: one summary call, and one
    /// fallback after it ended uncommitted, per compaction. It cannot mint a
    /// terminal.
    pub fn compaction_call(&self) -> ProviderCompactionCall {
        ProviderCompactionCall {
            opener: self.opener.clone(),
            error_observer: self.error_observer.clone(),
            capacity_observer: self.capacity_observer.clone(),
            child_capacity: self.child_capacity,
            inbox: self.inbox.clone(),
            ids: self.ids.clone(),
            clock: self.clock.clone(),
            session: self.session.clone(),
            ceiling: self.ceiling,
            ledger: self.ledger.clone(),
            binding: self.compaction.clone(),
        }
    }

    /// The ledger, for witnesses that assert custody is held and released at
    /// the right moments.
    pub fn ledger(&self) -> &AttemptCustodyLedger {
        &self.ledger
    }

    fn lock_inbox(&self) -> MutexGuard<'_, Inbox> {
        match self.inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Reserve the durable receipt and commit the send permit ATOMICALLY under
    /// the already-elected writer; the receipt names the handoff's exact
    /// terminal coordinates, which is what the handoff exists to carry.
    /// All-or-nothing is the load-bearing half — either a receipt reserved AND
    /// permitted, or nothing durable, with no third state to inherit.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn reserve_and_permit(
        &self,
        attempt: &PhysicalAttemptId,
        step: StepNumber,
        ordinal: u32,
        terminal: &TerminalCoordinates,
        context: (
            Option<&grapt_types::RequestContextIdentity>,
            Option<grapt_types::request_cache::RequestCacheFingerprint>,
        ),
        mail: (
            Option<&crate::mailbox::remote::NativeMailBatch>,
            Option<&grapt_inbox::operation_mail::OperationMailBatch>,
        ),
    ) -> Result<(), String> {
        let Some(maximum) = NonZeroU32::new(self.ceiling.get()) else {
            return Err("the attempt record ceiling is zero".to_owned());
        };
        let Some(ordinal) = AttemptOrdinal::new(ordinal) else {
            return Err("attempt ordinals are 1-based".to_owned());
        };
        let bounded_attempt = BoundedAttemptId::new(attempt.clone(), maximum)
            .map_err(|error| format!("attempt identity refuses the ceiling: {error:?}"))?;
        let bounded_session = BoundedSessionId::new(self.session.clone(), maximum)
            .map_err(|error| format!("session identity refuses the ceiling: {error:?}"))?;
        let reserved = ReservedMarker::new(Timestamp::from_millis(self.clock.now_ms()));
        let receipt = DurableAttemptReceipt::reserved(
            bounded_attempt,
            bounded_session,
            step,
            ordinal,
            reserved,
        );

        let mut inbox = self.lock_inbox();
        let store = inbox.store_mut();
        let aggregate = AggregateId::Session(self.session.clone());
        let writer = store
            .fencing_token(&aggregate)
            .map_err(|error| format!("no elected Session writer: {error}"))?;
        let permit = RequestSentMarker::new(Timestamp::from_millis(self.clock.now_ms()));
        // ONE caller-owned transaction carries BOTH writes, so a permit refusal
        // rolls the reservation back with it. Split across two transactions the
        // reservation is already durable when the permit refuses: prepare then
        // returns `StoreRefused`, custody is never held (that is step 6 below),
        // and the settlement owner settles LEDGER entries only — so the Step's
        // admission-refused terminal would commit over a receipt nothing can
        // ever resolve. Dropping this transaction on the `?` rolls it back.
        let tx = store
            .governed_transaction()
            .map_err(|error| format!("attempt transaction refused: {error}"))?;
        request_cache::record(
            &tx,
            &aggregate,
            writer,
            self.ids.as_ref(),
            self.clock.as_ref(),
            grapt_event::request_cache::RequestCacheFact {
                attempt: attempt.clone(),
                step,
                fingerprint: context.1,
            },
        )?;
        if let Some(context) = context.0 {
            let current = grapt_store::compaction::latest_descriptor(&tx, &self.session)
                .map_err(|error| error.to_string())?
                .ok_or("request context has no committed descriptor")?;
            if current.0 != context.descriptor
                || current.2.target_window() != &context.window
                || current.2.target_epoch() != context.epoch
            {
                return Err("request context generation changed before admission".to_owned());
            }
            let event = format!("evt_{}", self.ids.mint().as_str())
                .parse::<grapt_types::EventId>()
                .map_err(|_| "request context event identity invalid")?;
            let data = grapt_event::request_context::encode(context, attempt, step);
            EventStore::append_within(
                &tx,
                grapt_store::AppendInput {
                    id: &event,
                    aggregate: aggregate.clone(),
                    writer,
                    kind_name: "session.request.context",
                    version: grapt_types::EventVersion::new(1).ok_or("event version")?,
                    created_ms: self.clock.now_ms(),
                    data: &data,
                },
                &mut [],
            )
            .map_err(|error| error.to_string())?;
        }
        EventStore::reserve_attempt_receipt_within(
            &tx,
            &ReserveInput {
                receipt: &receipt,
                writer,
                terminal,
                ceiling: self.ceiling,
            },
        )
        .map_err(|error| format!("reserve refused: {error}"))?;
        EventStore::permit_attempt_send_within(&tx, attempt, writer, self.ceiling, permit)
            .map_err(|error| format!("send permit refused: {error}"))?;
        if let Some(mail) = mail.0 {
            let event = format!("evt_{}", self.ids.mint().as_str())
                .parse::<grapt_types::EventId>()
                .map_err(|_| "mail presentation event identity invalid")?;
            let data = mail
                .presentation(attempt, step)
                .map_err(|error| error.to_string())?
                .encode();
            mail.capture_within(
                &tx,
                grapt_store::AppendInput {
                    id: &event,
                    aggregate,
                    writer,
                    kind_name: "session.mail.presented",
                    version: grapt_types::EventVersion::new(1).ok_or("event version")?,
                    created_ms: self.clock.now_ms(),
                    data: &data,
                },
                self.ceiling,
            )
            .map_err(|error| format!("mail capture refused: {error}"))?;
        }
        if let Some(mail) = mail.1 {
            mail.capture_within(
                &tx,
                attempt,
                step,
                writer,
                self.ceiling,
                self.clock.now_ms(),
            )
            .map_err(|error| format!("agent mail capture refused: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("attempt commit refused: {error}"))?
            .discard();
        Ok(())
    }
}

/// Fault injection for the admission-refusal witness: the open step refuses
/// before any byte is staged or sent. Testkit builds only.
#[cfg(feature = "loopback-testkit")]
fn open_refusal_faulted() -> Option<AttemptAdmissionRefusal> {
    std::env::var_os("GRAPT_TESTKIT_OPEN_REFUSAL").map(|_| AttemptAdmissionRefusal::OpenRefused {
        detail: "testkit open refusal".to_owned(),
    })
}

#[cfg(not(feature = "loopback-testkit"))]
fn open_refusal_faulted() -> Option<AttemptAdmissionRefusal> {
    None
}

impl<P: AttemptPlan> AttemptSource for ProviderAttemptSource<P> {
    type Reader = ProviderStreamReader;

    fn retrying(&mut self, notice: Option<crate::step::RetryNotice>) {
        self.planner.retrying(notice);
    }

    fn promote_input(&mut self, input: &grapt_types::MessageId) -> Result<(), String> {
        self.plan_state.clear();
        self.planner.promote_input(input)
    }

    fn output_continuation(&mut self, enabled: bool) -> bool {
        self.planner.output_continuation(enabled)
    }

    fn capacity_progress(&self) -> Option<tokio::sync::watch::Receiver<()>> {
        Some(self.capacity_progress.subscribe())
    }

    fn reserve_capacity(&mut self) -> crate::BoxFuture<'_, ()> {
        // Set the refusal and release a prior slot synchronously: a biased
        // deadline may discard this future before it is ever polled.
        self.capacity = grapt_provider::AttemptCapacity::default();
        self.capacity_error = Some(AttemptAdmissionRefusal::Saturated {
            detail: "the bounded provider slot wait ended before a reservation was acquired".into(),
        });
        Box::pin(async move {
            match capacity::reserve(
                &self.opener,
                self.capacity_observer.clone(),
                &self.capacity_progress,
                self.child_capacity,
            )
            .await
            {
                Ok(capacity) => {
                    self.capacity = capacity;
                    self.capacity_error = None;
                }
                Err(error) => self.capacity_error = Some(capacity::refusal(error)),
            }
        })
    }

    fn context_preparation_progress(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<crate::step::ContextPreparationStage>> {
        self.planner.context_preparation_progress()
    }

    fn prepare_context(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> crate::BoxFuture<'_, AttemptPreparation<Self::Reader>> {
        Box::pin(async move {
            if self.plan_state.pending(step, ordinal, generation) {
                return self.prepare(step, ordinal, generation);
            }
            self.plan_state.clear();
            match self.planner.refresh_context(step, generation).await {
                Ok(()) => self.prepare(step, ordinal, generation),
                Err(error) => {
                    let (failure, continuity) = match error {
                        ContextPreparationFailure::Context(failure) => (failure, None),
                        ContextPreparationFailure::Continuity(failure) => (
                            grapt_types::ContextFailure::ContinuityUnavailable,
                            Some(failure),
                        ),
                    };
                    AttemptPreparation::refused(AttemptAdmissionRefusal::ContextUnavailable {
                        failure,
                        continuity,
                    })
                }
            }
        })
    }

    fn shutdown(&mut self) -> crate::BoxFuture<'_, Result<(), String>> {
        self.plan_state.clear();
        self.planner.shutdown()
    }

    fn discard_preparation(&mut self) {
        self.plan_state.clear();
        self.capacity = grapt_provider::AttemptCapacity::default();
        self.capacity_error = None;
    }

    fn finish_context(&mut self, cancelled: bool) -> crate::BoxFuture<'_, Result<(), String>> {
        if cancelled {
            self.plan_state.clear();
        }
        // A preparation that never reached its open must not keep the
        // transport's place from every other Session.
        self.capacity = grapt_provider::AttemptCapacity::default();
        self.planner.finish_context(cancelled)
    }

    fn prepare(
        &mut self,
        step: StepNumber,
        ordinal: u32,
        generation: HistoryGeneration,
    ) -> AttemptPreparation<Self::Reader> {
        // Taken first, so a preparation refused before its open frees the
        // place at once: a compaction it asks for must not wait on it.
        let capacity = std::mem::take(&mut self.capacity);
        if let Some(refusal) = self.capacity_error.take() {
            return AttemptPreparation::refused(refusal);
        }
        // 1. The terminal handoff must exist and name THIS Step.
        let terminal = match &self.terminal {
            Some(handoff) if handoff.step() == step => {
                TerminalCoordinates::new(handoff.event().clone(), handoff.created_ms())
            }
            Some(handoff) => {
                return AttemptPreparation::refused(AttemptAdmissionRefusal::MissingStepTerminal {
                    detail: format!(
                        "the handoff names step {} but step {} is preparing",
                        handoff.step().value(),
                        step.value()
                    ),
                })
            }
            None => {
                return AttemptPreparation::refused(AttemptAdmissionRefusal::MissingStepTerminal {
                    detail: "no terminal handoff was received".to_owned(),
                })
            }
        };

        // 2. Plan once, retaining the request across transient admission waits.
        let planned = match self.plan_state.prepare(
            &mut self.planner,
            step,
            ordinal,
            generation,
            &self.compaction,
        ) {
            Ok(planned) => planned,
            Err(refusal) => return AttemptPreparation::refused(refusal),
        };

        // 3. Mint the attempt identity from the injected authority.
        let token = self.ids.mint();
        let Some(attempt) = PhysicalAttemptId::new(token.as_str()) else {
            return AttemptPreparation::refused(AttemptAdmissionRefusal::PlanRefused {
                detail: "the id authority returned an empty token".to_owned(),
            });
        };

        // 4. The sealed binding and staging. Zero bytes; a refusal here wrote
        //    nothing durable, which is why open precedes reserve — a refused
        //    open must not leave a receipt for an attempt that never existed.
        if let Some(refusal) = open_refusal_faulted() {
            return AttemptPreparation::refused(refusal);
        }
        let opened = match self.opener.open_reserved(
            attempt.clone(),
            planned.authority,
            planned.request,
            capacity,
        ) {
            Ok(opened) => opened.with_diagnostic_session(&self.session),
            Err(refusal) if refusal.is_saturated() => {
                return AttemptPreparation::refused(AttemptAdmissionRefusal::Saturated {
                    detail: refusal.to_string(),
                })
            }
            Err(refusal) => {
                return AttemptPreparation::refused(AttemptAdmissionRefusal::OpenRefused {
                    detail: refusal.to_string(),
                })
            }
        };

        // 5. Reserve the durable receipt and send permit BEFORE the stream can
        //    open. A store refusal discards the staged reader unpolled.
        let receipt_ordinal = {
            let binding = compaction::lock(&self.compaction);
            ordinal.checked_add(binding.calls)
        };
        let Some(receipt_ordinal) = receipt_ordinal else {
            return AttemptPreparation::refused(AttemptAdmissionRefusal::StoreRefused {
                detail: "physical receipt ordinal overflowed".to_owned(),
            });
        };
        if let Err(detail) = self.reserve_and_permit(
            &attempt,
            step,
            receipt_ordinal,
            &terminal,
            (planned.context.as_ref(), opened.cache_fingerprint()),
            (planned.mail.as_ref(), planned.operation_mail.as_ref()),
        ) {
            return AttemptPreparation::refused(AttemptAdmissionRefusal::StoreRefused { detail });
        }

        // 6. Custody is held from this moment until the settlement owner
        //    settles the receipt and the settling transaction commits.
        let (custody, reader) = opened.into_parts();
        self.ledger.hold(attempt.clone(), custody);
        self.planner.admitted(&attempt);
        {
            let mut binding = compaction::lock(&self.compaction);
            binding.generation = Some(generation);
            binding.preceding_ordinal = Some(ordinal);
        }

        let observer = self.error_observer.clone();
        let observed_attempt = attempt.clone();
        let unopened = UnopenedAttempt::new(attempt.clone(), move || {
            let mut reader = ProviderStreamReader::new(reader);
            reader.error_observer = observer.map(|observer| (observer, observed_attempt));
            reader
        });
        AttemptPreparation::reserved(unopened, ReservedAttemptAdmission::new(attempt))
    }

    fn begin_step(&mut self, terminal: StepTerminalHandoff) {
        self.plan_state.clear();
        *compaction::lock(&self.compaction) = CompactionBinding {
            terminal: Some(terminal.clone()),
            ..CompactionBinding::default()
        };
        self.terminal = Some(terminal);
    }

    fn receipt_settlement(&self) -> Option<ProviderReceiptSettlement> {
        Some(ProviderReceiptSettlement {
            ledger: self.ledger.clone(),
            ceiling: self.ceiling,
        })
    }
}

/// The opener's [`AttemptReader`] with exactly ONE opening marker suppressed:
/// the one at POSITION ONE. The supervisor opens the attempt itself, so a
/// first-SEEN `AttemptOpened` that is not FIRST is not the marker; suppressing
/// it would hide a malformed stream from the `AlreadyOpened` validator. The
/// decision is made ONCE, and anything but `AttemptOpened` at position one
/// means the stream never opened — which outranks whatever class it claimed.
#[allow(
    missing_debug_implementations,
    reason = "it owns the opener's reader, whose staged arm carries the authorization header"
)]
pub struct ProviderStreamReader {
    inner: AttemptReader,
    error_observer: Option<(Arc<dyn ProviderErrorObserver>, PhysicalAttemptId)>,
    opening_decided: bool,
}

impl ProviderStreamReader {
    fn new(inner: AttemptReader) -> Self {
        Self {
            inner,
            error_observer: None,
            opening_decided: false,
        }
    }
}

impl ProviderReader for ProviderStreamReader {
    async fn next(&mut self) -> Option<AttemptFragment> {
        loop {
            let fragment = self.inner.next_fragment().await;
            if let Some(detail) = self.inner.take_error_detail() {
                if let Some((observer, attempt)) = &self.error_observer {
                    observer.observe(attempt.clone(), detail).await;
                }
            }
            if self.opening_decided {
                return fragment;
            }
            self.opening_decided = true;
            match fragment {
                // `continue`, never a direct await of the next fragment: every
                // fragment after the decision must leave through the ONE
                // forward above, or a mutant of that forward is unreachable
                // for the first content fragment and its row survives green.
                Some(AttemptFragment::AttemptOpened) => continue,
                Some(_) => {
                    return Some(AttemptFragment::ProviderFailure {
                        class: ProviderFailureClass::InvalidProviderOutput,
                        classification: None,
                        diagnostic: None,
                        message:
                            "provider stream did not open: first fragment was not AttemptOpened"
                                .to_owned(),
                        state: AdapterState::none(),
                    })
                }
                None => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests;
