//! Step-scoped durable settlement for `session.step.failed.3` and
//! `session.step.ended.3`.
//!
//! The application captures one context before provider I/O and wraps the
//! Location's existing owner for exactly one [`crate::StepDriver::run`]. The
//! wrapper delegates every attempt fact unchanged and replaces both Step
//! terminal commits with the accepted event/store seams. There is no
//! post-return writer and no automatic SQLite retry.
//!
//! Until L4a only the failure half was wired: [`DurableFact::StepEnded`] fell
//! through to the in-memory inner owner, so a Step that SUCCEEDED wrote nothing
//! durable and died with the process. A Step has exactly one durable terminal
//! fact, so the two commits share one advisory slot and one captured
//! [`EventId`]; which of the two is written is the driver's classification, and
//! this seam adds no third route.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use grapt_event::session_step_ended::StepEndedFact;
use grapt_event::session_step_failed::StepFailedFact;
use grapt_event::session_step_observed::{CompletionObservedAt, ObservedStep};
use grapt_event::session_tool_terminal::ToolTerminalFact;
use grapt_inbox::Inbox;
use grapt_provider::{FinishReason, NormalizedFinish};
use grapt_store::{Appended, FencedWriter};
use grapt_types::{EventId, PhysicalAttemptId, SessionId, StepNumber, ToolCallId};

use crate::application::{Clock, EventIds};
use crate::owner::{AttemptOwner, DurableFact, Observation, OwnerError, StepFailure};

/// Coordinates captured once for one logical Step, before its first physical
/// attempt. Same-owner explicit replay reuses this value; it never mints again.
#[derive(Debug)]
pub(crate) struct StepSettlementContext {
    session: SessionId,
    step: StepNumber,
    event_id: EventId,
    created_ms: i64,
    writer: FencedWriter,
    ids: Arc<dyn EventIds>,
    clock: Arc<dyn Clock>,
}

impl StepSettlementContext {
    pub(crate) fn new(
        session: SessionId,
        step: StepNumber,
        event_id: EventId,
        created_ms: i64,
        writer: FencedWriter,
        ids: Arc<dyn EventIds>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            session,
            step,
            event_id,
            created_ms,
            writer,
            ids,
            clock,
        }
    }
}

/// Whether the committed Step terminal earned its advisory notice.
///
/// One slot, because a Step has exactly one durable terminal fact. It was named
/// for the failure half when that was the only half wired; L4a gave the success
/// half the same discipline rather than a second, differently-behaved path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepTerminalAdvisory {
    None,
    NotifyCommitted,
    SuppressIdempotent,
}

#[derive(Debug)]
struct ToolSettlementCoordinate {
    event_id: EventId,
    created_ms: i64,
}

/// The one-Step decorator installed at the existing `StepDriver` terminal
/// commit. The inner owner is returned to the active chain after the driver.
pub(crate) struct StepSettlementOwner<O> {
    inner: O,
    inbox: Arc<Mutex<Inbox>>,
    context: StepSettlementContext,
    advisory: StepTerminalAdvisory,
    terminal_position: Option<grapt_session::CommittedPosition>,
    completion_observed_at: Option<CompletionObservedAt>,
    current_attempt: Option<PhysicalAttemptId>,
    tool_coordinates: BTreeMap<(PhysicalAttemptId, ToolCallId), ToolSettlementCoordinate>,
    tool_advisories: BTreeMap<(PhysicalAttemptId, ToolCallId), StepTerminalAdvisory>,
    /// The production attempt source's receipt hook, when one is wired. Every
    /// durable attempt receipt the Step reserved settles INSIDE the terminal
    /// transaction below, and its custody is released only after that commit —
    /// "settles before release".
    receipts: Option<crate::provider_source::ProviderReceiptSettlement>,
}

impl<O: AttemptOwner> StepSettlementOwner<O> {
    pub(crate) fn new(inner: O, inbox: Arc<Mutex<Inbox>>, context: StepSettlementContext) -> Self {
        Self {
            inner,
            inbox,
            context,
            advisory: StepTerminalAdvisory::None,
            terminal_position: None,
            completion_observed_at: None,
            current_attempt: None,
            tool_coordinates: BTreeMap::new(),
            tool_advisories: BTreeMap::new(),
            receipts: None,
        }
    }

    /// Join the attempt source's receipt settlement to this Step's terminal
    /// commit. `None` — a source that reserves nothing — changes nothing.
    pub(crate) fn with_receipts(
        mut self,
        receipts: Option<crate::provider_source::ProviderReceiptSettlement>,
    ) -> Self {
        self.receipts = receipts;
        self
    }

    pub(crate) fn into_inner(self) -> O {
        self.inner
    }

    fn lock_inbox(&self) -> MutexGuard<'_, Inbox> {
        let _lock = self.inner.timing_context().map(|mut context| {
            context.scope.class = grapt_types::timing::TimingClass::Step;
            context.span(grapt_types::timing::TimingStage::StoreLock)
        });
        match self.inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    // Capture once after provider/tool processing, then retain the same observation
    // for explicit same-owner replay. Reserved event coordinates never move.
    fn completion_time(&mut self) -> Result<CompletionObservedAt, OwnerError> {
        if let Some(at) = self.completion_observed_at {
            return Ok(at);
        }
        let observed = self.context.clock.now_ms();
        if observed < self.context.created_ms {
            return Err(OwnerError {
                reason: "completion observation precedes Step reservation".into(),
            });
        }
        let at = CompletionObservedAt::new(grapt_types::Timestamp::from_millis(observed))
            .ok_or_else(|| OwnerError {
                reason: "completion observation is not a positive timestamp".into(),
            })?;
        self.completion_observed_at = Some(at);
        Ok(at)
    }

    fn commit_failure(&mut self, failure: &StepFailure) -> Result<(), OwnerError> {
        // The other boundary where the detail is in hand and the durable fact
        // does not carry it.
        crate::failure_diagnostic::step_failed(&self.context.session, self.context.step, failure);
        let fact =
            StepFailedFact::new(self.context.step, failure.reason().clone()).map_err(|error| {
                OwnerError {
                    reason: format!("StepFailedV2 codec refused: {error}"),
                }
            })?;

        let fact = ObservedStep::new(fact, self.completion_time()?);
        let appended = {
            let mut inbox = self.lock_inbox();
            let store = inbox.store_mut();
            let tx = store.governed_transaction().map_err(|error| OwnerError {
                reason: format!("StepFailedV2 transaction refused: {error}"),
            })?;
            let appended = grapt_store::step_failed::record_observed(
                &tx,
                &self.context.session,
                &self.context.event_id,
                self.context.writer,
                self.context.created_ms,
                &fact,
                &mut [],
            )
            .map_err(|error| OwnerError {
                reason: format!("StepFailedV2 append refused: {error}"),
            })?;
            // Every reserved attempt receipt settles in the SAME transaction
            // as the terminal it names — the store's atomic path. Custody is
            // released only after the commit below succeeds.
            let settled = match &self.receipts {
                Some(receipts) => receipts
                    .settle_within(&tx, self.context.writer, self.context.clock.now_ms())
                    .map_err(|error| OwnerError {
                        reason: format!("attempt receipt settlement refused: {error}"),
                    })?,
                None => Vec::new(),
            };
            let notices = tx.commit().map_err(|error| OwnerError {
                reason: format!("StepFailedV2 commit refused: {error}"),
            })?;
            // R2b-3 has no feed owner. The committed carrier therefore receives
            // its named lossy disposition here, before the existing advisory.
            notices.discard();
            if let Some(receipts) = &self.receipts {
                let _release = self
                    .inner
                    .timing_context()
                    .map(|context| context.span(grapt_types::timing::TimingStage::ReceiptRelease));
                receipts.release(&settled);
            }
            appended
        };

        self.terminal_position = Some(grapt_session::CommittedPosition {
            event: self.context.event_id.clone(),
            created_ms: self.context.created_ms,
            seq: match &appended {
                Appended::Committed { seq } | Appended::Idempotent { seq } => *seq,
            },
        });
        self.advisory = match appended {
            Appended::Committed { .. } => StepTerminalAdvisory::NotifyCommitted,
            Appended::Idempotent { .. } => StepTerminalAdvisory::SuppressIdempotent,
        };
        Ok(())
    }

    /// Commit the Step SUCCESS terminal.
    ///
    /// The durable fact carries a `ProviderCompletionDisposition`, never the
    /// raw `FinishReason`: a durable schema must not move when a provider adds
    /// a finish reason, so a new variant becomes a compile error at
    /// `FinishReason::normalize` rather than a silent change to stored bytes.
    ///
    /// A failure-shaped finish is REFUSED here rather than encoded. The attempt
    /// loop already classifies before settling, so this cannot arrive from the
    /// driver; it is kept because this is the durability boundary, and a caller
    /// that hands a `Failed`-normalizing finish to a success terminal must be
    /// told, not quietly obeyed. It is the second half of one rule stated at
    /// both ends, not a duplicate of the first.
    fn commit_ended(&mut self, finish: FinishReason) -> Result<(), OwnerError> {
        let completion = match finish.normalize() {
            NormalizedFinish::Completed { completion } => completion,
            NormalizedFinish::Failed { class } => {
                return Err(OwnerError {
                    reason: format!(
                        "StepEndedV2 refused: finish {finish:?} normalizes to a provider failure ({class:?}), which is not a success terminal"
                    ),
                })
            }
        };
        let fact = StepEndedFact::new(self.context.step, completion);

        let fact = ObservedStep::new(fact, self.completion_time()?);
        let appended = {
            let mut inbox = self.lock_inbox();
            let store = inbox.store_mut();
            let tx = store.governed_transaction().map_err(|error| OwnerError {
                reason: format!("StepEndedV2 transaction refused: {error}"),
            })?;
            let appended = grapt_store::step_ended::record_observed(
                &tx,
                &self.context.session,
                &self.context.event_id,
                self.context.writer,
                self.context.created_ms,
                &fact,
                &mut [],
            )
            .map_err(|error| OwnerError {
                reason: format!("StepEndedV2 append refused: {error}"),
            })?;
            // Same discipline as the failure half: receipts settle in the
            // terminal's own transaction, custody releases after its commit.
            let settled = match &self.receipts {
                Some(receipts) => receipts
                    .settle_within(&tx, self.context.writer, self.context.clock.now_ms())
                    .map_err(|error| OwnerError {
                        reason: format!("attempt receipt settlement refused: {error}"),
                    })?,
                None => Vec::new(),
            };
            let notices = tx.commit().map_err(|error| OwnerError {
                reason: format!("StepEndedV2 commit refused: {error}"),
            })?;
            // Same named lossy disposition as the failure half: there is no
            // feed owner at this base, and the committed carrier is disposed of
            // here rather than escaping the transaction.
            notices.discard();
            if let Some(receipts) = &self.receipts {
                let _release = self
                    .inner
                    .timing_context()
                    .map(|context| context.span(grapt_types::timing::TimingStage::ReceiptRelease));
                receipts.release(&settled);
            }
            appended
        };

        self.terminal_position = Some(grapt_session::CommittedPosition {
            event: self.context.event_id.clone(),
            created_ms: self.context.created_ms,
            seq: match &appended {
                Appended::Committed { seq } | Appended::Idempotent { seq } => *seq,
            },
        });
        self.advisory = match appended {
            Appended::Committed { .. } => StepTerminalAdvisory::NotifyCommitted,
            Appended::Idempotent { .. } => StepTerminalAdvisory::SuppressIdempotent,
        };
        Ok(())
    }

    fn commit_tool_terminal(
        &mut self,
        call: &ToolCallId,
        outcome: &grapt_types::ToolTerminalOutcome,
    ) -> Result<(), OwnerError> {
        let attempt = self.current_attempt.clone().ok_or_else(|| OwnerError {
            reason: "ToolTerminal has no active PhysicalAttempt".to_owned(),
        })?;
        let key = (attempt.clone(), call.clone());
        if !self.tool_coordinates.contains_key(&key) {
            self.tool_coordinates.insert(
                key.clone(),
                ToolSettlementCoordinate {
                    event_id: self.context.ids.mint(),
                    created_ms: self.context.clock.now_ms(),
                },
            );
        }
        let coordinate = self.tool_coordinates.get(&key).ok_or_else(|| OwnerError {
            reason: "ToolTerminal coordinate allocation failed".to_owned(),
        })?;
        let fact = ToolTerminalFact::new(attempt, call.clone(), outcome.clone());

        let appended = {
            let mut inbox = self.lock_inbox();
            let store = inbox.store_mut();
            let tx = store.governed_transaction().map_err(|error| OwnerError {
                reason: format!("ToolTerminalV1 transaction refused: {error}"),
            })?;
            let appended = grapt_store::tool_terminal::record(
                &tx,
                &self.context.session,
                &coordinate.event_id,
                self.context.writer,
                coordinate.created_ms,
                &fact,
                &mut [],
            )
            .map_err(|error| OwnerError {
                reason: format!("ToolTerminalV1 append refused: {error}"),
            })?;
            tx.commit()
                .map_err(|error| OwnerError {
                    reason: format!("ToolTerminalV1 commit refused: {error}"),
                })?
                .discard();
            appended
        };
        let advisory = match appended {
            Appended::Committed { .. } => StepTerminalAdvisory::NotifyCommitted,
            Appended::Idempotent { .. } => StepTerminalAdvisory::SuppressIdempotent,
        };
        self.tool_advisories.insert(key, advisory);
        Ok(())
    }
}

impl<O: AttemptOwner> AttemptOwner for StepSettlementOwner<O> {
    fn committed_output(&self) -> bool {
        self.inner.committed_output()
    }

    fn capture_hosted_output(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        self.inner.capture_hosted_output(text)
    }

    fn capture_diagnostic_detail(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        self.inner.capture_diagnostic_detail(text)
    }

    fn timing_context(&self) -> Option<grapt_types::timing::TimingContext> {
        self.inner.timing_context()
    }

    fn commit(&mut self, fact: DurableFact) -> Result<(), OwnerError> {
        if matches!(
            fact,
            DurableFact::StepEnded { .. } | DurableFact::StepFailed { .. }
        ) {
            if let Err(error) = self.inner.prepare_step_terminal() {
                // Lost capture is a durability failure, not a successful or
                // silently shortened response. Still settle if the store can
                // commit; the caller receives OwnerRejected and cannot retry
                // the already settled Step as a new external effect.
                let failure = StepFailure::new(
                    grapt_types::StepTerminalReason::DurabilityFailure,
                    grapt_provider::BoundedDetail::from(error.reason.as_str()),
                );
                self.commit_failure(&failure)
                    .map_err(|terminal_error| OwnerError {
                        reason: format!(
                            "{}; terminal settlement also failed: {}",
                            error.reason, terminal_error.reason
                        ),
                    })?;
                self.notify(&DurableFact::StepFailed { failure });
                return Err(error);
            }
        }
        match &fact {
            DurableFact::StepFailed { failure } => self.commit_failure(failure),
            DurableFact::StepEnded { finish } => self.commit_ended(*finish),
            DurableFact::AttemptStarted { attempt } => {
                let attempt = attempt.clone();
                self.inner.commit(fact)?;
                self.current_attempt = Some(attempt);
                Ok(())
            }
            DurableFact::ToolTerminal { call, outcome } => self.commit_tool_terminal(call, outcome),
            // Delegated unchanged, and enumerated rather than swept.
            //
            // A wildcard here was correct for every fact that exists and wrong
            // for the first one that needs settlement: a new variant would fall
            // through, be committed as an ordinary attempt fact, and leave no
            // translation, no store write and no advisory — with the gate still
            // green, because nothing fails. That is the shape a governed receipt
            // has, and the lane that adds one is next.
            //
            // Enumerating costs nothing today and makes the next variant
            // unaddable without a decision. It changes no behaviour: these four
            // are exactly what the wildcard matched.
            DurableFact::Fragment { .. }
            | DurableFact::NestedToolCalled { .. }
            | DurableFact::AttemptEnded { .. }
            | DurableFact::AttemptFailed { .. } => self.inner.commit(fact),
        }
    }

    fn notify(&mut self, fact: &DurableFact) {
        if let DurableFact::ToolTerminal { call, .. } = fact {
            let advisory = self
                .current_attempt
                .as_ref()
                .map(|attempt| (attempt.clone(), call.clone()))
                .and_then(|key| self.tool_advisories.remove(&key));
            match advisory {
                Some(StepTerminalAdvisory::NotifyCommitted) => {
                    self.inner.tool_terminal_committed(fact)
                }
                Some(StepTerminalAdvisory::SuppressIdempotent)
                | Some(StepTerminalAdvisory::None)
                | None => {}
            }
        } else if matches!(
            fact,
            DurableFact::StepFailed { .. } | DurableFact::StepEnded { .. }
        ) {
            match self.advisory {
                StepTerminalAdvisory::NotifyCommitted
                | StepTerminalAdvisory::SuppressIdempotent => {
                    if let Some(position) = self.terminal_position.take() {
                        let publication = match self.advisory {
                            StepTerminalAdvisory::NotifyCommitted => {
                                crate::owner::TerminalPublication::Notify
                            }
                            StepTerminalAdvisory::SuppressIdempotent => {
                                crate::owner::TerminalPublication::SuppressReplay
                            }
                            StepTerminalAdvisory::None => {
                                unreachable!("matched a committed disposition")
                            }
                        };
                        self.inner
                            .step_terminal_committed(fact, &position, publication);
                    }
                }
                StepTerminalAdvisory::None => {}
            }
            self.advisory = StepTerminalAdvisory::None;
        } else {
            // Exhaustive, and it has to be. An `else` here is the wildcard
            // wearing different syntax, and a `debug_assert` is no better: both
            // let a new variant reach `self.inner.notify` and be notified as an
            // ordinary attempt fact, silently.
            //
            // That is WORSE than the plain wildcard it replaced, because a lane
            // adding a variant is stopped by `commit` above, fixes it there, and
            // has every reason to conclude this seam is closed. One site guarded
            // and one site open is the shape that gets missed.
            //
            // The two arms handled by the branches above are named `unreachable!`
            // rather than folded in, so this match keeps its meaning: those two
            // are settled elsewhere, and the other five are delegated unchanged.
            match fact {
                DurableFact::AttemptStarted { .. }
                | DurableFact::Fragment { .. }
                | DurableFact::NestedToolCalled { .. }
                | DurableFact::AttemptEnded { .. }
                | DurableFact::AttemptFailed { .. } => self.inner.notify(fact),
                DurableFact::ToolTerminal { .. }
                | DurableFact::StepFailed { .. }
                | DurableFact::StepEnded { .. } => {
                    unreachable!("settled by the branches above this one")
                }
            }
        }
    }

    fn observe(&mut self, observation: Observation) {
        self.inner.observe(observation);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::owner::{RecordingOwner, StepFailure};
    use grapt_store::{AggregateId, EventStore};
    use grapt_types::{InterruptionSource, PolicyVersion, StepTerminalReason, ToolTerminalOutcome};

    #[derive(Debug)]
    struct TestIds;

    impl EventIds for TestIds {
        fn mint(&self) -> EventId {
            "evt_r2_tool_terminal"
                .parse()
                .unwrap_or_else(|error| panic!("test event id must parse: {error}"))
        }
    }

    #[derive(Debug)]
    struct TestClock;

    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            43
        }
    }

    #[derive(Debug)]
    struct SequenceIds(AtomicUsize);

    impl EventIds for SequenceIds {
        fn mint(&self) -> EventId {
            let ordinal = self.0.fetch_add(1, Ordering::SeqCst);
            match format!("evt_r2_tool_terminal_{ordinal}").parse() {
                Ok(id) => id,
                Err(error) => panic!("test event id must parse: {error}"),
            }
        }
    }

    fn lock(inbox: &Mutex<Inbox>) -> MutexGuard<'_, Inbox> {
        match inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Harness for one Step terminal: an elected in-memory store, a captured
    /// context, and the wrapped owner the driver would use.
    fn settled(
        label: &str,
    ) -> (
        SessionId,
        Arc<Mutex<Inbox>>,
        StepSettlementOwner<RecordingOwner>,
    ) {
        let session: SessionId = match label.parse() {
            Ok(session) => session,
            Err(error) => panic!("session id must parse: {error}"),
        };
        let aggregate = AggregateId::Session(session.clone());
        let mut store = match EventStore::open_in_memory() {
            Ok(store) => store,
            Err(error) => panic!("in-memory store must open: {error}"),
        };
        if let Err(error) = store.elect_writer(&aggregate) {
            panic!("test owner must elect once upstream: {error}");
        }
        let writer = match store.fencing_token(&aggregate) {
            Ok(writer) => writer,
            Err(error) => panic!("the elected writer token must exist: {error}"),
        };
        let inbox = match Inbox::new(store) {
            Ok(inbox) => Arc::new(Mutex::new(inbox)),
            Err(error) => panic!("inbox must initialise: {error}"),
        };
        let event_id: EventId = match "evt_l4a_step_ended".parse() {
            Ok(event_id) => event_id,
            Err(error) => panic!("event id must parse: {error}"),
        };
        let step = match StepNumber::new(1) {
            Some(step) => step,
            None => panic!("Step 1 must be positive"),
        };
        let context = StepSettlementContext::new(
            session.clone(),
            step,
            event_id,
            42,
            writer,
            Arc::new(TestIds),
            Arc::new(TestClock),
        );
        let owner = StepSettlementOwner::new(RecordingOwner::new(), Arc::clone(&inbox), context);
        (session, inbox, owner)
    }

    fn rows(inbox: &Mutex<Inbox>, session: &SessionId, kind: &str) -> i64 {
        let guard = lock(inbox);
        let sql = format!("SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = '{kind}'");
        match guard
            .store()
            .connection()
            .query_row(&sql, [session.as_str()], |row| row.get(0))
        {
            Ok(rows) => rows,
            Err(error) => panic!("durable {kind} count must read: {error}"),
        }
    }

    /// THE LANE. A Step that SUCCEEDS now writes a durable terminal.
    ///
    /// Before L4a this fact fell through to the in-memory inner owner and died
    /// with the process, which is why nothing was red: the assertion that would
    /// have failed did not exist. The inner owner is asserted EMPTY as well as
    /// the row being present — a dispatch that wrote durably AND delegated would
    /// leave two carriers of one terminal.
    #[test]
    fn a_successful_step_writes_a_durable_success_terminal() {
        let (session, inbox, mut owner) = settled("ses_l4a_ended");
        let fact = DurableFact::StepEnded {
            finish: FinishReason::Stop,
        };
        if let Err(error) = owner.commit(fact.clone()) {
            panic!("the success terminal must commit: {error}");
        }
        owner.notify(&fact);

        assert_eq!(
            rows(&inbox, &session, "session.step.ended.3"),
            1,
            "a successful Step writes exactly one durable terminal"
        );
        assert_eq!(
            rows(&inbox, &session, "session.step.ended.1"),
            0,
            "the frozen oracle v1 kind is never written"
        );
        let inner = owner.into_inner();
        assert!(
            inner.committed.is_empty(),
            "the provisional in-memory StepEnded writer is replaced, not doubled"
        );
        assert_eq!(inner.notified.len(), 1, "a committed terminal is notified");
    }

    /// The disposition reaches the durable bytes, not the raw finish.
    ///
    /// `ToolCalls` is the interesting one: it is a perfectly ordinary success
    /// that means "continue", and a codec keyed on the raw finish would store a
    /// provider word instead of the normalized disposition.
    #[test]
    fn the_durable_bytes_carry_the_normalized_disposition() {
        let (session, inbox, mut owner) = settled("ses_l4a_disposition");
        let fact = DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        };
        if let Err(error) = owner.commit(fact.clone()) {
            panic!("the success terminal must commit: {error}");
        }
        let data: String = {
            let guard = lock(&inbox);
            match guard.store().connection().query_row(
                "SELECT data FROM event WHERE aggregate_id = ?1",
                [session.as_str()],
                |row| row.get(0),
            ) {
                Ok(data) => data,
                Err(error) => panic!("the durable row must read: {error}"),
            }
        };
        assert!(
            data.contains("asserted-continue"),
            "the normalized disposition is what is stored: {data}"
        );
        assert!(
            !data.contains("tool-calls") && !data.contains("ToolCalls"),
            "the raw provider finish never reaches durable bytes: {data}"
        );
    }

    /// A failure-shaped finish is refused at the durability boundary.
    ///
    /// The attempt loop classifies before settling, so this cannot arrive from
    /// the driver. It is witnessed because this is the boundary that must not be
    /// talked into writing a success terminal for a content-policy refusal, and
    /// because a rule stated at one end only is a rule with one place left to
    /// get it wrong.
    #[test]
    fn a_failure_shaped_finish_is_refused_rather_than_written_as_a_success() {
        for finish in [FinishReason::ContentFilter, FinishReason::Error] {
            let (session, inbox, mut owner) = settled("ses_l4a_refusal");
            let error = match owner.commit(DurableFact::StepEnded { finish }) {
                Err(error) => error,
                Ok(()) => panic!("{finish:?} must not commit as a success terminal"),
            };
            assert!(
                error.reason.contains("not a success terminal"),
                "the refusal must name what it refused: {}",
                error.reason
            );
            assert_eq!(
                rows(&inbox, &session, "session.step.ended.3"),
                0,
                "a refused terminal writes nothing"
            );
        }
    }

    /// Same-owner replay reconciles and emits no duplicate advisory.
    ///
    /// The success half gets the same discipline as the failure half rather
    /// than a second, differently-behaved path.
    #[test]
    fn same_owner_replay_of_the_success_terminal_is_idempotent() {
        let (session, inbox, mut owner) = settled("ses_l4a_replay");
        let fact = DurableFact::StepEnded {
            finish: FinishReason::Stop,
        };
        for _ in 0..2 {
            if let Err(error) = owner.commit(fact.clone()) {
                panic!("same-owner replay must reconcile: {error}");
            }
            owner.notify(&fact);
        }
        assert_eq!(
            rows(&inbox, &session, "session.step.ended.3"),
            1,
            "exact replay leaves one row and one sequence"
        );
        assert_eq!(
            owner.into_inner().notified.len(),
            1,
            "Idempotent emits no duplicate advisory"
        );
    }

    #[test]
    fn same_owner_replay_is_idempotent_and_suppresses_duplicate_advisory() {
        let session: SessionId = match "ses_r2b3_idempotent".parse() {
            Ok(session) => session,
            Err(error) => panic!("session id must parse: {error}"),
        };
        let aggregate = AggregateId::Session(session.clone());
        let mut store = match EventStore::open_in_memory() {
            Ok(store) => store,
            Err(error) => panic!("in-memory store must open: {error}"),
        };
        if let Err(error) = store.elect_writer(&aggregate) {
            panic!("test owner must elect once upstream: {error}");
        }
        let writer = match store.fencing_token(&aggregate) {
            Ok(writer) => writer,
            Err(error) => panic!("the elected writer token must exist: {error}"),
        };
        let inbox = match Inbox::new(store) {
            Ok(inbox) => Arc::new(Mutex::new(inbox)),
            Err(error) => panic!("inbox must initialise: {error}"),
        };
        let event_id: EventId = match "evt_r2b3_idempotent".parse() {
            Ok(event_id) => event_id,
            Err(error) => panic!("event id must parse: {error}"),
        };
        let step = match StepNumber::new(1) {
            Some(step) => step,
            None => panic!("Step 1 must be positive"),
        };
        let policy = match PolicyVersion::new("r2b3-idempotent/1") {
            Some(policy) => policy,
            None => panic!("policy version must be non-empty"),
        };
        let failure = StepFailure::new(
            StepTerminalReason::PolicyLimit { policy },
            "attempts exhausted",
        );
        let fact = DurableFact::StepFailed { failure };
        let context = StepSettlementContext::new(
            session.clone(),
            step,
            event_id,
            42,
            writer,
            Arc::new(TestIds),
            Arc::new(TestClock),
        );
        let mut owner =
            StepSettlementOwner::new(RecordingOwner::new(), Arc::clone(&inbox), context);

        for _ in 0..2 {
            if let Err(error) = owner.commit(fact.clone()) {
                panic!("same-owner replay must reconcile: {error}");
            }
            owner.notify(&fact);
        }

        let inner = owner.into_inner();
        assert!(
            inner.committed.is_empty(),
            "the provisional inner StepFailed writer is deleted"
        );
        assert_eq!(
            inner.notified.len(),
            1,
            "Idempotent emits no duplicate existing advisory"
        );
        let inbox_guard = lock(&inbox);
        let rows: i64 = match inbox_guard.store().connection().query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = 'session.step.failed.3'",
            [session.as_str()],
            |row| row.get(0),
        ) {
            Ok(rows) => rows,
            Err(error) => panic!("durable Step failure count must read: {error}"),
        };
        assert_eq!(rows, 1, "exact replay leaves one row and one sequence");
    }

    #[test]
    fn reused_call_token_is_scoped_by_physical_attempt_and_same_pair_is_idempotent() {
        let session: SessionId = match "ses_r2_tool_identity".parse() {
            Ok(session) => session,
            Err(error) => panic!("session id must parse: {error}"),
        };
        let aggregate = AggregateId::Session(session.clone());
        let mut store = match EventStore::open_in_memory() {
            Ok(store) => store,
            Err(error) => panic!("in-memory store must open: {error}"),
        };
        if let Err(error) = store.elect_writer(&aggregate) {
            panic!("test owner must elect once upstream: {error}");
        }
        let writer = match store.fencing_token(&aggregate) {
            Ok(writer) => writer,
            Err(error) => panic!("the elected writer token must exist: {error}"),
        };
        let inbox = match Inbox::new(store) {
            Ok(inbox) => Arc::new(Mutex::new(inbox)),
            Err(error) => panic!("inbox must initialise: {error}"),
        };
        let step = match StepNumber::new(1) {
            Some(step) => step,
            None => panic!("Step 1 must be positive"),
        };
        let context = StepSettlementContext::new(
            session.clone(),
            step,
            match "evt_r2_tool_identity_step".parse() {
                Ok(event_id) => event_id,
                Err(error) => panic!("event id must parse: {error}"),
            },
            42,
            writer,
            Arc::new(SequenceIds(AtomicUsize::new(0))),
            Arc::new(TestClock),
        );
        let mut owner =
            StepSettlementOwner::new(RecordingOwner::new(), Arc::clone(&inbox), context);
        let call = match ToolCallId::new("call_reused_by_provider") {
            Some(call) => call,
            None => panic!("static call id must be valid"),
        };
        let outcome = ToolTerminalOutcome::Cancelled {
            by: InterruptionSource::User,
        };

        for attempt_name in ["att_identity_a", "att_identity_b"] {
            let attempt = match PhysicalAttemptId::new(attempt_name) {
                Some(attempt) => attempt,
                None => panic!("static attempt id must be valid"),
            };
            let started = DurableFact::AttemptStarted { attempt };
            if let Err(error) = owner.commit(started.clone()) {
                panic!("attempt start must commit: {error}");
            }
            owner.notify(&started);

            let terminal = DurableFact::ToolTerminal {
                call: call.clone(),
                outcome: outcome.clone(),
            };
            if let Err(error) = owner.commit(terminal.clone()) {
                panic!("first terminal for the pair must commit: {error}");
            }
            owner.notify(&terminal);
            if attempt_name == "att_identity_a" {
                if let Err(error) = owner.commit(terminal.clone()) {
                    panic!("same pair retry must reconcile idempotently: {error}");
                }
                owner.notify(&terminal);
            }
        }

        let inner = owner.into_inner();
        assert_eq!(
            inner
                .notified
                .iter()
                .filter(|fact| matches!(fact, DurableFact::ToolTerminal { .. }))
                .count(),
            2,
            "same pair suppresses its retry while the new attempt projects once"
        );
        let inbox_guard = lock(&inbox);
        let rows: i64 = match inbox_guard.store().connection().query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = 'session.tool.terminal.1'",
            [session.as_str()],
            |row| row.get(0),
        ) {
            Ok(rows) => rows,
            Err(error) => panic!("durable tool-terminal count must read: {error}"),
        };
        assert_eq!(
            rows, 2,
            "the same call token under two Physical Attempts has two stable identities"
        );
    }
}
